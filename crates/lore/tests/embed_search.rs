//! Hybrid search end to end: the real router, the real store, the real
//! fusion, and a stub embedding endpoint.
//!
//! The corpus is engineered so the two arms genuinely disagree. `docs/two.md`
//! shares **no searchable word** with the query and is therefore invisible to
//! BM25; the stub's synonym-group model puts it top of the vector list. That
//! is the only honest way to show the vector arm contributed something rather
//! than merely agreeing with BM25.

mod daemon_support;
mod embed_support;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use daemon_support::Fixture;
use embed_support::{Stub, settings};
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::full_scan;
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch;
use lore::embed::Embedder;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A query with two ordinary words. `docs/one.md` contains both; nothing else
/// contains either.
const QUERY: &str = "results ordering";

struct Harness {
    fixture: Fixture,
    router: Router,
    embedder: Embedder,
}

fn harness(embedder: Embedder) -> Harness {
    let fixture = Fixture::new("demo");
    let (watch_tx, _watch_rx) = watch::channel();
    let state = AppState {
        store: fixture.store.clone(),
        queue: IndexQueue::new(),
        watch: watch_tx,
        watch_status: watch::WatchStatus::new(),
        config: Arc::new(Config::default()),
        embeddings: embedder.clone(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        data_dir: fixture.data_dir.clone(),
    };
    // The receiver is dropped; nothing under test watches the filesystem.
    Harness {
        router: router(state),
        fixture,
        embedder,
    }
}

/// Three single-section documents, chosen for what they do *not* share.
fn populate(fixture: &Fixture) {
    // Lexically on target: contains both query words.
    fixture.write(
        "docs/one.md",
        "# One\n\nThe JSON response lists results; ordering is stable.\n",
    );
    // Semantically on target, lexically invisible: no query word appears.
    fixture.write(
        "docs/two.md",
        "# Two\n\nReciprocal rank fusion combines the lexical and vector arms.\n",
    );
    // Neither.
    fixture.write(
        "docs/three.md",
        "# Three\n\nThe watcher debounces filesystem events before reindexing.\n",
    );
    full_scan(&fixture.context(), &fixture.project);
}

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("router never fails");
    decode(response).await
}

async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .expect("router never fails");
    decode(response).await
}

async fn decode(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}

fn paths(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|hit| hit["path"].as_str().unwrap().to_string())
        .collect()
}

/// Embed the whole corpus through the worker, so the store holds real vectors.
async fn embed_everything(h: &Harness) {
    let mut worker = h
        .embedder
        .worker(
            h.fixture.store.clone(),
            Arc::new(tokio::sync::Notify::new()),
            h.fixture.cancel.clone(),
        )
        .expect("a configured embedder");
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await, "the stub is healthy");
    worker.drain().await;
}

// ---------------------------------------------------------------------------
// Hybrid ranking
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_semantically_close_chunk_is_found_that_bm25_cannot_see() {
    let stub = Stub::start().await;
    let h = harness(Embedder::from_settings(settings(&stub.base)));
    populate(&h.fixture);

    // Control: with no vectors in the store, BM25 is the whole story.
    let (status, body) = post(&h.router, "/v1/search", json!({ "query": QUERY })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lexical_only"], true, "nothing is embedded yet");
    assert_eq!(paths(&body), ["docs/one.md"]);

    embed_everything(&h).await;

    let (status, body) = post(&h.router, "/v1/search", json!({ "query": QUERY })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lexical_only"], false, "the vector arm participated");
    // `two.md` was unreachable by BM25 and is now second: RRF puts the
    // document both arms found first, then the vector arm's own top hit.
    assert_eq!(
        paths(&body),
        ["docs/one.md", "docs/two.md", "docs/three.md"],
        "{body:#}"
    );
    // Scores are fused, descending, and comparable only within the response.
    let scores: Vec<f64> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["score"].as_f64().unwrap())
        .collect();
    assert!(scores[0] > scores[1] && scores[1] > scores[2], "{scores:?}");

    stub.shutdown().await;
}

/// Filters are pushed into SQL for both arms, so a scope with no vectors is
/// honestly reported as lexical-only rather than pretending vectors ran.
#[tokio::test]
async fn filters_apply_to_both_arms() {
    let stub = Stub::start().await;
    let h = harness(Embedder::from_settings(settings(&stub.base)));
    populate(&h.fixture);
    embed_everything(&h).await;

    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": QUERY, "path_prefix": "docs/two" }),
    )
    .await;
    assert_eq!(paths(&body), ["docs/two.md"], "{body:#}");
    assert_eq!(body["lexical_only"], false);

    // A scope containing nothing at all: still a 200, still no vectors.
    let (status, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": QUERY, "language": "csharp" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(paths(&body).is_empty());
    assert_eq!(body["lexical_only"], true, "no vectors in scope");

    stub.shutdown().await;
}

/// D-0007's core promise: losing the endpoint costs recall, never results.
#[tokio::test]
async fn an_endpoint_that_dies_degrades_the_next_request_to_lexical_only() {
    let stub = Stub::start().await;
    let mut config = settings(&stub.base);
    // Keep the failing request short; a dead loopback port does not always
    // refuse promptly on Windows.
    config.retry.max_attempts = 1;
    config.request_timeout = std::time::Duration::from_millis(300);

    let h = harness(Embedder::from_settings(config));
    populate(&h.fixture);
    embed_everything(&h).await;

    let (_, body) = post(&h.router, "/v1/search", json!({ "query": QUERY })).await;
    assert_eq!(body["lexical_only"], false);

    stub.shutdown().await;

    // Same query, same corpus, same stored vectors — but the query can no
    // longer be embedded, so the vector arm cannot run.
    let (status, body) = post(&h.router, "/v1/search", json!({ "query": QUERY })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lexical_only"], true, "{body:#}");
    assert_eq!(paths(&body), ["docs/one.md"]);

    // And the failure is visible, not silent.
    let (_, status_body) = get(&h.router, "/v1/status").await;
    assert_eq!(status_body["embeddings"]["state"], "unreachable");
    assert!(
        status_body["embeddings"]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
}

/// The vault-authority modifier applies to the fused ranking, so a *validated*
/// `decided` document outranks an equally-relevant exploration one — and an
/// unvalidated one does not. Same corpus, same query, same relevance: only the
/// authority Lore assigns can separate these.
#[tokio::test]
async fn vault_authority_breaks_ties_in_the_fused_ranking() {
    let stub = Stub::start().await;
    let h = harness(Embedder::from_settings(settings(&stub.base)));
    let body = "Ranking of results depends on ordering.";
    h.fixture.write(
        "design/0_Canon/DECISIONS.md",
        "# Ledger\n\n## D-0007 — Interfaces\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    h.fixture.write(
        "docs/explore.md",
        format!("---\ndesign_status: exploration\n---\n\n# Explore\n\n{body}\n"),
    );
    h.fixture.write(
        "docs/settled.md",
        format!(
            "---\ndesign_status: decided\ndecision_refs: [D-0007]\n---\n\n# Settled\n\n{body}\n"
        ),
    );
    // Declares the same thing, cites nothing: the declaration is refused and
    // it ranks with plain exploration material, not above it.
    h.fixture.write(
        "docs/claimed.md",
        format!("---\ndesign_status: decided\n---\n\n# Claimed\n\n{body}\n"),
    );
    h.fixture.write(
        "docs/old.md",
        format!("---\ndesign_status: deprecated\n---\n\n# Old\n\n{body}\n"),
    );
    full_scan(&h.fixture.context(), &h.fixture.project);
    embed_everything(&h).await;

    let (_, response) = post(&h.router, "/v1/search", json!({ "query": QUERY })).await;
    // The ledger itself is indexed like anything else and can match; only the
    // four documents under test are ordered here.
    let ranked: Vec<String> = paths(&response)
        .into_iter()
        .filter(|path| path.starts_with("docs/"))
        .collect();
    assert_eq!(
        ranked,
        [
            "docs/settled.md",
            "docs/explore.md",
            "docs/claimed.md",
            "docs/old.md"
        ],
        "{response:#}"
    );

    // The refusal is on the result, not only in the ranking.
    let claimed = response["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "docs/claimed.md")
        .expect("the demoted document is still a result");
    assert_eq!(claimed["design_status"], "decided", "declaration preserved");
    assert_eq!(claimed["effective_authority"], "neutral");
    assert_eq!(
        claimed["authority_note"],
        "decided declared but cites no active decision"
    );

    // …and `status` counts it, so a human sees it without running a query.
    let (_, status_body) = get(&h.router, "/v1/status").await;
    assert_eq!(status_body["projects"][0]["authority_violations"], 1);
    assert_eq!(
        status_body["projects"][0]["authority_violation_paths"][0],
        "docs/claimed.md"
    );
    stub.shutdown().await;
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_reflects_every_embedding_state() {
    // Unconfigured: no endpoint at all.
    let h = harness(Embedder::disabled());
    let (_, body) = get(&h.router, "/v1/status").await;
    assert_eq!(body["embeddings"]["state"], "unconfigured");
    assert!(body["embeddings"]["endpoint"].is_null());

    // Configured but not yet probed: degraded, never optimistic.
    let stub = Stub::start().await;
    let mut config = settings(&stub.base);
    // The last phase probes a port nobody is listening on; keep it brief.
    config.retry.max_attempts = 1;
    config.request_timeout = std::time::Duration::from_millis(300);
    let h = harness(Embedder::from_settings(config));
    let (_, body) = get(&h.router, "/v1/status").await;
    assert_eq!(body["embeddings"]["state"], "unreachable");

    // Ready after a successful probe.
    assert!(h.embedder.refresh().await);
    let (_, body) = get(&h.router, "/v1/status").await;
    assert_eq!(body["embeddings"]["state"], "ready");
    assert_eq!(body["embeddings"]["endpoint"], stub.base);
    assert_eq!(body["embeddings"]["model"], "stub-model");

    // Unreachable again once the server goes away.
    stub.shutdown().await;
    assert!(!h.embedder.refresh().await);
    let (_, body) = get(&h.router, "/v1/status").await;
    assert_eq!(body["embeddings"]["state"], "unreachable");
}

/// `embedded_chunks` is how a user checks the backlog is draining, so it has
/// to move with reality rather than with configuration.
#[tokio::test]
async fn status_counts_embedded_chunks_as_they_land() {
    let stub = Stub::start().await;
    let h = harness(Embedder::from_settings(settings(&stub.base)));
    populate(&h.fixture);

    let (_, body) = get(&h.router, "/v1/status").await;
    let chunks = body["projects"][0]["chunks"].as_u64().unwrap();
    assert_eq!(body["projects"][0]["embedded_chunks"], 0);

    embed_everything(&h).await;

    let (_, body) = get(&h.router, "/v1/status").await;
    assert_eq!(body["projects"][0]["embedded_chunks"], chunks);
    stub.shutdown().await;
}
