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

use axum::routing::{get, post};
use axum::{Json, Router};
use camino::Utf8PathBuf;
use lore_core::{
    DaemonStatus, EmbeddingStatus, ExpandResponse, ProjectStatus, SearchResponse, SearchResult,
    WatchState,
};
use lore_mcp::{Endpoint, LoreServer};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Stub daemon
// ---------------------------------------------------------------------------

/// Canned `/v1` responses, built from the real `lore_core` types so a change to
/// the wire contract breaks this file at compile time rather than at snapshot
/// time.
async fn stub_daemon() -> String {
    let app = Router::new()
        .route("/v1/search", post(|| async { Json(canned_search()) }))
        .route("/v1/expand", post(|| async { Json(canned_expand()) }))
        .route("/v1/status", get(|| async { Json(canned_status()) }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

/// One vault hit (authority + heading path + cited decisions) and one code hit
/// (symbol path + a truncated excerpt) — the two shapes the renderer treats
/// differently, in one response.
fn canned_search() -> SearchResponse {
    SearchResponse {
        results: vec![
            SearchResult {
                chunk_id: "9f3a1c2b7e".into(),
                project: "lore".into(),
                path: "design/4_Interfaces/4.1_MCP_Surface.md".into(),
                line_start: 15,
                line_end: 18,
                language: Some("markdown".into()),
                symbol_path: None,
                heading_path: Some(vec!["MCP Tool Surface".into(), "v0.1 tools".into()]),
                design_status: Some("decided".into()),
                decision_refs: vec!["D-0007".into(), "D-0008".into()],
                score: 0.87413,
                excerpt: "- **`search`** - one unified hybrid query. Filters: project, path \
                          glob,\n  language, vault status.\n"
                    .into(),
                excerpt_truncated: false,
            },
            SearchResult {
                chunk_id: "4e77ba0193".into(),
                project: "lexomancy".into(),
                path: "Assets/Scripts/Board.cs".into(),
                line_start: 120,
                line_end: 141,
                language: Some("csharp".into()),
                symbol_path: Some("Board.Update".into()),
                heading_path: None,
                design_status: None,
                decision_refs: vec![],
                score: 0.61208,
                excerpt: "void Update()\n{\n    if (!_dirty) return;\n    Rebuild();".into(),
                excerpt_truncated: true,
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

/// Degraded on purpose: an unreachable embedding endpoint plus a project with
/// zero embedded chunks is the state an agent most needs to be able to name.
fn canned_status() -> DaemonStatus {
    DaemonStatus {
        api_version: 1,
        daemon_version: "0.1.0".into(),
        generation: 42,
        projects: vec![
            ProjectStatus {
                id: 1,
                name: "lexomancy".into(),
                root: r"C:\repos\Lexomancy".into(),
                files: 812,
                chunks: 9134,
                embedded_chunks: 0,
                watch: WatchState::Armed,
            },
            ProjectStatus {
                id: 2,
                name: "lore".into(),
                root: r"C:\Users\wrysk\wryskware\lore".into(),
                files: 96,
                chunks: 1204,
                embedded_chunks: 1204,
                watch: WatchState::Armed,
            },
        ],
        embeddings: EmbeddingStatus::Unreachable {
            endpoint: "http://127.0.0.1:11434".into(),
            error: "connection refused".into(),
        },
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
            "project": "lore",
            "status": ["decided", "leaning"],
            "limit": 5
        }),
    )
    .await;
    insta::assert_snapshot!("search_vault_and_code", rendered);
}

#[tokio::test]
async fn expand_returns_a_span_header_and_the_text() {
    let rendered = call_tool(
        against_stub().await,
        "expand",
        json!({
            "project": "lexomancy",
            "chunk_id": "4e77ba0193",
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
