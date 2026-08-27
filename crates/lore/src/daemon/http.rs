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
//! Every *query* is scoped to exactly one project: `search`, `expand` and
//! `bundle` reject a request that names none, with an error that says how to
//! name one
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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use camino::{Utf8Path, Utf8PathBuf};

use lore_core::snapshot::{
    PushCommitRequest, PushCommitResponse, PushEpoch, PushFileParams, PushFileResponse,
    PushLeaseRequest, PushLeaseResponse, PushManifestRequest, PushManifestResponse,
    PushRenewRequest, PushSessionId, malformed_path,
};
use lore_core::{
    BundleRequest, DaemonStatus, ExpandRequest, IndexRequest, IndexResponse, ProjectInfo,
    ProjectList, RegisterProjectRequest, RemoveProjectResponse, SearchRequest,
};

use crate::config::Config;
use crate::embed::Embedder;
use crate::store::{Project, ProjectId, RegisterError};

use super::index::{ApplyOptions, IndexContext};
use super::push::{PushClaim, PushError, PushLeases};
use super::queue::IndexQueue;
use super::snapshot::{Scope, Snapshot};
use super::store_handle::StoreHandle;
use super::watch::{WatchCommand, WatchSender, WatchStatus};
use super::{bundle, expand, follow, index, paths, push, search};

/// Request bodies are small JSON documents; a megabyte is generous for the
/// largest realistic one (a pasted query) and cheap insurance otherwise.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// The two push routes that carry bulk get their own, much larger ceiling: a
/// manifest is ~100 bytes per file (a 300k-file repository is a ~30 MB
/// message, D-0015) and an upload is one file's bytes.
///
/// Generous rather than tuned. It is not a policy on what may be indexed —
/// oversized files are the chunker's business, and pusher-side ignore rules
/// decide what is listed at all — it exists so one malformed request cannot
/// ask this process for unbounded memory.
pub const MAX_PUSH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub store: StoreHandle,
    pub queue: IndexQueue,
    pub watch: WatchSender,
    /// Live per-project watcher coverage, written by the watcher pump.
    pub watch_status: WatchStatus,
    /// The indexer's context, shared so a push commit runs the *same* apply
    /// pipeline the walker's snapshots run through (D-0015) rather than a
    /// parallel one. It also carries the mass-delete guard's record, which is
    /// what `status` reports.
    pub index: IndexContext,
    /// Per-project push leases, epochs and staging areas.
    pub push: PushLeases,
    pub config: Arc<Config>,
    /// Live embedding capability and health. Shared with the embed worker,
    /// which is the thing that actually probes the endpoint.
    pub embeddings: Embedder,
    /// Query-surface latency windows, reported by `status`.
    pub latency: crate::daemon::latency::LatencyRecorder,
    /// Chunker plugins this daemon loaded at startup. The same registry the
    /// indexer routes through, shared so `status` and the push lease report
    /// what is actually in force rather than re-reading the directory and
    /// possibly disagreeing with it.
    pub plugins: Arc<crate::plugin::PluginRegistry>,
    /// What the plugin load refused, already rendered. Rendered rather than
    /// typed because this surface only ever prints them.
    pub plugin_diagnostics: Arc<Vec<String>>,
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
        .route("/bundle", post(bundle_route))
        // The push surface (D-0015). Same `/v1`, same loopback listener, same
        // absence of authentication: a lease is a consistency primitive, and
        // a deployment that serves more than one trusting party authenticates
        // *in front of* this daemon (issue #18).
        .route("/push/lease", post(push_lease))
        .route("/push/lease/renew", post(push_renew))
        .route(
            "/push/manifest",
            post(push_manifest).layer(DefaultBodyLimit::max(MAX_PUSH_BYTES)),
        )
        .route(
            "/push/file",
            post(push_file).layer(DefaultBodyLimit::max(MAX_PUSH_BYTES)),
        )
        .route("/push/commit", post(push_commit))
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

    // Resolved here rather than read from the store, because neither the extent
    // nor the plugin opt-in is stored: every pass loads `.lore.toml` fresh, and
    // status showing anything else would be showing the configuration of some
    // earlier pass. Off the async thread — it reads a file and canonicalizes a
    // path per source.
    let roots: Vec<(ProjectId, camino::Utf8PathBuf)> = status
        .projects
        .iter()
        .map(|p| (p.project, p.root.clone()))
        .collect();
    type RepoConfigs = BTreeMap<ProjectId, (crate::sources::Sources, BTreeSet<String>)>;
    let configs: RepoConfigs = tokio::task::spawn_blocking(move || {
        roots
            .into_iter()
            .map(|(id, root)| {
                let extent = crate::sources::Sources::load(&root);
                (id, (extent, crate::repo_config::enabled_plugins(&root)))
            })
            .collect()
    })
    .await
    .map_err(|err| ApiErr::internal("status", err))?;
    let extents: BTreeMap<ProjectId, &crate::sources::Sources> = configs
        .iter()
        .map(|(id, (extent, _))| (*id, extent))
        .collect();

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
            // Reported only when the project is more than its own root: one
            // anonymous source on every project would be noise, and its
            // absence is exactly what "this project is its root" means.
            sources: match extents.get(&p.project) {
                Some(extent) if !extent.is_only_root() => extent
                    .iter()
                    .map(|source| lore_core::SourceInfo {
                        mount: source.mount.clone(),
                        root: source.root.to_string(),
                    })
                    .collect(),
                _ => Vec::new(),
            },
            sources_error: extents
                .get(&p.project)
                .and_then(|extent| extent.error().map(ToString::to_string)),
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
            // A refused apply (D-0015). Same reasoning as the watch state
            // above: an index that has stopped tracking its project is a
            // silent failure unless it is reported.
            mass_delete_guard: state.index.guard.of(p.project),
            // Push state (D-0015). Absent for a purely local project, which
            // never takes a lease; present, and *churning*, is what sustained
            // pusher contention looks like from outside.
            push_lease_epoch: state.push.state(p.project).map(|push| push.epoch.0),
            push_staged: state.push.state(p.project).is_some_and(|push| push.staged),
            // Chunking for a project is the intersection of installed and
            // enabled, so both halves of the gap are reported: the plugins in
            // force, and the names this repo asked for that nobody installed.
            plugins_enabled: configs
                .get(&p.project)
                .map(|(_, enabled)| {
                    enabled
                        .iter()
                        .filter_map(|name| state.plugins.get(name))
                        .map(plugin_info)
                        .collect()
                })
                .unwrap_or_default(),
            plugins_missing: configs
                .get(&p.project)
                .map(|(_, enabled)| {
                    enabled
                        .iter()
                        .filter(|name| state.plugins.get(name).is_none())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            plugin_fallback_files: state.index.fallbacks.of(p.project),
        })
        .collect();

    // Scoping the report, when asked. Identity is matched the same three ways
    // everything else accepts a project (name, key, id), so a caller holding
    // any of them can narrow without first translating it — and the name is
    // folded exactly as `resolve_project` folds it, because this route is also
    // how `lore-mcp` resolves a `LORE_PROJECT` pin.
    if let Some(wanted) = query.project.as_deref() {
        projects.retain(|p| {
            p.name.eq_ignore_ascii_case(wanted) || p.key == wanted || p.id.to_string() == wanted
        });
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
            // The label is whatever the request said, so a name is matched the
            // same folded way it was resolved; a key is opaque and matched
            // exactly.
            let names_a_kept_project = |label: &str| {
                projects
                    .iter()
                    .any(|p| p.name.eq_ignore_ascii_case(label) || p.key == label)
            };
            let mut latency = state.latency.snapshot();
            latency.retain(|l| match l.endpoint.split_once(':') {
                None => true, // global endpoints always
                Some((_, project)) => query.project.is_some() && names_a_kept_project(project),
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
        // Also machine-wide, and for the same reason: plugins are *installed*
        // per machine. What a scoped caller wants is the per-project pair
        // above, which is already filtered with it.
        plugins: installed_plugins(&state),
        plugin_diagnostics: state.plugin_diagnostics.as_ref().clone(),
    }))
}

/// Every installed plugin, on the wire.
fn installed_plugins(state: &AppState) -> Vec<lore_core::PluginInfo> {
    state
        .plugins
        .plugins()
        .iter()
        .map(|plugin| plugin_info(plugin))
        .collect()
}

/// One loaded plugin, on the wire. Shared by `status` and the push lease so a
/// pusher and a local operator are told the same three things about a plugin.
fn plugin_info(plugin: &crate::plugin::Plugin) -> lore_core::PluginInfo {
    lore_core::PluginInfo {
        name: plugin.name.clone(),
        fingerprint: plugin.fingerprint.clone(),
        extensions: plugin
            .extensions()
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    }
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

    // The declared name is the project's identity (D-0016), so the store
    // refuses to bind one that another root already holds — see
    // [`Store::register_project`] for why the check lives there and not here.
    //
    // 409 rather than 400: the request is well-formed and the name is legal —
    // it is the *registry's current state* that refuses it, and that is a
    // state the caller can change. Re-adding the same root under the same name
    // is not a collision with itself; it is the idempotent rename path.
    let registered = {
        let (owned_root, owned_name) = (root.clone(), name.clone());
        state
            .store
            .with(move |store| store.register_project(&owned_root, &owned_name))
            .await
            .map_err(|err| ApiErr::internal("register", err))?
            .map_err(|err| match err {
                RegisterError::NameTaken {
                    name,
                    held_name,
                    held_by,
                } => ApiErr::conflict(format!(
                    "a project named `{held_name}` is already registered at {held_by}, so {root} \
                     cannot also claim `{name}`: a project's identity is the name it is \
                     registered under, names are compared without regard to case, and one name \
                     binds to one root. Register this one under a different name with \
                     `lore add <path> --name <name>` (or edit that repo's .lore.toml), or free \
                     the name with `lore remove {held_name}`"
                )),
                RegisterError::Store(err) => ApiErr::internal("register", err),
            })?
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

    // Nothing is written into the project. D-0020 retired `.loreignore`
    // generation: a project's ignore rules exist only where a human wrote them,
    // and registration's whole footprint on the repo is `.lore.toml`'s name.
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
    state.index.guard.forget(id);
    state.index.fallbacks.forget(id);
    // A lease on a project that no longer exists cannot commit anything, so
    // its staged content goes with it rather than waiting for the reaper.
    if let Some(dir) = state.push.forget(id) {
        discard_staging(dir).await;
    }

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
/// **A discovery convenience, and local-only — never identity.** D-0016
/// settled what the scoping brief deferred: a project's identity is its
/// registry-bound declared name, and cwd-containment is demoted to a local
/// convenience by which a co-located client *fills in* an identifier it was
/// not given. It never overrides one. Callers hold that line — the CLI's
/// `--project` and the MCP server's `LORE_PROJECT` are both consulted first,
/// and a pin that names no project is an error rather than a quiet fall back
/// to wherever the process happens to be standing.
///
/// Containment stays local-only because it is meaningless anywhere else:
/// registered roots are paths on the daemon's filesystem, and D-0015's push
/// ingestion means a client's paths need not exist there at all. The rest of
/// the wire is name/key-based; this is the one route that takes a path.
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
            // Failing to discover a project is not failing to have one: a
            // project is identified by the name it is registered under, and
            // this route only guesses that name from a path. So both ways
            // forward are named — supply the identity, or create it.
            ApiErr::not_found(format!(
                "path is not inside any registered project; {NAME_A_PROJECT}"
            ))
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
        if request.allow_mass_delete {
            state.queue.request_full_allowing_mass_delete(project.id);
        } else {
            state.queue.request_full(project.id);
        }
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

/// `POST /v1/bundle` — one query in, a finished evidence bundle out.
///
/// The route is `search` plus everything a caller would otherwise do with the
/// answer: verify each hit against the file on disk, widen and merge the spans,
/// fit them to a token budget, and put an honest verdict on top. It lives here
/// rather than in a client for the reason D-0003 gives — the daemon is the one
/// owner of index state, and it is also the only process that can read the
/// corpus knowing what the index currently believes about it.
///
/// Two stages, deliberately separated: the store answers the query and the lock
/// is released, and only then does assembly touch the filesystem, on the
/// blocking pool. Holding the store across two dozen file reads would make
/// every other query wait on this one's IO.
async fn bundle_route(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<BundleRequest>,
) -> ApiResult<lore_core::BundleResponse> {
    let started = std::time::Instant::now();

    // Same scoping requirement as `search`, and resolved to the *project* here
    // rather than passed through as a label, because assembly needs the root
    // the paths in the answer are relative to.
    let scope = request
        .project_key
        .as_deref()
        .or(request.project.as_deref())
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .ok_or_else(|| {
            ApiErr::bad_request(format!(
                "bundle is scoped to one project: pass `project` (or `project_key`) and \
                 {NAME_A_PROJECT}"
            ))
        })?
        .to_string();
    let projects = projects_of(&state).await?;
    let project = super::resolve_project_key(&projects, &scope)
        .or_else(|| super::resolve_project(&projects, &scope))
        .ok_or_else(|| ApiErr::not_found(format!("unknown project `{scope}`; {NAME_A_PROJECT}")))?
        .clone();

    let limit = request
        .limit
        .unwrap_or(bundle::DEFAULT_LIMIT)
        .min(search::MAX_LIMIT);
    let budget_tokens = request
        .budget_tokens
        .unwrap_or(bundle::DEFAULT_BUDGET_TOKENS);
    let search_request = SearchRequest {
        query: request.query.clone(),
        project_key: Some(project.key.clone()),
        limit: Some(limit),
        ..SearchRequest::default()
    };

    // Following is OFF unless the caller asks for it. The design pre-committed
    // to a bar (primary span_recall_half +0.10 on the follow pair) before the
    // measurement ran; the 2026-08-27 retrieval eval came in under it — verdicts
    // and distractors held perfectly, tokens +9-11%, but primary half-coverage
    // did not move on n=18. "Below that bar the feature is not worth its tokens
    // and should not default on." A consumption-side result (an answerer round
    // with follow on) is what would earn the default back.
    //
    // When it does run, it is one more statement inside the *same* store
    // acquisition as the search itself: taking the lock twice would let the
    // index move between the ranking and the definitions it named.
    let follow = request.follow.unwrap_or(false);
    let project_id = project.id;

    // Network I/O before the store lock, exactly as `search` does it; `None`
    // means this bundle runs lexical-only (D-0007) and says so in its header.
    let query_vector = state.embeddings.embed_query(&search_request.query).await;
    let (results, followed) = state
        .store
        .with(move |store| {
            let results = search::execute(store, &search_request, query_vector.as_deref())?;
            let followed = follow::resolve(store, project_id, &results.results, follow);
            Ok::<_, search::SearchError>((results, followed))
        })
        .await
        .map_err(|err| ApiErr::internal("bundle", err))?
        // The project was resolved above, so the scoping variants are
        // unreachable here; they are still mapped rather than swallowed into a
        // 500, because a project removed between the two calls is the caller's
        // answer to have.
        .map_err(|err| match err {
            err @ (search::SearchError::UnknownProject(_)
            | search::SearchError::UnknownProjectKey(_)) => ApiErr::not_found(err.to_string()),
            err => ApiErr::internal("bundle", err),
        })?;

    let query = request.query;
    let root = project.root.clone();
    let response = tokio::task::spawn_blocking(move || {
        // Loaded per request rather than cached: every other pass reads
        // `.lore.toml` fresh, and a bundle resolving paths against a stale
        // extent would quote files from a mount the project no longer declares.
        let sources = crate::sources::Sources::load(&root);
        bundle::assemble(&query, &results, &followed, &sources, budget_tokens, limit)
    })
    .await
    .map_err(|err| ApiErr::internal("bundle", err))?;

    state.latency.record("bundle", started.elapsed());
    Ok(Json(response))
}

// ---------------------------------------------------------------------------
// Push (D-0015)
// ---------------------------------------------------------------------------
//
// Four steps — lease, manifest, upload, commit — over the same loopback
// listener as everything else. The handlers are thin on purpose: consistency
// lives in [`super::push`], applying lives in [`super::index`], and neither
// learns anything here about HTTP.
//
// **No authorization anywhere in this section.** Every refusal below answers
// "is this pusher's view of the project current?", never "may this caller
// write?". The daemon has no users and no roles; nothing binds beyond
// 127.0.0.1.

/// Push refusals, by kind.
///
/// The consistency failures are 409: the request is well-formed and legal, and
/// it is the *daemon's current state* — someone else holds the lease — that
/// refuses it, which is a state the caller can change by acquiring again. The
/// contract violations are 400, because the caller sent something that was
/// never going to work. The floor is 429, because the only fix is to wait.
fn push_err(err: PushError) -> ApiErr {
    let status = match &err {
        PushError::NoLease { .. }
        | PushError::TakenOver { .. }
        | PushError::UnknownSession { .. }
        | PushError::EpochAhead { .. }
        | PushError::CommitInFlight { .. } => StatusCode::CONFLICT,
        PushError::TooSoon { .. } => StatusCode::TOO_MANY_REQUESTS,
        PushError::ManifestCorrupt
        | PushError::ManifestMalformed { .. }
        | PushError::NoManifest
        | PushError::NotInManifest { .. }
        | PushError::SizeMismatch { .. }
        | PushError::HashMismatch { .. }
        | PushError::Incomplete { .. } => StatusCode::BAD_REQUEST,
        PushError::Staging { .. } => return ApiErr::internal("push staging", err),
    };
    ApiErr::new(status, err.to_string())
}

/// Resolve the project a push names.
///
/// Identity is the registry-bound declared name (D-0016). Containment plays no
/// part: a pusher's paths need not exist on this daemon's filesystem at all,
/// which is the whole point of the inversion.
async fn push_project(state: &AppState, wanted: &str) -> Result<Project, ApiErr> {
    let projects = projects_of(state).await?;
    super::resolve_project(&projects, wanted)
        .cloned()
        .ok_or_else(|| ApiErr::not_found(format!("unknown project `{wanted}`; {NAME_A_PROJECT}")))
}

/// The identity every push route checks against the live lease.
fn claim(project: &Project, session: &PushSessionId, epoch: PushEpoch) -> PushClaim {
    PushClaim {
        project: project.id,
        name: project.name.clone(),
        session: session.clone(),
        epoch,
    }
}

/// Delete a staging area off the runtime; failures are the module's to log.
async fn discard_staging(dir: Utf8PathBuf) {
    let _ = tokio::task::spawn_blocking(move || push::discard(&dir)).await;
}

/// The lease, and everything about this receiver a pusher cannot infer.
///
/// The plugin set rides here rather than on a route of its own because a pusher
/// already calls this before every push and never needs it at any other moment:
/// chunking is receiver-side, so what the receiver has installed is a property
/// of the session being opened. It is advertisement only — no negotiation, no
/// refusal, nothing conditional on it (2026-08-17 contract, "Remote mode").
fn lease_response(state: &AppState, session: PushSessionId, epoch: PushEpoch) -> PushLeaseResponse {
    PushLeaseResponse {
        session,
        epoch,
        ttl_secs: state.push.ttl().as_secs(),
        heartbeat_secs: state.config.push.heartbeat().as_secs(),
        min_push_interval_secs: state.push.min_interval().as_secs(),
        plugins: installed_plugins(state),
    }
}

/// `POST /v1/push/lease` — take the project's push lease.
///
/// Always succeeds against a live project: conflict policy is takeover
/// (D-0015, decided), so a second acquirer displaces the holder rather than
/// waiting for a TTL nobody can choose correctly. The displaced session learns
/// about it on its next request, by name and with a timestamp.
///
/// The epoch is minted by the store, not by this handler, because it has to be
/// monotonic across daemon restarts as well as across concurrent acquirers.
async fn push_lease(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<PushLeaseRequest>,
) -> ApiResult<PushLeaseResponse> {
    let project = push_project(&state, &request.project).await?;
    let id = project.id;
    let epoch = state
        .store
        .with(move |store| store.bump_push_epoch(id))
        .await
        .map_err(|err| ApiErr::internal("push lease", err))?
        .map_err(|err| ApiErr::internal("push lease", err))?
        .ok_or_else(|| {
            ApiErr::not_found(format!(
                "project `{}` was removed while its lease was being acquired; {NAME_A_PROJECT}",
                project.name
            ))
        })?;
    let epoch = PushEpoch(epoch);

    let (session, displaced) = state
        .push
        .acquire(id, &project.name, epoch)
        .map_err(push_err)?;
    // A displaced session's uploads can never be committed, so they are
    // deleted rather than left to the reaper: D-0015's only cleanup.
    if let Some(dir) = displaced {
        discard_staging(dir).await;
    }
    tracing::info!(project = %project.name, epoch = %epoch, "push lease acquired");
    Ok(Json(lease_response(&state, session, epoch)))
}

/// `POST /v1/push/lease/renew` — heartbeat an idle lease.
///
/// Only needed in the gaps: every accepted push request is itself proof of
/// life, so a pusher working its way through uploads never has to renew
/// concurrently.
async fn push_renew(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<PushRenewRequest>,
) -> ApiResult<PushLeaseResponse> {
    let project = push_project(&state, &request.project).await?;
    state
        .push
        .touch(&claim(&project, &request.session, request.epoch))
        .map_err(push_err)?;
    Ok(Json(lease_response(&state, request.session, request.epoch)))
}

/// `POST /v1/push/manifest` — negotiate what content the daemon needs.
///
/// Order is load-bearing. The checksum is verified **first**, before the
/// listing is allowed to influence anything: a manifest's *absences* delete
/// files, so a listing that did not survive the wire is not a smaller listing,
/// it is a deletion instruction of unknown provenance.
///
/// The answer also carries the delete count, which is the mass-delete guard's
/// preview: a pusher learns its listing looks like a catastrophe here, before
/// it uploads anything. Enforcement stays where it already lives — the apply
/// pipeline's guard at commit — so there is exactly one notion of "too many",
/// and the per-invocation override rides along to it on the session.
///
/// # The hard-exclude backstop (D-0015)
///
/// Ignore evaluation is trusted client-side; the receiver keeps a backstop, and
/// it answers two different kinds of bad entry two different ways.
///
/// **Structurally impossible paths refuse the whole manifest** (400). Empty,
/// absolute, backslashed, `..`-shaped: a client that emits one is not
/// implementing the contract, so nothing else it listed can be trusted either —
/// least of all the *absences*, which delete files.
///
/// **Hard-excluded paths are refused individually** and the manifest is
/// accepted without them. A client listing `.env` is misconfigured, not broken;
/// refusing its whole snapshot would take a working index offline over one
/// file. Everything downstream then runs on the accepted set: the needed list,
/// the delete preview, the stored manifest, and therefore the mass-delete
/// guard's arithmetic at commit. One consequence is deliberate — a refused path
/// the store already has is now *absent*, and absence is deletion, so committing
/// purges it. A secret that reached the index once has to be able to leave it.
async fn push_manifest(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<PushManifestRequest>,
) -> ApiResult<PushManifestResponse> {
    if !request.manifest.is_intact() {
        return Err(push_err(PushError::ManifestCorrupt));
    }
    // Before the project is even resolved: a listing this shape is not a
    // listing, whoever it was meant for.
    if let Some((path, reason)) =
        request.manifest.entries.iter().find_map(|entry| {
            malformed_path(&entry.path).map(|reason| (entry.path.clone(), reason))
        })
    {
        return Err(push_err(PushError::ManifestMalformed { path, reason }));
    }
    let project = push_project(&state, &request.project).await?;
    let claim = claim(&project, &request.session, request.epoch);
    state.push.touch(&claim).map_err(push_err)?;
    state
        .push
        .check_interval(project.id, &project.name)
        .map_err(push_err)?;

    // The diff is one `list_files` — which is what a manifest diff *is* — plus
    // the stored authority profile, because the stamp a stored hash carries
    // depends on it (D-0012). Stored rather than freshly read from disk: it is
    // the fingerprint the last pass wrote, so it answers "what does the store
    // already have" exactly. A `.lore.toml` edit landing between this
    // negotiation and the commit makes the commit re-chunk Markdown it did not
    // ask content for; those files count as pass errors and are *not* deleted
    // (see `StagedContent::read`), and the next push settles it.
    let id = project.id;
    let (stored, profile) = state
        .store
        .with(move |store| {
            let files = store.list_files(id)?;
            let authority = store.project_authority(id)?;
            Ok::<_, crate::store::StoreError>((files, authority.active()))
        })
        .await
        .map_err(|err| ApiErr::internal("push manifest", err))?
        .map_err(|err| ApiErr::internal("push manifest", err))?;

    // No content screening: D-0020 makes the receiver structural-only, and the
    // structural verdict was already reached above. What the listing otherwise
    // contains is the pusher's business.
    let manifest = request.manifest;

    // The plugins in force for this project, resolved the same way the pass
    // itself resolves them (receiver-side `.lore.toml` ∩ installed): a stamp
    // carries the claiming plugin's fingerprint, so a diff that did not know
    // about plugins would consider every claimed file unchanged and never ask
    // for the content that has to be re-chunked.
    let root = project.root.clone();
    let enabled = tokio::task::spawn_blocking(move || crate::repo_config::enabled_plugins(&root))
        .await
        .map_err(|err| ApiErr::internal("push manifest", err))?;
    let plugins = state.plugins.enabled_only(&enabled);

    let known: BTreeMap<&str, &str> = stored
        .iter()
        .map(|record| (record.path.as_str(), record.content_hash.as_str()))
        .collect();
    let needed: Vec<String> = manifest
        .entries
        .iter()
        .filter(|entry| {
            let stamp = index::content_stamp(
                Utf8Path::new(&entry.path),
                &entry.hash,
                profile,
                Some(&plugins),
            );
            known.get(entry.path.as_str()).copied() != Some(stamp.as_str())
        })
        .map(|entry| entry.path.clone())
        .collect();
    // Deletion is absence, and a manifest speaks for the whole project — the
    // *accepted* manifest, so a stored copy of a refused path counts here and
    // is purged at commit, and the mass-delete guard sees the same number.
    let deletes = stored
        .iter()
        .filter(|record| manifest.get(record.path.as_str()).is_none())
        .count() as u64;

    let missing = state
        .push
        .record_manifest(&claim, manifest, request.allow_mass_delete, needed)
        .map_err(push_err)?;

    tracing::debug!(
        project = %project.name,
        epoch = %request.epoch,
        needed = missing.len(),
        deletes,
        "push manifest negotiated"
    );
    Ok(Json(PushManifestResponse {
        needed: missing,
        deletes,
    }))
}

/// `POST /v1/push/file?project=&session=&epoch=&path=` — stage one needed
/// path's content, sent as the raw request body.
///
/// Every upload is verified against the manifest that negotiated it — size
/// first because it is free, then the hash. A mismatch stages nothing and is
/// refused by name, so the path stays needed and the commit refuses until it
/// is right; content that disagrees with the listing is not the content this
/// commit was negotiated over.
async fn push_file(
    State(state): State<AppState>,
    Query(params): Query<PushFileParams>,
    body: Bytes,
) -> ApiResult<PushFileResponse> {
    let project = push_project(&state, &params.project).await?;
    let claim = claim(&project, &params.session, params.epoch);
    let target = state
        .push
        .stage_target(&claim, &params.path)
        .map_err(push_err)?;

    // Hashing and writing megabytes: off the reactor, and off the lease lock.
    let path = params.path.clone();
    let hash = tokio::task::spawn_blocking(move || push::stage_bytes(&target, &path, &body))
        .await
        .map_err(|err| ApiErr::internal("push upload", err))?
        .map_err(push_err)?;

    let progress = state
        .push
        .stage_recorded(&claim, &params.path, hash)
        .map_err(push_err)?;
    Ok(Json(PushFileResponse {
        staged: progress.staged,
        needed: progress.needed,
    }))
}

/// `POST /v1/push/commit` — publish everything staged under this session.
///
/// The epoch is verified **here**, with the commit, and not merely when the
/// manifest was negotiated: a session taken over mid-upload must not publish,
/// and at manifest time the takeover had not happened yet.
///
/// What the commit deliberately does *not* do is hold the lease lock across
/// the apply. A takeover landing during a commit therefore wins the next
/// commit rather than interleaving with this one — which is exactly what
/// D-0015 means by degrading to flapping between internally-consistent
/// snapshots. The transaction semantics of the publication itself are the
/// pipeline's, unchanged: this is the existing apply/prune with a different
/// observation source.
async fn push_commit(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<PushCommitRequest>,
) -> ApiResult<PushCommitResponse> {
    let project = push_project(&state, &request.project).await?;
    let claim = claim(&project, &request.session, request.epoch);
    let plan = state.push.take_commit(&claim).map_err(push_err)?;

    let options = ApplyOptions {
        allow_mass_delete: plan.allow_mass_delete,
    };
    let snapshot = Snapshot {
        manifest: plan.manifest,
        // A manifest is the complete listing of the project, which is what
        // makes its absences deletions.
        scope: Scope::Project,
        // A manifest that reached the commit is by construction complete: the
        // pusher declared it whole and the checksum agreed.
        complete: true,
        unreadable: BTreeSet::new(),
        // A pusher resolves its own links before it builds a manifest; what
        // it declined to follow is its business and never reaches the wire.
        links: BTreeSet::new(),
        content: Box::new(plan.content),
    };

    let ctx = state.index.clone();
    let target = project.clone();
    let applied = tokio::task::spawn_blocking(move || {
        index::apply_snapshot(&ctx, &target, snapshot, options)
    })
    .await;

    let summary = match applied {
        Ok(summary) => summary,
        Err(err) => {
            // The apply panicked. Release the commit claim anyway, or this
            // project could never be committed to again.
            state.push.commit_finished(&claim, false);
            return Err(ApiErr::internal("push commit", err));
        }
    };

    let published = summary.mass_delete_blocked.is_none() && !summary.cancelled;
    if let Some(dir) = state.push.commit_finished(&claim, published) {
        discard_staging(dir).await;
    }

    if let Some(trip) = summary.mass_delete_blocked {
        return Err(ApiErr::conflict(format!(
            "{trip}; nothing was written. The staged content is kept — re-send this manifest \
             with `allow_mass_delete` and commit again if the deletion is intended"
        )));
    }
    if summary.cancelled {
        return Err(ApiErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the daemon is shutting down; nothing was published and the staged content is kept",
        ));
    }

    tracing::info!(
        project = %project.name,
        epoch = %request.epoch,
        generation = summary.generation,
        indexed = summary.indexed,
        deleted = summary.removed,
        "push committed"
    );
    Ok(Json(PushCommitResponse {
        generation: summary.generation,
        indexed: summary.indexed as u64,
        deleted: summary.removed as u64,
    }))
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
