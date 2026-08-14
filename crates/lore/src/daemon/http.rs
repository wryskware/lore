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
//! No authentication: the listener binds `127.0.0.1` only, so reaching it
//! already requires local code execution.
// TODO(hardening): a bearer secret written into daemon.json (readable only by
// the owning user) would additionally stop *other local users* and untrusted
// local processes from reading the index. Deferred out of M1 deliberately —
// it needs a decision about how MCP clients receive the secret.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use camino::Utf8PathBuf;

use lore_core::{
    DaemonStatus, ExpandRequest, IndexRequest, IndexResponse, ProjectInfo, ProjectList,
    RegisterProjectRequest, SearchRequest,
};

use crate::config::Config;
use crate::store::Project;

use super::queue::IndexQueue;
use super::store_handle::StoreHandle;
use super::watch::{WatchCommand, WatchSender};
use super::{expand, paths, search};

/// Request bodies are small JSON documents; a megabyte is generous for the
/// largest realistic one (a pasted query) and cheap insurance otherwise.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub store: StoreHandle,
    pub queue: IndexQueue,
    pub watch: WatchSender,
    pub config: Arc<Config>,
    pub data_dir: Utf8PathBuf,
}

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/status", get(status))
        .route("/projects", get(list_projects).post(register_project))
        .route("/index", post(index))
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

async fn status(State(state): State<AppState>) -> ApiResult<DaemonStatus> {
    let status = state
        .store
        .with(|store| store.status())
        .await
        .map_err(|err| ApiErr::internal("status", err))?
        .map_err(|err| ApiErr::internal("status", err))?;

    Ok(Json(DaemonStatus {
        api_version: lore_core::API_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        generation: status.generation,
        projects: status
            .projects
            .into_iter()
            .map(|p| lore_core::ProjectStatus {
                id: p.project,
                name: p.name,
                root: p.root.into_string(),
                files: p.files,
                chunks: p.chunks,
                embedded_chunks: p.embedded_chunks,
            })
            .collect(),
        embeddings: state.config.embedding_status(),
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

    let project = Project {
        id: registered,
        root,
        name,
    };
    tracing::info!(project = %project.name, root = %project.root, id = project.id, "project registered");
    let _ = state.watch.send(WatchCommand::Watch(project.clone()));
    state.queue.request_full(project.id);

    Ok(Json(info(&project)))
}

async fn index(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<IndexRequest>,
) -> ApiResult<IndexResponse> {
    let projects = projects_of(&state).await?;
    let targets: Vec<&Project> = match &request.project {
        Some(key) => vec![
            super::resolve_project(&projects, key)
                .ok_or_else(|| ApiErr::not_found(format!("unknown project `{key}`")))?,
        ],
        None => projects.iter().collect(),
    };

    for project in &targets {
        state.queue.request_full(project.id);
    }
    Ok(Json(IndexResponse {
        queued: targets.into_iter().map(info).collect(),
    }))
}

async fn search_route(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<SearchRequest>,
) -> ApiResult<lore_core::SearchResponse> {
    let outcome = state
        .store
        .with(move |store| search::execute(store, &request))
        .await
        .map_err(|err| ApiErr::internal("search", err))?;
    match outcome {
        Ok(response) => Ok(Json(response)),
        Err(err @ search::SearchError::UnknownProject(_)) => {
            Err(ApiErr::not_found(err.to_string()))
        }
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
    let projects = projects_of(&state).await?;
    let project = super::resolve_project(&projects, &request.project)
        .ok_or_else(|| ApiErr::not_found(format!("unknown project `{}`", request.project)))?
        .clone();

    let chunk_id = request.chunk_id.clone();
    let context_lines = request.context_lines;
    let found = state
        .store
        .with(move |store| expand::execute(store, &project, &chunk_id, context_lines))
        .await
        .map_err(|err| ApiErr::internal("expand", err))?
        .map_err(|err| ApiErr::internal("expand", err))?;

    found
        .map(Json)
        .ok_or_else(|| ApiErr::not_found(format!("unknown chunk `{}`", request.chunk_id)))
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
        root: project.root.to_string(),
    }
}
