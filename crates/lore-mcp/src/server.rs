//! The MCP surface (design 4.1): `search`, `expand`, `status`. Three tools,
//! because those three compose — not because of a tool-count budget.
//!
//! Registration and reindex are deliberately absent: they are CLI-only
//! (`lore add`, `lore index`), so an agent cannot enroll a random directory
//! into the index on its own initiative.
//!
//! Every tool is a proxy. This module parses arguments, calls one daemon route,
//! renders the answer, and returns; it holds no index state and makes no
//! ranking decisions of its own (D-0007).
//!
//! # Scoping is automatic and not negotiable
//!
//! Every query is scoped to exactly one project (the scoping resolution in
//! `design/4_Interfaces/2026-08-16_project-scoping-decision-brief.md`), and the
//! agent does not choose it: the server resolves *its own* project once per
//! process and sends it on every call. Cross-project search is therefore not
//! reachable from here at all — the `project` parameter is gone rather than
//! defaulted, because a parameter an agent can set is a parameter it will set.
//!
//! The identifier itself is the registry-bound declared name (D-0016), which
//! is what `LORE_PROJECT` carries and what every call ultimately sends. When
//! it is unset, resolution falls back to the daemon's `GET /v1/resolve`
//! against this process's working directory — an editor spawns the server
//! inside the workspace it opened, so containment usually guesses right. That
//! ordering is the whole of the rule: containment *fills in* an identity that
//! was not supplied and never overrides one, so a `LORE_PROJECT` naming no
//! registered project is an error rather than a silent fall back to whatever
//! directory this process was started in.

use std::sync::Arc;

use camino::Utf8PathBuf;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use tokio::sync::OnceCell;

use lore_core::{ExpandRequest, ProjectInfo, SearchRequest};

use crate::daemon::{DaemonClient, DaemonError, Endpoint};
use crate::render;

// Spelled out as an enum rather than free strings so the vocabulary reaches the
// agent in the tool schema instead of having to be guessed. NOTE: this doc
// comment is agent-facing — schemars copies it into the JSON Schema.
/// How settled a design document *declares* itself to be. `decided` means
/// validated canon: Lore honors it only when the document cites a decision
/// that is still active in the project's ledger, and demotes it otherwise
/// (the result then carries an `authority note` saying so). `leaning` and
/// `exploration` record thinking that is not binding; `deprecated` has been
/// superseded; `unclassified` means the document declares no status at all.
/// Note that this filters on the declaration, not on the authority Lore
/// actually assigns — scratch and research material is demoted by path
/// whatever it declares.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StatusFilter {
    Exploration,
    Leaning,
    Decided,
    Deprecated,
    Unclassified,
}

impl StatusFilter {
    fn as_wire(self) -> &'static str {
        match self {
            StatusFilter::Exploration => "exploration",
            StatusFilter::Leaning => "leaning",
            StatusFilter::Decided => "decided",
            StatusFilter::Deprecated => "deprecated",
            StatusFilter::Unclassified => "unclassified",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    #[schemars(
        description = "What to look for. Natural language or literal identifiers both \
                              work; the query is matched lexically and semantically."
    )]
    pub query: String,
    #[schemars(
        description = "Project-relative path prefix filter, forward slashes (e.g. \"design/\")."
    )]
    pub path_prefix: Option<String>,
    #[schemars(
        description = "Lowercase language tag filter (\"csharp\", \"rust\", \"markdown\")."
    )]
    pub language: Option<String>,
    #[schemars(
        description = "Keep only chunks carrying one of these vault authority levels. \
                              Omit for no filter."
    )]
    pub status: Option<Vec<StatusFilter>>,
    #[schemars(
        description = "Maximum results; default 10, clamped to a sane ceiling. Results past \
                       the top few are pointer headers either way, so raising this buys \
                       breadth, not longer excerpts."
    )]
    pub limit: Option<u32>,
}

impl SearchParams {
    /// The wire request, scoped to the project this server resolved. `project`
    /// stays empty: the key is exact, and sending a display name alongside it
    /// would only invite a client to trust the weaker of the two.
    fn into_request(self, project_key: &str) -> SearchRequest {
        SearchRequest {
            project_key: Some(project_key.to_string()),
            ..SearchRequest::from(self)
        }
    }
}

impl From<SearchParams> for SearchRequest {
    fn from(params: SearchParams) -> Self {
        SearchRequest {
            query: params.query,
            project: None,
            project_key: None,
            path_prefix: params.path_prefix,
            language: params.language,
            status: params
                .status
                .unwrap_or_default()
                .into_iter()
                .map(|status| status.as_wire().to_string())
                .collect(),
            // Not exposed as a tool parameter: every corpus a v1 daemon can
            // hold is a repo, so a filter for it would be a knob with one
            // setting. It arrives with the session ledger at M3.
            sources: None,
            limit: params.limit,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExpandParams {
    #[schemars(
        description = "project_key exactly as it appeared in the search result. Prefer this \
                       over `project`: it identifies the source unambiguously."
    )]
    pub project_key: Option<String>,
    #[schemars(
        description = "Project display name, for hand-typed calls. Ignored when project_key \
                       is given, and ambiguous if two indexed projects share a name."
    )]
    pub project: Option<String>,
    #[schemars(
        description = "chunk_id from the search result you want to read. Search prints a \
                       shortened id; pass it exactly as shown - any prefix of at least 8 \
                       hexadecimal characters resolves, and an ambiguous one is answered with \
                       the candidates."
    )]
    pub chunk_id: String,
    #[schemars(
        description = "Extra lines of context on each side of the chunk. The daemon applies \
                       a default and a ceiling."
    )]
    pub context_lines: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NoParams {}

/// Environment variable that pins this server's project, by name or key.
///
/// Overrides working-directory resolution completely — it exists for the
/// arrangements where the server's cwd is not the workspace (a globally
/// configured MCP server, a launcher that starts it from `/`).
pub const PROJECT_ENV: &str = "LORE_PROJECT";

#[derive(Debug, Clone)]
pub struct LoreServer {
    client: DaemonClient,
    /// `LORE_PROJECT`, read once at construction. Held as a value rather than
    /// read at use so the override is a property of the server a test can set,
    /// not a property of the process a test has to mutate.
    pinned: Option<String>,
    /// Resolved lazily on the first tool call, then reused.
    ///
    /// Not resolved in `new`: the server is a long-lived stdio child that an
    /// editor may start before the daemon exists, and a constructor that
    /// needed a running daemon would turn "start them in the wrong order" into
    /// a dead session. Only *success* is cached, so the first call after the
    /// daemon appears still works.
    scope: Arc<OnceCell<ProjectInfo>>,
    tool_router: ToolRouter<Self>,
}

impl LoreServer {
    pub fn new(endpoint: Endpoint) -> Self {
        let pinned = std::env::var(PROJECT_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Self::pinned_to(endpoint, pinned)
    }

    /// [`Self::new`] with the `LORE_PROJECT` override supplied directly.
    pub fn pinned_to(endpoint: Endpoint, project: Option<String>) -> Self {
        Self {
            client: DaemonClient::new(endpoint),
            pinned: project,
            scope: Arc::new(OnceCell::new()),
            tool_router: Self::tool_router(),
        }
    }

    /// The one project every call in this process is scoped to.
    async fn scope(&self) -> Result<&ProjectInfo, DaemonError> {
        self.scope
            .get_or_try_init(|| async {
                match &self.pinned {
                    Some(pinned) => self.client.lookup(pinned).await,
                    None => {
                        let cwd = std::env::current_dir()
                            .map_err(|err| DaemonError::Unscoped(err.to_string()))
                            .and_then(|cwd| {
                                Utf8PathBuf::from_path_buf(cwd).map_err(|path| {
                                    DaemonError::Unscoped(format!(
                                        "the working directory is not valid UTF-8: {}",
                                        path.display()
                                    ))
                                })
                            })?;
                        self.client.resolve(&cwd).await.map_err(|err| match err {
                            // The daemon's remedy is the useful half; it is
                            // relayed inside a message that also names the
                            // override, which the daemon cannot know about.
                            DaemonError::Api {
                                status: 404,
                                message,
                            } => DaemonError::Unscoped(message),
                            other => other,
                        })
                    }
                }
            })
            .await
    }
}

#[tool_router]
impl LoreServer {
    #[tool(
        description = "Your first tool for any \"where is X / how does Y work / what was \
                       decided about Z\" question in an indexed project: one query replaces a \
                       chain of exploratory greps and directory reads, and also surfaces \
                       design-doc and decision context that grep cannot see. Prefer it \
                       whenever you do not already know the exact file; fall back to grep only \
                       for exhaustive literal sweeps (every occurrence of an exact string) - \
                       and note that inconsistently-named concepts defeat literal grep but not \
                       this search. \
                       Hybrid lexical+semantic search over this session's project - the code and \
                       design vault of the repository you are working in, scoped automatically, \
                       with no way to reach another project. Each hit carries provenance (file, \
                       line span, \
                       symbol path for code, heading path for Markdown), the status the \
                       document declares, and the authority Lore actually assigns it. Those \
                       differ on purpose: `decided` is honored only when the document cites a \
                       decision still active in the project's ledger, and scratch/research \
                       paths are capped whatever they declare - a demoted hit says why in its \
                       authority note. Prefer sources whose *effective* authority is `decided` \
                       when documents disagree; cited decision IDs are provenance, not \
                       authority. Excerpts are inline for the top few hits only and truncated \
                       even there; every later hit is a pointer (header lines, no excerpt). \
                       Call `expand` with a hit's project_key and chunk_id to read any of \
                       them - do not re-search hoping for longer excerpts."
    )]
    async fn search(&self, Parameters(params): Parameters<SearchParams>) -> CallToolResult {
        let scope = match self.scope().await {
            Ok(scope) => scope,
            Err(err) => return failed(&err),
        };
        let query = params.query.clone();
        match self.client.search(&params.into_request(&scope.key)).await {
            Ok(response) => text(render::search(&query, &response)),
            Err(err) => failed(&err),
        }
    }

    #[tool(
        description = "Read a search hit in full, plus surrounding context lines. This is the \
                       'now actually read it' step: search returns truncated excerpts, so do \
                       not quote or edit code from a search result without expanding it first."
    )]
    async fn expand(&self, Parameters(params): Parameters<ExpandParams>) -> CallToolResult {
        // The parameters stay: a hit carries its own `project_key`, and echoing
        // it back is the exact round trip `search` documents. They are only
        // *filled in* when the agent supplied neither, which is the common case
        // now that there is only ever one project to expand from.
        let mut project_key = params.project_key;
        if project_key.is_none() && params.project.is_none() {
            match self.scope().await {
                Ok(scope) => project_key = Some(scope.key.clone()),
                Err(err) => return failed(&err),
            }
        }
        let label = params
            .project
            .clone()
            .or_else(|| project_key.clone())
            .unwrap_or_default();
        let request = ExpandRequest {
            project: params.project.unwrap_or_default(),
            project_key,
            chunk_id: params.chunk_id,
            context_lines: params.context_lines,
        };
        match self.client.expand(&request).await {
            Ok(response) => text(render::expand(&label, &response)),
            Err(err) => failed(&err),
        }
    }

    #[tool(
        description = "Index health for this session's project: daemon version, index \
                       generation, the project's file/chunk/embedding coverage, and the \
                       embedding endpoint's state. Check this when search comes back empty, \
                       stale, or lexical-only - an unconfigured or unreachable embedding \
                       endpoint silently costs you every semantic match."
    )]
    async fn status(&self, Parameters(_): Parameters<NoParams>) -> CallToolResult {
        let scope = match self.scope().await {
            Ok(scope) => scope,
            Err(err) => return failed(&err),
        };
        match self.client.status(&scope.key).await {
            Ok(response) => text(render::status(&response)),
            Err(err) => failed(&err),
        }
    }
}

fn text(body: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(body)])
}

/// A dead daemon is a tool-level error, never a transport error and never a
/// panic: the request was well-formed and the agent can act on the answer (ask
/// the user to start it), so it belongs in the result, where the model sees it.
fn failed(err: &DaemonError) -> CallToolResult {
    tracing::warn!(error = %err, "daemon call failed");
    CallToolResult::error(vec![ContentBlock::text(err.to_string())])
}

// `router = self.tool_router` on purpose: the macro's default is
// `Self::tool_router()`, which rebuilds the whole router (and every JSON schema
// in it) on every `tools/list` and every `tools/call`. Pointing at the field
// built once in `new` costs nothing and leaves the field actually read.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for LoreServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("lore-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Lore indexes this project's code and design vault locally. Search it before \
                 answering questions about this codebase or about past design decisions, and \
                 prefer sources whose effective authority is `decided` over prose that merely \
                 sounds confident - a document declaring itself decided has not earned that \
                 unless Lore validated it against the project's decision ledger, and it will \
                 say so when it did not. Every tool here is scoped to the one project this \
                 session is in; other projects on this machine are not reachable and asking \
                 for them is not an option. Registering projects and forcing a reindex are \
                 deliberately not available either: ask the user to run `lore add <path>` or \
                 `lore index`.",
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_params_map_onto_the_wire_request_and_lower_status_filters() {
        let params = SearchParams {
            query: "chunk boundaries".into(),
            path_prefix: Some("design/".into()),
            language: Some("markdown".into()),
            status: Some(vec![StatusFilter::Decided, StatusFilter::Leaning]),
            limit: Some(5),
        };
        let request = params.into_request("lore-9a1c");

        assert_eq!(request.query, "chunk boundaries");
        // The scope is the server's, never the agent's: the key goes on the
        // wire and the ambiguous display name is left empty.
        assert_eq!(request.project_key.as_deref(), Some("lore-9a1c"));
        assert_eq!(request.project, None);
        assert_eq!(request.path_prefix.as_deref(), Some("design/"));
        assert_eq!(request.language.as_deref(), Some("markdown"));
        assert_eq!(request.status, vec!["decided", "leaning"]);
        assert_eq!(request.limit, Some(5));
    }

    #[test]
    fn absent_status_filter_is_an_empty_vec_not_a_null() {
        // The daemon reads `status: []` as "no filter"; sending null would be a
        // deserialization error on a `#[serde(default)] Vec<String>` field.
        let request: SearchRequest = SearchParams {
            query: "q".into(),
            path_prefix: None,
            language: None,
            status: None,
            limit: None,
        }
        .into();
        assert!(request.status.is_empty());
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["status"], serde_json::json!([]));
    }

    #[test]
    fn status_filter_wire_strings_match_the_documented_vocabulary() {
        let all = [
            StatusFilter::Exploration,
            StatusFilter::Leaning,
            StatusFilter::Decided,
            StatusFilter::Deprecated,
            StatusFilter::Unclassified,
        ];
        let wire: Vec<&str> = all.iter().map(|status| status.as_wire()).collect();
        assert_eq!(
            wire,
            [
                "exploration",
                "leaning",
                "decided",
                "deprecated",
                "unclassified"
            ]
        );
        // …and the same strings deserialize from an agent's arguments.
        for name in wire {
            let parsed: StatusFilter = serde_json::from_value(serde_json::json!(name))
                .unwrap_or_else(|err| panic!("status filter `{name}` should deserialize: {err}"));
            assert_eq!(parsed.as_wire(), name);
        }
    }

    #[test]
    fn all_three_tools_are_registered_and_registration_tools_are_not() {
        let router = LoreServer::tool_router();
        let mut names: Vec<String> = router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["expand", "search", "status"]);
    }

    #[test]
    fn tool_descriptions_steer_the_agent_rather_than_restating_the_name() {
        let router = LoreServer::tool_router();
        let described = |name: &str| -> String {
            router
                .list_all()
                .into_iter()
                .find(|tool| tool.name == name)
                .and_then(|tool| tool.description.clone())
                .unwrap_or_else(|| panic!("no description for {name}"))
                .to_string()
        };
        assert!(described("search").contains("effective* authority is `decided`"));
        // The declared/effective split has to reach the agent in the schema,
        // not only in the payload it is about to misread.
        assert!(described("search").contains("still active in the project's ledger"));
        assert!(described("expand").contains("truncated"));
        assert!(described("status").contains("lexical-only"));
    }

    #[test]
    fn a_dead_daemon_is_a_tool_error_carrying_the_remedy() {
        let result = failed(&DaemonError::NotRunning("no daemon handshake".into()));
        assert_eq!(result.is_error, Some(true));
        let rendered = result.content[0].as_text().unwrap().text.clone();
        assert!(rendered.contains("`lore daemon`"), "{rendered}");
    }
}
