//! The loopback HTTP API (D-0007). Wire types come from `lore-core` and are
//! never redefined here.
//!
//! Shape rules:
//! - Everything lives under `/v1`; there is no unversioned surface, so a
//!   future `/v2` can coexist rather than break thin clients mid-upgrade.
//! - Every non-2xx response — including extractor rejections and unknown
//!   routes — is a JSON [`lore_core::ApiError`]. A client that has to
//!   distinguish "my JSON was malformed" (plain text, by default in axum)
//!   from "your project does not exist" (JSON) is a client that will parse
//!   error bodies wrong.
//! - Handlers never touch the store directly: they go through
//!   [`StoreHandle::with`], which puts every blocking call on the blocking
//!   pool.
//!
//! # Scoping
//!
//! Every *query* is scoped to exactly one project: `search` and `expand`
//! reject a request that names none, with an error that says how to name one
//! (`design/4_Interfaces/2026-08-16_project-scoping-decision-brief.md`,
//! "Resolution"). `GET /v1/status?project=` narrows the same way.
//!
//! **Bare `GET /v1/status` deliberately stays machine-wide.** It is the
//! local-admin surface — what `lore status` prints for the person who owns the
//! daemon — not the answer a scoped client gets. `lore-mcp` therefore always
//! passes `?project=`, so an agent enumerates only its own project. When the
//! daemon stops being loopback-only, this is the view that needs a capability
//! boundary rather than a filter.
//!
//! No authentication: the listener binds `127.0.0.1` only, so reaching it
//! already requires local code execution.
// TODO(hardening): a bearer secret written into daemon.json (readable only by
// the owning user) would additionally stop *other local users* and untrusted
// local processes from reading the index. Deferred out of M1 deliberately —
// it needs a decision about how MCP clients receive the secret.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use camino::{Utf8Path, Utf8PathBuf};

use lore_core::{
    DaemonStatus, ExpandRequest, IndexRequest, IndexResponse, ProjectInfo, ProjectList,
    RegisterProjectRequest, RemoveProjectResponse, SearchRequest,
};

use crate::config::Config;
use crate::embed::Embedder;
use crate::store::Project;

use super::queue::IndexQueue;
use super::store_handle::StoreHandle;
use super::watch::{WatchCommand, WatchSender, WatchStatus};
use super::{expand, ignorefile, paths, search};

/// Request bodies are small JSON documents; a megabyte is generous for the
/// largest realistic one (a pasted query) and cheap insurance otherwise.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub store: StoreHandle,
    pub queue: IndexQueue,
    pub watch: WatchSender,
    /// Live per-project watcher coverage, written by the watcher pump.
    pub watch_status: WatchStatus,
    pub config: Arc<Config>,
    /// Live embedding capability and health. Shared with the embed worker,
    /// which is the thing that actually probes the endpoint.
    pub embeddings: Embedder,
    /// Query-surface latency windows, reported by `status`.
    pub latency: crate::daemon::latency::LatencyRecorder,
    pub data_dir: Utf8PathBuf,
    /// The daemon's shutdown token — the same one the workers and the server's
    /// own graceful-shutdown future watch. `POST /v1/shutdown` cancels it;
    /// nothing else here reads it.
    pub shutdown: tokio_util::sync::CancellationToken,
}

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/status", get(status))
        .route("/projects", get(list_projects).post(register_project))
        .route("/projects/{project}", delete(remove_project))
        .route("/resolve", get(resolve))
        .route("/index", post(index))
        .route("/shutdown", post(shutdown))
        .route("/search", post(search_route))
        .route("/expand", post(expand_route))
        .with_state(state);

    Router::new()
        .nest("/v1", v1)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiErr {
    status: StatusCode,
    message: String,
}

impl ApiErr {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
}

/// Appended to every "which project?" refusal. Errors in this codebase name
/// the remedy, and for scoping there are exactly two: look one up, or enroll
/// one.
const NAME_A_PROJECT: &str =
    "name a registered project (see `lore status`), or register one with `lore add <path>`";

impl ApiErr {
    /// Internal failures are logged with their full chain and reported with a
    /// short message: the client can only retry, and the detail belongs in
    /// the daemon's log where it is correlated with everything else.
    pub fn internal(context: &str, error: impl std::fmt::Display) -> Self {
        tracing::error!(context, error = %error, "request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{context} failed; see daemon log"),
        )
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(lore_core::ApiError {
                message: self.message,
            }),
        )
            .into_response()
    }
}

async fn not_found() -> ApiErr {
    ApiErr::not_found("no such endpoint; this daemon speaks /v1 only")
}

/// `Json`, but its rejections are [`ApiErr`] too.
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    Json<T>: FromRequest<S, Rejection = axum::extract::rejection::JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiErr;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => Err(ApiErr::new(rejection.status(), rejection.body_text())),
        }
    }
}

type ApiResult<T> = Result<Json<T>, ApiErr>;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /v1/status?project=<name-or-key>` — scope the report to one project.
///
/// With the filter the `projects` list holds that project alone and the
/// latency list gains its per-corpus store-scan window; an unknown name is a
/// 404 rather than an empty list, because "no such project" and "that project
/// has nothing indexed" are different answers.
///
/// Without it the report stays machine-wide: see the module header on why the
/// unscoped view is the local-admin surface rather than the default a scoped
/// client should get.
#[derive(Debug, serde::Deserialize)]
struct StatusQuery {
    project: Option<String>,
}

async fn status(
    State(state): State<AppState>,
    Query(query): Query<StatusQuery>,
) -> ApiResult<DaemonStatus> {
    let status = state
        .store
        .with(|store| store.status())
        .await
        .map_err(|err| ApiErr::internal("status", err))?
        .map_err(|err| ApiErr::internal("status", err))?;

    let mut projects: Vec<lore_core::ProjectStatus> = status
        .projects
        .into_iter()
        .map(|p| lore_core::ProjectStatus {
            id: p.project,
            name: p.name,
            key: p.key,
            root: p.root.into_string(),
            kind: p.kind.as_str().to_string(),
            files: p.files,
            chunks: p.chunks,
            embedded_chunks: p.embedded_chunks,
            authority_violations: p.authority_violations,
            authority_violation_paths: p
                .authority_violation_paths
                .into_iter()
                .map(camino::Utf8PathBuf::into_string)
                .collect(),
            // The repo's own `.lore.toml` verdict (D-0012). The behavior
            // is reported only alongside a profile: on its own it would
            // claim a mode for a repo that declared nothing.
            authority_profile: p
                .authority
                .profile
                .map(|profile| profile.as_str().to_string()),
            authority_behavior: p
                .authority
                .profile
                .map(|_| p.authority.behavior.as_str().to_string()),
            authority_config_error: p.authority.error,
            decisions_active: p.decisions_active,
            decisions_total: p.decisions_total,
            decision_violations: p
                .decision_violations
                .iter()
                .map(ToString::to_string)
                .collect(),
            // Same reasoning as `embeddings` below: a watch that is not
            // armed degrades the daemon silently unless it is reported.
            watch: state.watch_status.of(p.project),
        })
        .collect();

    // Scoping the report, when asked. Identity is matched the same three ways
    // everything else accepts a project (name, key, id), so a caller holding
    // any of them can narrow without first translating it.
    if let Some(wanted) = query.project.as_deref() {
        projects.retain(|p| p.name == wanted || p.key == wanted || p.id.to_string() == wanted);
        if projects.is_empty() {
            return Err(ApiErr::not_found(format!(
                "unknown project `{wanted}`; {NAME_A_PROJECT}"
            )));
        }
    }

    Ok(Json(DaemonStatus {
        api_version: lore_core::API_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        generation: status.generation,
        // Per-corpus latency rows are labeled `endpoint:<whatever the request
        // said>`, which may be a name or a key; matching the scoped project's
        // own identifiers keeps the row visible either way.
        latency: {
            let identifiers: Vec<&str> = projects
                .iter()
                .flat_map(|p| [p.name.as_str(), p.key.as_str()])
                .collect();
            let mut latency = state.latency.snapshot();
            latency.retain(|l| match l.endpoint.split_once(':') {
                None => true, // global endpoints always
                Some((_, project)) => query.project.is_some() && identifiers.contains(&project),
            });
            latency
        },
        projects,
        // Live probe result, not a guess derived from the config file: the
        // whole point of D-0007 is that a user can see *why* results are
        // lexical-only.
        embeddings: state.embeddings.status(),
        // Machine-wide even under `?project=`: the worker's poison set is not
        // per project, and attributing its count to whichever project was
        // asked about would be a fabrication.
        embed_abandoned: state.embeddings.abandoned_chunks(),
    }))
}

async fn list_projects(State(state): State<AppState>) -> ApiResult<ProjectList> {
    let projects = projects_of(&state).await?;
    Ok(Json(ProjectList {
        projects: projects.iter().map(info).collect(),
    }))
}

/// Register (or rename) a project root, then immediately watch and scan it.
///
/// The root is canonicalized here because the store deliberately does not:
/// registering `.\lexomancy` and `C:\repos\Lexomancy` as two projects would
/// index everything twice and give the watcher two identities for one tree.
async fn register_project(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RegisterProjectRequest>,
) -> ApiResult<ProjectInfo> {
    let root = paths::canonicalize_root(&request.root)
        .map_err(|err| ApiErr::bad_request(format!("{err:#}")))?;
    if !root.is_dir() {
        return Err(ApiErr::bad_request(format!(
            "project root is not a directory: {root}"
        )));
    }
    if paths::is_within(&state.data_dir, &root) {
        return Err(ApiErr::bad_request(format!(
            "refusing to index the daemon's own data directory: {root}"
        )));
    }

    let name = match request.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => root
            .file_name()
            .ok_or_else(|| ApiErr::bad_request(format!("cannot derive a name from {root}")))?
            .to_string(),
    };

    // Display names are how humans (and the CLI, and older wire clients) name
    // a project, so two projects sharing one is not a cosmetic problem — it
    // makes `expand(project, chunk_id)` resolve the wrong source or 404
    // (S1#3). Reject at the door rather than silently accepting an ambiguous
    // registry; the caller has a one-flag fix.
    //
    // 409 rather than 400: the request is well-formed and the name is legal —
    // it is the *registry's current state* that refuses it, and that is a
    // state the caller can change. Re-adding the same root under the same name
    // is not a collision with itself; it is the idempotent rename path below.
    let existing = projects_of(&state).await?;
    if crate::registry::names_taken_by_others(&existing, &root).contains(&name) {
        let other = existing
            .iter()
            .find(|project| project.name == name)
            .map(|project| project.root.to_string())
            .unwrap_or_default();
        return Err(ApiErr::conflict(format!(
            "a project named `{name}` is already registered at {other}, so {root} cannot \
             also claim it; choose another name with `--name <name>`, or edit that repo's \
             .lore.toml to rename it"
        )));
    }

    let registered = {
        let root = root.clone();
        let name = name.clone();
        state
            .store
            .with(move |store| store.register_project(&root, &name))
            .await
            .map_err(|err| ApiErr::internal("register", err))?
            .map_err(|err| ApiErr::internal("register", err))?
    };

    // The manifest is authoritative, so it is republished from the store the
    // moment the store changes. A failure here does not fail the
    // registration — the project *is* registered — but it means the next
    // startup would drop it, which is exactly the kind of thing that must not
    // be silent.
    {
        let data_dir = state.data_dir.clone();
        match state
            .store
            .with(move |store| crate::registry::publish(store, &data_dir))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) | Err(err) => tracing::error!(
                error = %format!("{err:#}"),
                "project registered but the registry manifest could not be written; \
                 it will not survive a restart"
            ),
        }
    }

    let projects = projects_of(&state).await?;
    let project = projects
        .into_iter()
        .find(|project| project.id == registered)
        .ok_or_else(|| ApiErr::internal("register", "the registered project vanished"))?;

    tracing::info!(
        project = %project.name,
        key = %project.key,
        root = %project.root,
        id = project.id,
        "project registered"
    );

    // The exclusion policy is written the moment the project is enrolled, so
    // it exists before the first scan and before the user goes looking for
    // it. `spawn_blocking` because detection reads directories, and the first
    // scan would only redo this work anyway.
    {
        let root = project.root.clone();
        let _ = tokio::task::spawn_blocking(move || ignorefile::ensure(&root)).await;
    }

    let _ = state.watch.send(WatchCommand::Watch(project.clone()));
    state.queue.request_full(project.id);

    Ok(Json(info(&project)))
}

/// `DELETE /v1/projects/{name-or-key}` — deregister a project and forget its
/// index.
///
/// The four things that make a project "registered" are undone in an order
/// that cannot leave a half-removed one behind on any single failure:
/// the manifest is authoritative, so it is republished from the store the
/// moment the store changes, and the watch is dropped last because a watcher
/// still armed on a forgotten root only costs a wasted rescan request, while a
/// manifest still naming it would resurrect the project on the next start.
async fn remove_project(
    State(state): State<AppState>,
    Path(wanted): Path<String>,
) -> ApiResult<RemoveProjectResponse> {
    let projects = projects_of(&state).await?;
    // Name first, then key — the same precedence `resolve_project` documents,
    // extended to keys because a caller who has only a key (an agent replaying
    // a search result, a script) should not have to translate it first.
    let project = super::resolve_project(&projects, &wanted)
        .or_else(|| super::resolve_project_key(&projects, &wanted))
        .ok_or_else(|| ApiErr::not_found(format!("unknown project `{wanted}`; {NAME_A_PROJECT}")))?
        .clone();

    // Counted before the delete: afterwards there is nothing left to count,
    // and "removed 0 chunks" would misreport every removal.
    let counted = state
        .store
        .with(|store| store.status())
        .await
        .map_err(|err| ApiErr::internal("remove project", err))?
        .map_err(|err| ApiErr::internal("remove project", err))?
        .projects
        .into_iter()
        .find(|p| p.project == project.id);
    let (files, chunks) = counted.map_or((0, 0), |p| (p.files, p.chunks));

    let id = project.id;
    let removed = state
        .store
        .with(move |store| store.remove_project(id))
        .await
        .map_err(|err| ApiErr::internal("remove project", err))?
        .map_err(|err| ApiErr::internal("remove project", err))?;
    if !removed {
        return Err(ApiErr::not_found(format!(
            "unknown project `{wanted}`; {NAME_A_PROJECT}"
        )));
    }

    // Same reasoning as registration: a stale manifest outlives the process,
    // so a failure here is loud even though the removal itself succeeded.
    {
        let data_dir = state.data_dir.clone();
        match state
            .store
            .with(move |store| crate::registry::publish(store, &data_dir))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) | Err(err) => tracing::error!(
                error = %format!("{err:#}"),
                "project removed but the registry manifest could not be written; \
                 it will come back on the next start"
            ),
        }
    }

    let _ = state.watch.send(WatchCommand::Unwatch(id));
    state.watch_status.forget(id);

    tracing::info!(
        project = %project.name,
        key = %project.key,
        root = %project.root,
        files, chunks,
        "project removed"
    );
    Ok(Json(RemoveProjectResponse {
        project: info(&project),
        files,
        chunks,
    }))
}

/// `GET /v1/resolve?path=<absolute path>` — which registered project contains
/// this path?
///
/// **INTERIM, and local-only.** The scoping resolution
/// (`design/4_Interfaces/2026-08-16_project-scoping-decision-brief.md`) settled
/// the wire contract — every query names one project — while explicitly
/// *deferring* the identity mechanism to issue #18's ingestion fork. Until
/// that is decided the identifier is the registry's own project name/key, and
/// this endpoint is the sanctioned convenience by which a co-located client
/// fills it in from where it happens to be standing.
///
/// It is acceptable only because the daemon is loopback-only today: registered
/// roots are paths on the daemon's filesystem, which means nothing to a remote
/// client. Expect it to be revisited — most sharply if ingestion inverts, which
/// makes path-based anything moot. The rest of the wire stays name/key-based;
/// this is the one route that takes a path.
#[derive(Debug, serde::Deserialize)]
struct ResolveQuery {
    path: String,
}

async fn resolve(
    State(state): State<AppState>,
    Query(query): Query<ResolveQuery>,
) -> ApiResult<ProjectInfo> {
    let path = Utf8Path::new(query.path.trim());
    if path.as_str().is_empty() || !path.is_absolute() {
        return Err(ApiErr::bad_request(format!(
            "resolve needs an absolute path, not `{}`; pass the client's working directory",
            query.path
        )));
    }

    let projects = projects_of(&state).await?;
    // Longest root wins. Roots may legitimately nest (a repo and a package
    // inside it are two projects), and the innermost one is the project the
    // caller is actually standing in — the same rule the watcher's routing
    // would reach for if it had to pick just one.
    let project = projects
        .iter()
        .filter(|project| paths::is_within(&project.root, path))
        .max_by_key(|project| project.root.as_str().len())
        .ok_or_else(|| {
            ApiErr::not_found(
                "path is not inside any registered project; register it with `lore add <path>`",
            )
        })?;
    Ok(Json(info(project)))
}

async fn index(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<IndexRequest>,
) -> ApiResult<IndexResponse> {
    let projects = projects_of(&state).await?;
    let targets: Vec<&Project> = match &request.project {
        Some(key) => vec![super::resolve_project(&projects, key).ok_or_else(|| {
            ApiErr::not_found(format!("unknown project `{key}`; {NAME_A_PROJECT}"))
        })?],
        None => projects.iter().collect(),
    };

    for project in &targets {
        state.queue.request_full(project.id);
    }
    Ok(Json(IndexResponse {
        queued: targets.into_iter().map(info).collect(),
    }))
}

/// `POST /v1/shutdown` — stop this daemon cleanly.
///
/// It exists because the alternative is killing the process, and a killed
/// daemon leaves behind a handshake whose heartbeat is still fresh: every
/// client then follows it to a dead port for up to `STALE_AFTER`, which the
/// stop/rebuild/start loop pays every single time (#8).
///
/// The token cancelled here is the daemon's one shutdown signal, so the same
/// thing happens as on ctrl-c: axum stops accepting and drains the requests
/// already in flight (this one included, which is why the response is sent at
/// all), the workers wind down, and the handshake is withdrawn last. The
/// answer is therefore an acknowledgement, not a completion — the caller
/// learns the daemon is *gone* by watching `daemon.json` disappear.
///
/// CLI-only by convention, exactly like registration: an agent that could stop
/// the daemon could stop every other agent's index.
async fn shutdown(State(state): State<AppState>) -> ApiResult<lore_core::ShutdownResponse> {
    let pid = std::process::id();
    tracing::info!(pid, "clean shutdown requested over the API");
    state.shutdown.cancel();
    Ok(Json(lore_core::ShutdownResponse { pid }))
}

async fn search_route(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<SearchRequest>,
) -> ApiResult<lore_core::SearchResponse> {
    let started = std::time::Instant::now();

    // Scoping is a requirement, not a default: an unscoped query used to span
    // every project on the machine, which on a shared daemon is one user
    // reading another's code.
    //
    // API_VERSION is deliberately *not* bumped for this. /v1 is pre-release,
    // no released client depends on the unscoped behavior, and the refusal is
    // self-describing — a client that sends the old shape gets a 400 telling
    // it exactly what to add, which is strictly more useful than a version
    // handshake failure that says only "upgrade".
    let scope = request
        .project_key
        .as_deref()
        .or(request.project.as_deref())
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .ok_or_else(|| {
            ApiErr::bad_request(format!(
                "search is scoped to one project: pass `project` (or `project_key`) and {NAME_A_PROJECT}"
            ))
        })?;

    // The store stage is recorded per project: the vector arm is an O(n)
    // scan, so its cost is a property of the corpus searched, not of the
    // daemon.
    let store_label = format!("search_store:{scope}");

    // Embedding the query is network I/O, so it happens *before* the store
    // lock is taken — never while holding it. `None` simply means this
    // request runs lexical-only (D-0007); it is never an error.
    let query_vector = state.embeddings.embed_query(&request.query).await;
    state.latency.record("search_embed", started.elapsed());

    let store_started = std::time::Instant::now();
    let outcome = state
        .store
        .with(move |store| search::execute(store, &request, query_vector.as_deref()))
        .await
        .map_err(|err| ApiErr::internal("search", err))?;
    // Both the global aggregate and the per-corpus window: `status` shows the
    // aggregate by default and the labeled row on request.
    state
        .latency
        .record("search_store", store_started.elapsed());
    state.latency.record(&store_label, store_started.elapsed());
    state.latency.record("search", started.elapsed());
    match outcome {
        Ok(response) => Ok(Json(response)),
        Err(
            err @ (search::SearchError::UnknownProject(_)
            | search::SearchError::UnknownProjectKey(_)),
        ) => Err(ApiErr::not_found(err.to_string())),
        Err(err @ search::SearchError::UnknownStatus(_)) => {
            Err(ApiErr::bad_request(err.to_string()))
        }
        Err(err) => Err(ApiErr::internal("search", err)),
    }
}

async fn expand_route(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ExpandRequest>,
) -> ApiResult<lore_core::ExpandResponse> {
    let started = std::time::Instant::now();
    let projects = projects_of(&state).await?;
    // Key first: it is exact. The display-name path stays for humans, older
    // clients, and anyone typing a command by hand.
    let project = match &request.project_key {
        Some(key) => super::resolve_project_key(&projects, key).ok_or_else(|| {
            ApiErr::not_found(format!("unknown project key `{key}`; {NAME_A_PROJECT}"))
        })?,
        // Same scoping requirement as `search`, and the same reasoning about
        // API_VERSION; here it was already an error, only under-explained.
        None if request.project.trim().is_empty() => {
            return Err(ApiErr::bad_request(format!(
                "expand is scoped to one project: pass `project_key` from the search result \
                 (or `project`) and {NAME_A_PROJECT}"
            )));
        }
        None => super::resolve_project(&projects, &request.project).ok_or_else(|| {
            ApiErr::not_found(format!(
                "unknown project `{}`; {NAME_A_PROJECT}",
                request.project
            ))
        })?,
    }
    .clone();

    let chunk_id = request.chunk_id.clone();
    let context_lines = request.context_lines;
    let found = state
        .store
        .with(move |store| expand::execute(store, &project, &chunk_id, context_lines))
        .await
        .map_err(|err| ApiErr::internal("expand", err))?;
    state.latency.record("expand", started.elapsed());

    // The id the caller sent is theirs to fix in three of these four cases,
    // and each message already says how; only a store failure is ours.
    found.map(Json).map_err(|err| match err {
        expand::ExpandError::Unknown { .. } => ApiErr::not_found(err.to_string()),
        expand::ExpandError::Store(err) => ApiErr::internal("expand", err),
        err => ApiErr::bad_request(err.to_string()),
    })
}

async fn projects_of(state: &AppState) -> Result<Vec<Project>, ApiErr> {
    state
        .store
        .with(|store| store.list_projects())
        .await
        .map_err(|err| ApiErr::internal("list projects", err))?
        .map_err(|err| ApiErr::internal("list projects", err))
}

fn info(project: &Project) -> ProjectInfo {
    ProjectInfo {
        id: project.id,
        name: project.name.clone(),
        key: project.key.clone(),
        root: project.root.to_string(),
        kind: project.kind.as_str().to_string(),
    }
}
