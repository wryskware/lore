//! Golden-file MCP harness (milestone M0 debt).
//!
//! This drives the *real* protocol: a stub daemon answers canned `/v1` JSON, a
//! real `rmcp` client speaks JSON-RPC to a real `LoreServer` over an in-memory
//! duplex, and the snapshots capture exactly the bytes an agent would read.
//! Nothing here reaches into handler internals — the point is to notice when a
//! wire-contract change, an rmcp upgrade, or a rendering tweak silently changes
//! what the model sees. If a snapshot moves, that is the review.
//!
//! Coverage is deliberately three-legged: what the agent is offered
//! (`tools/list`), what it gets when things work, and what it gets when the
//! daemon is not there — the last being the case a proxy is most likely to get
//! wrong.

use std::sync::{Arc, Mutex};

use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::routing::{get, post};
use axum::{Json, Router};
use camino::Utf8PathBuf;
use lore_core::{
    DaemonStatus, EmbeddingStatus, ExpandResponse, ProjectInfo, ProjectStatus, SearchRequest,
    SearchResponse, SearchResult, WatchState,
};
use lore_mcp::{Endpoint, LoreServer};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Stub daemon
// ---------------------------------------------------------------------------

/// What the stub was asked for, in order.
///
/// Scoping is invisible in the rendered output an agent reads — the whole point
/// is that the agent never chooses it — so the only way to assert it is to
/// watch what actually went over the wire.
type Requests = Arc<Mutex<Vec<String>>>;

/// Canned `/v1` responses, built from the real `lore_core` types so a change to
/// the wire contract breaks this file at compile time rather than at snapshot
/// time.
async fn stub_daemon() -> String {
    stub_daemon_recording().await.0
}

async fn stub_daemon_recording() -> (String, Requests) {
    let seen: Requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/search",
            post({
                let seen = seen.clone();
                move |Json(request): Json<SearchRequest>| {
                    let seen = seen.clone();
                    async move {
                        seen.lock().unwrap().push(format!(
                            "search project_key={:?} project={:?}",
                            request.project_key, request.project
                        ));
                        Json(canned_search())
                    }
                }
            }),
        )
        .route("/v1/expand", post(|| async { Json(canned_expand()) }))
        .route("/v1/status", get(|| async { Json(canned_status()) }))
        // Every tool call resolves its project first now; without this route
        // the stub would answer nothing at all.
        .route("/v1/resolve", get(|| async { Json(canned_project()) }))
        .layer(middleware::from_fn({
            let seen = seen.clone();
            move |request: Request, next: Next| {
                let seen = seen.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(format!("{} {}", request.method(), request.uri()));
                    next.run(request).await
                }
            }
        }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), seen)
}

/// What `/v1/resolve` hands back: the project this session is standing in.
fn canned_project() -> ProjectInfo {
    ProjectInfo {
        id: 2,
        name: "lore".into(),
        key: "lore".into(),
        root: r"C:\Users\wrysk\wryskware\lore".into(),
        kind: "repo".into(),
    }
}

/// Three shapes the renderer treats differently, in one response: a vault hit
/// whose `decided` declaration Lore validated, a code hit with a symbol path
/// and a truncated excerpt, and — the case that matters most for the agent —
/// a hit whose declaration Lore *refused*, where declared and effective
/// authority disagree and the note says why.
fn canned_search() -> SearchResponse {
    SearchResponse {
        results: vec![
            SearchResult {
                chunk_id: "9f3a1c2b7e4d0123456789abcdef0123456789abcdef0123456789abcdef0123".into(),
                project: "lore".into(),
                project_key: "lore".into(),
                path: "design/4_Interfaces/4.1_MCP_Surface.md".into(),
                line_start: 15,
                line_end: 18,
                language: Some("markdown".into()),
                symbol_path: None,
                heading_path: Some(vec!["MCP Tool Surface".into(), "v0.1 tools".into()]),
                design_status: Some("decided".into()),
                effective_authority: Some("decided".into()),
                authority_note: None,
                decision_refs: vec!["D-0007".into(), "D-0008".into()],
                score: 0.87413,
                excerpt: "- **`search`** - one unified hybrid query. Filters: project, path \
                          glob,\n  language, vault status.\n"
                    .into(),
                excerpt_truncated: false,
            },
            SearchResult {
                chunk_id: "4e77ba0193ab0123456789abcdef0123456789abcdef0123456789abcdef0123".into(),
                project: "lexomancy".into(),
                project_key: "lexomancy".into(),
                path: "Assets/Scripts/Board.cs".into(),
                line_start: 120,
                line_end: 141,
                language: Some("csharp".into()),
                symbol_path: Some("Board.Update".into()),
                heading_path: None,
                design_status: None,
                effective_authority: Some("neutral".into()),
                authority_note: None,
                decision_refs: vec![],
                score: 0.61208,
                excerpt: "void Update()\n{\n    if (!_dirty) return;\n    Rebuild();".into(),
                excerpt_truncated: true,
            },
            SearchResult {
                chunk_id: "1c0ffee042ab0123456789abcdef0123456789abcdef0123456789abcdef0123".into(),
                project: "lore".into(),
                project_key: "lore".into(),
                path: "design/99_Scratch/2026-08-14_notes.md".into(),
                line_start: 3,
                line_end: 9,
                language: Some("markdown".into()),
                symbol_path: None,
                heading_path: Some(vec!["Ranking rewrite".into()]),
                design_status: Some("decided".into()),
                effective_authority: Some("deprecated".into()),
                authority_note: Some("99_Scratch path cap".into()),
                decision_refs: vec!["D-0007".into()],
                score: 0.41,
                excerpt: "Per D-0007 the daemon owns index state, so ranking should...".into(),
                excerpt_truncated: false,
            },
        ],
        lexical_only: false,
    }
}

fn canned_expand() -> ExpandResponse {
    ExpandResponse {
        path: "Assets/Scripts/Board.cs".into(),
        line_start: 118,
        line_end: 126,
        text: "    private bool _dirty;\n\n    void Update()\n    {\n        if (!_dirty) \
               return;\n        Rebuild();\n        _dirty = false;\n    }\n"
            .into(),
        file_lines: 412,
    }
}

/// Degraded on purpose: an unreachable embedding endpoint, a project with zero
/// embedded chunks, a project with refused authority declarations, and chunks
/// the endpoint would not embed — the states an agent most needs to be able to
/// name, all of which are invisible in search results themselves.
fn canned_status() -> DaemonStatus {
    DaemonStatus {
        api_version: 1,
        daemon_version: "0.1.0".into(),
        generation: 42,
        projects: vec![
            ProjectStatus {
                id: 1,
                name: "lexomancy".into(),
                key: "lexomancy".into(),
                root: r"C:\repos\Lexomancy".into(),
                kind: "repo".into(),
                files: 812,
                chunks: 9134,
                embedded_chunks: 0,
                authority_violations: 0,
                authority_violation_paths: Vec::new(),
                authority_profile: Some("lore-v1".into()),
                authority_behavior: Some("rank".into()),
                decisions_active: 14,
                decisions_total: 16,
                watch: WatchState::Armed,
                ..ProjectStatus::default()
            },
            ProjectStatus {
                id: 2,
                name: "lore".into(),
                key: "lore".into(),
                root: r"C:\Users\wrysk\wryskware\lore".into(),
                kind: "repo".into(),
                files: 96,
                chunks: 1204,
                embedded_chunks: 1204,
                authority_violations: 3,
                authority_violation_paths: vec![
                    "design/2_Memory/2.1_Memory_Model.md".into(),
                    "design/5_Implementation/5.1_Milestones.md".into(),
                ],
                authority_profile: Some("lore-v1".into()),
                authority_behavior: Some("rank".into()),
                decisions_active: 13,
                decisions_total: 13,
                watch: WatchState::Armed,
                ..ProjectStatus::default()
            },
        ],
        embeddings: EmbeddingStatus::Unreachable {
            endpoint: "http://127.0.0.1:11434".into(),
            error: "connection refused".into(),
        },
        latency: Vec::new(),
        embed_abandoned: 12,
    }
}

// ---------------------------------------------------------------------------
// MCP harness
// ---------------------------------------------------------------------------

/// Call one tool over a real client/server pair and return what the agent sees:
/// the rendered text, prefixed when the result is flagged as a tool error.
async fn call_tool(server: LoreServer, tool: &str, arguments: Value) -> String {
    let (server_side, client_side) = tokio::io::duplex(64 * 1024);
    // The server's `serve` blocks until the client sends `initialize`, so it
    // has to be running concurrently with the client's — awaiting them in
    // sequence deadlocks each side on the other.
    let serving = tokio::spawn(async move {
        let running = server.serve(server_side).await.expect("server handshake");
        running.waiting().await
    });

    let client = ().serve(client_side).await.expect("client handshake");
    let arguments = arguments
        .as_object()
        .cloned()
        .expect("tool arguments must be a JSON object");
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(arguments))
        .await
        .expect("tool call");

    let body = result
        .content
        .iter()
        .map(|block| {
            block
                .as_text()
                .map(|text| text.text.clone())
                .unwrap_or_else(|| "<non-text content block>".to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");

    client.cancel().await.expect("client shutdown");
    let _ = serving.await;

    match result.is_error {
        Some(true) => format!("TOOL ERROR\n{body}"),
        _ => body,
    }
}

async fn against_stub() -> LoreServer {
    LoreServer::new(Endpoint::Fixed(stub_daemon().await))
}

// ---------------------------------------------------------------------------
// Golden files
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tools_list_is_the_agent_facing_contract() {
    let (server_side, client_side) = tokio::io::duplex(64 * 1024);
    let serving = tokio::spawn(async move {
        // No daemon is ever contacted here: listing tools touches no route.
        let running = LoreServer::new(Endpoint::Fixed("http://127.0.0.1:1/v1".into()))
            .serve(server_side)
            .await
            .expect("server handshake");
        running.waiting().await
    });
    let client = ().serve(client_side).await.unwrap();

    let mut tools = client.list_all_tools().await.unwrap();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let rendered = serde_json::to_string_pretty(&json!({
        "instructions": client.peer_info().and_then(|info| info.instructions.clone()),
        "tools": tools,
    }))
    .unwrap();

    client.cancel().await.unwrap();
    let _ = serving.await;

    insta::assert_snapshot!("tools_list", rendered);
}

#[tokio::test]
async fn search_renders_vault_authority_and_a_truncated_code_hit() {
    let rendered = call_tool(
        against_stub().await,
        "search",
        json!({
            "query": "how does expand work",
            "status": ["decided", "leaning"],
            "limit": 5
        }),
    )
    .await;
    insta::assert_snapshot!("search_vault_and_code", rendered);
}

/// The agent cannot ask for a project, so the server has to supply one — and
/// it has to be the *key*, which identifies a source exactly where the display
/// name only usually does. Asserted on the wire rather than in the rendering,
/// because the agent never sees this happen.
#[tokio::test]
async fn every_tool_call_resolves_and_scopes_itself_without_being_asked() {
    let (base, seen) = stub_daemon_recording().await;
    let server = LoreServer::new(Endpoint::Fixed(base));

    call_tool(server.clone(), "search", json!({ "query": "anything" })).await;
    let requests = seen.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("GET /v1/resolve?path=")),
        "the server must resolve its own project: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r == r#"search project_key=Some("lore") project=None"#),
        "the resolved key must reach the wire: {requests:?}"
    );

    // Resolution is cached: a second call does not re-resolve, and `status`
    // asks for its own project rather than the machine-wide view.
    seen.lock().unwrap().clear();
    call_tool(server, "status", json!({})).await;
    let requests = seen.lock().unwrap().clone();
    assert!(
        !requests.iter().any(|r| r.contains("/v1/resolve")),
        "resolution should be cached per process: {requests:?}"
    );
    assert!(
        requests.iter().any(|r| r == "GET /v1/status?project=lore"),
        "status must be scoped, never machine-wide: {requests:?}"
    );
}

/// `LORE_PROJECT` replaces working-directory resolution entirely — the escape
/// hatch for a server whose cwd is not the workspace. It is normalized through
/// `status`, so a name and a key both land on the same project key.
#[tokio::test]
async fn a_pinned_project_replaces_working_directory_resolution() {
    let (base, seen) = stub_daemon_recording().await;
    let server = LoreServer::pinned_to(Endpoint::Fixed(base), Some("lexomancy".into()));

    call_tool(server, "search", json!({ "query": "anything" })).await;
    let requests = seen.lock().unwrap().clone();
    assert!(
        !requests.iter().any(|r| r.contains("/v1/resolve")),
        "a pinned project must not consult the working directory: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r == "GET /v1/status?project=lexomancy"),
        "the pin is normalized through status: {requests:?}"
    );
    // The stub's status lists lexomancy first, so its key is what scopes the
    // search — the pin named it, the daemon resolved it, the key travelled.
    assert!(
        requests
            .iter()
            .any(|r| r == r#"search project_key=Some("lexomancy") project=None"#),
        "{requests:?}"
    );
}

/// The `chunk_id` here is the *shortened* id the search snapshot prints, not
/// the full one the wire carried — that round trip is the whole point of
/// shortening, and the daemon resolves the prefix (this stub answers anything).
#[tokio::test]
async fn expand_returns_a_span_header_and_the_text() {
    let rendered = call_tool(
        against_stub().await,
        "expand",
        json!({
            "project_key": "lexomancy",
            "chunk_id": "4e77ba0193ab",
            "context_lines": 3
        }),
    )
    .await;
    insta::assert_snapshot!("expand_board_update", rendered);
}

#[tokio::test]
async fn status_surfaces_an_unreachable_embedding_endpoint() {
    let rendered = call_tool(against_stub().await, "status", json!({})).await;
    insta::assert_snapshot!("status_embeddings_unreachable", rendered);
}

#[tokio::test]
async fn a_missing_daemon_is_a_tool_error_that_tells_the_agent_what_to_ask_for() {
    let empty = tempfile::tempdir().unwrap();
    let data_dir = Utf8PathBuf::from_path_buf(empty.path().to_path_buf()).unwrap();
    let rendered = call_tool(
        LoreServer::new(Endpoint::DataDir(data_dir)),
        "search",
        json!({ "query": "anything" }),
    )
    .await;

    // The tempdir path is machine-specific; only the shape is golden.
    let redacted = rendered
        .split_once("(no daemon handshake at ")
        .map(|(head, tail)| {
            let rest = tail.split_once(')').map(|(_, rest)| rest).unwrap_or("");
            format!("{head}(no daemon handshake at <DATA_DIR>/daemon.json){rest}")
        })
        .unwrap_or(rendered);
    insta::assert_snapshot!("daemon_not_running", redacted);
}
