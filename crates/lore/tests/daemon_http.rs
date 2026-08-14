//! The `/v1` HTTP surface, driven through `tower::ServiceExt::oneshot` — the
//! real router, the real handlers, the real store, but no port and no
//! background tasks. Anything that binds a socket is testing tokio, not Lore.

mod daemon_support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use daemon_support::{Fixture, populate_standard_tree};
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::full_scan;
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch::{self, WatchCommand, WatchReceiver};
use lore::embed::Embedder;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

struct Harness {
    fixture: Fixture,
    router: Router,
    queue: IndexQueue,
    watch: WatchReceiver,
}

fn harness() -> Harness {
    let fixture = Fixture::new("demo");
    let queue = IndexQueue::new();
    let (watch_tx, watch) = watch::channel();
    let state = AppState {
        store: fixture.store.clone(),
        queue: queue.clone(),
        watch: watch_tx,
        config: Arc::new(Config::default()),
        // No embedding endpoint: this file covers the lexical-only daemon.
        // Hybrid ranking and health transitions live in `embed_search.rs`.
        embeddings: Embedder::disabled(),
        data_dir: fixture.data_dir.clone(),
    };
    Harness {
        router: router(state),
        fixture,
        queue,
        watch,
    }
}

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .expect("router never fails");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            panic!(
                "body was not JSON ({err}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, json)
}

async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
    call(router, "GET", uri, None).await
}

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    call(router, "POST", uri, Some(body)).await
}

// ---------------------------------------------------------------------------
// status / projects
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_reports_the_contract_fields() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["api_version"], lore_core::API_VERSION);
    assert_eq!(body["daemon_version"], env!("CARGO_PKG_VERSION"));
    assert!(body["generation"].as_u64().unwrap() >= 1);
    // No endpoint in config ⇒ the degradation is visible, not silent (D-0007).
    assert_eq!(body["embeddings"]["state"], "unconfigured");

    let project = &body["projects"][0];
    assert_eq!(project["name"], "demo");
    assert_eq!(project["root"], h.fixture.root.as_str());
    assert_eq!(project["files"], 3);
    assert!(project["chunks"].as_u64().unwrap() >= 3);
    assert_eq!(project["embedded_chunks"], 0);
}

#[tokio::test]
async fn registering_a_project_lists_it_watches_it_and_queues_a_scan() {
    let mut h = harness();
    let other = tempfile::tempdir().unwrap();
    let root = other.path().to_string_lossy().to_string();

    let (status, body) = post(&h.router, "/v1/projects", json!({ "root": root })).await;
    assert_eq!(status, StatusCode::OK);
    let id = body["id"].as_i64().unwrap();
    assert!(!body["name"].as_str().unwrap().is_empty());

    let (status, body) = get(&h.router, "/v1/projects").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"demo"), "{names:?}");
    assert_eq!(names.len(), 2);

    // Registration is not just a database row: the project is watched and
    // scheduled for an initial scan before the response is sent.
    match h.watch.try_recv().expect("a watch command was emitted") {
        WatchCommand::Watch(project) => assert_eq!(project.id, id),
    }
    let (queued, work) = h.queue.take().expect("an initial scan was queued");
    assert_eq!(queued, id);
    assert!(work.full);
}

#[tokio::test]
async fn an_explicit_name_overrides_the_directory_name() {
    let h = harness();
    let other = tempfile::tempdir().unwrap();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": other.path().to_string_lossy(), "name": "lexomancy" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "lexomancy");
}

#[tokio::test]
async fn registering_a_nonexistent_root_is_a_client_error() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": "C:/definitely/not/here/at/all" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["message"].is_string(), "ApiError shape: {body}");
}

#[tokio::test]
async fn registering_the_data_directory_is_refused() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": h.fixture.data_dir.as_str() }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("data directory"),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

#[tokio::test]
async fn index_queues_the_named_project_or_all_of_them() {
    let h = harness();

    let (status, body) = post(&h.router, "/v1/index", json!({ "project": "demo" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"][0]["name"], "demo");
    assert!(h.queue.take().is_some());

    let (status, body) = post(&h.router, "/v1/index", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"].as_array().unwrap().len(), 1);
    assert!(h.queue.take().is_some());

    let (status, body) = post(&h.router, "/v1/index", json!({ "project": "nope" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("nope"));
}

#[tokio::test]
async fn a_project_can_be_addressed_by_id_as_well_as_name() {
    let h = harness();
    let id = h.fixture.project.id.to_string();
    let (status, body) = post(&h.router, "/v1/index", json!({ "project": id })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"][0]["name"], "demo");
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_returns_ranked_lexical_results_with_provenance() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(&h.router, "/v1/search", json!({ "query": "daemon owns" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["lexical_only"], true,
        "no endpoint configured ⇒ visible degradation (D-0007)"
    );

    let results = body["results"].as_array().unwrap();
    assert!(!results.is_empty(), "expected a hit: {body}");
    let top = &results[0];
    assert_eq!(top["project"], "demo");
    assert_eq!(top["path"], "docs/design.md");
    assert_eq!(top["design_status"], "decided");
    assert_eq!(top["decision_refs"][0], "D-0007");
    assert!(top["heading_path"].is_array());
    assert!(top["chunk_id"].as_str().unwrap().len() >= 32);
    assert!(top["line_start"].as_u64().unwrap() >= 1);
    assert!(top["line_end"].as_u64().unwrap() >= top["line_start"].as_u64().unwrap());
    assert_eq!(top["excerpt_truncated"], false);
    assert!(top["excerpt"].as_str().unwrap().contains("daemon"));
    assert!(top["score"].as_f64().is_some());
}

#[tokio::test]
async fn code_hits_carry_a_symbol_path_and_a_language() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "alpha", "language": "rust" }),
    )
    .await;
    let top = &body["results"][0];
    assert_eq!(top["language"], "rust");
    assert_eq!(top["path"], "src/lib.rs");
    assert!(top["symbol_path"].is_string(), "{top}");
    assert!(top["heading_path"].is_null());
    assert!(top["design_status"].is_null());
}

#[tokio::test]
async fn search_filters_are_all_applied() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let hits = |body: &Value| body["results"].as_array().unwrap().len();

    let (_, unfiltered) = post(&h.router, "/v1/search", json!({ "query": "daemon" })).await;
    assert!(hits(&unfiltered) >= 1);

    // Path prefix.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "path_prefix": "src/" }),
    )
    .await;
    assert_eq!(hits(&body), 0, "no `daemon` under src/: {body}");

    // Language.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "language": "markdown" }),
    )
    .await;
    assert!(hits(&body) >= 1);
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "language": "csharp" }),
    )
    .await;
    assert_eq!(hits(&body), 0);

    // Vault status, including the "no frontmatter" atom.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "status": ["decided"] }),
    )
    .await;
    assert!(hits(&body) >= 1);
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "status": ["unclassified"] }),
    )
    .await;
    assert_eq!(hits(&body), 0, "the only match is a decided doc: {body}");

    // Limit.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "the a demo daemon project", "limit": 1 }),
    )
    .await;
    assert!(hits(&body) <= 1);
}

#[tokio::test]
async fn search_rejects_an_unknown_project_and_an_unknown_status() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "x", "project": "ghost" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("ghost"));

    let (status, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "x", "status": ["definitely-decided"] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("definitely-decided")
    );
}

#[tokio::test]
async fn a_query_with_no_usable_terms_is_empty_not_an_error() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(&h.router, "/v1/search", json!({ "query": ")))  ((" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// expand
// ---------------------------------------------------------------------------

/// `expand` reads the *file*, so context lines can come from outside the
/// chunk — that is the whole point of the endpoint.
#[tokio::test]
async fn expand_widens_a_chunk_with_surrounding_file_context() {
    let h = harness();
    let lines: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    h.fixture.write("notes.md", format!("# Heading\n\n{lines}"));
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "line 150" })).await;
    let hit = &search["results"][0];
    let chunk_id = hit["chunk_id"].as_str().unwrap().to_string();
    let (chunk_start, chunk_end) = (
        hit["line_start"].as_u64().unwrap(),
        hit["line_end"].as_u64().unwrap(),
    );

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": chunk_id, "context_lines": 5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["path"], "notes.md");
    assert_eq!(body["file_lines"], 202);

    let start = body["line_start"].as_u64().unwrap();
    let end = body["line_end"].as_u64().unwrap();
    assert!(start >= 1 && start <= chunk_start);
    assert!(end >= chunk_end);
    assert_eq!(
        body["text"].as_str().unwrap().lines().count() as u64,
        end - start + 1,
        "the returned span and the returned text must agree"
    );
}

#[tokio::test]
async fn expand_clamps_context_and_never_runs_off_the_file() {
    let h = harness();
    h.fixture
        .write("small.md", "# Title\n\nJust one paragraph.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "paragraph" })).await;
    let chunk_id = search["results"][0]["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": chunk_id, "context_lines": 100_000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["line_start"], 1);
    assert_eq!(body["line_end"], 3);
    assert_eq!(body["file_lines"], 3);
}

/// Index and disk can disagree — the file may be gone by the time an agent
/// follows up on a search result. Serving the stored chunk beats a 404 the
/// caller cannot act on.
#[tokio::test]
async fn expand_falls_back_to_stored_text_when_the_file_is_gone() {
    let h = harness();
    h.fixture
        .write("gone.md", "# Gone\n\nSoon to be deleted.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "deleted" })).await;
    let chunk_id = search["results"][0]["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();

    h.fixture.remove("gone.md");

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": chunk_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["text"].as_str().unwrap().contains("deleted"));
    assert_eq!(body["line_end"], body["file_lines"]);
}

#[tokio::test]
async fn expand_rejects_unknown_projects_and_unknown_chunks() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "ghost", "chunk_id": "abc" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("ghost"));

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": "0000000000000000" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("chunk"));
}

// ---------------------------------------------------------------------------
// Protocol-level behaviour
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_error_body_is_an_api_error_including_rejections() {
    let h = harness();

    // Unknown route.
    let (status, body) = get(&h.router, "/v1/nonsense").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].is_string(), "{body}");

    // Unversioned route: there is no unversioned surface.
    let (status, body) = get(&h.router, "/status").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].is_string(), "{body}");

    // Malformed JSON — axum's own rejection would be plain text.
    let response = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).expect("rejections are JSON too");
    assert!(body["message"].is_string(), "{body}");

    // Missing required field.
    let (status, body) = post(&h.router, "/v1/search", json!({ "limit": 5 })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["message"].is_string(), "{body}");
}

#[tokio::test]
async fn oversized_bodies_are_rejected_rather_than_buffered() {
    let h = harness();
    let huge = "x".repeat(2 * 1024 * 1024);
    let response = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "query": huge })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
