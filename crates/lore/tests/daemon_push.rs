//! The push receiving side (D-0015), driven through the real router with
//! `tower::ServiceExt::oneshot` — the same shape `daemon_http.rs` uses, and for
//! the same reason: anything that binds a socket is testing tokio, not Lore.
//!
//! Two anchors and a floor. The happy path proves the seam actually seams —
//! content that only ever existed in a push request comes back out of `search`,
//! having gone through the untouched apply pipeline. The takeover proves the
//! consistency property the epoch exists for: a session that was displaced
//! cannot publish, and what it staged is never applied.

mod daemon_support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use daemon_support::Fixture;
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::push::PushLeases;
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch;
use lore::embed::Embedder;
use lore_core::snapshot::{Manifest, ManifestEntry};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// Content that exists only inside a push request — never written to the
/// fixture's project root. If `search` finds it, it can only have arrived
/// through the staging area and the apply pipeline.
const PUSHED: &str = "pub fn chartreuse() -> u32 {\n    7\n}\n";
const PUSHED_PATH: &str = "src/pushed.rs";

struct Harness {
    fixture: Fixture,
    router: Router,
}

fn harness() -> Harness {
    harness_with(|fixture| fixture.push_leases())
}

/// The same harness with caller-chosen lease policy, for the floor test.
fn harness_with(leases: impl FnOnce(&Fixture) -> PushLeases) -> Harness {
    // Neutral: this file is about ingestion, not about authority semantics.
    let fixture = Fixture::neutral("demo");
    let (watch_tx, _watch_rx) = watch::channel();
    let push = leases(&fixture);
    let state = AppState {
        store: fixture.store.clone(),
        queue: IndexQueue::new(),
        watch: watch_tx,
        watch_status: watch::WatchStatus::new(),
        // The real thing: a push commit runs this pipeline, not a copy of it.
        index: fixture.context(),
        push,
        config: Arc::new(Config::default()),
        embeddings: Embedder::disabled(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        data_dir: fixture.data_dir.clone(),
        shutdown: fixture.cancel.clone(),
    };
    Harness {
        router: router(state),
        fixture,
    }
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
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

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    send(router, request).await
}

/// The upload route: identity in the query string, the file's bytes as the
/// body.
async fn upload(
    router: &Router,
    session: &str,
    epoch: u64,
    path: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(format!(
            "/v1/push/file?project=demo&session={session}&epoch={epoch}&path={path}"
        ))
        .body(Body::from(bytes.to_vec()))
        .unwrap();
    send(router, request).await
}

/// A one-file manifest for [`PUSHED`], hashed exactly as an observer would.
fn manifest() -> Manifest {
    Manifest::new(vec![ManifestEntry {
        path: PUSHED_PATH.into(),
        hash: blake3::hash(PUSHED.as_bytes()).to_hex().to_string(),
        size: PUSHED.len() as u64,
    }])
}

/// Take a lease, returning the session handle and epoch.
async fn lease(router: &Router) -> (String, u64) {
    let (status, body) = post(router, "/v1/push/lease", json!({ "project": "demo" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["session"].as_str().expect("a session handle").into(),
        body["epoch"].as_u64().expect("an epoch"),
    )
}

/// The whole flow, end to end: nothing about `PUSHED` ever touches the
/// project root, and `search` returns it anyway.
#[tokio::test]
async fn a_push_reaches_the_index_and_search_finds_it() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;

    // A handle is not a counter and not a clock.
    assert_eq!(session.len(), 32, "session handles are 16 random bytes");
    assert!(session.chars().all(|c| c.is_ascii_hexdigit()));

    let (status, body) = post(
        router,
        "/v1/push/manifest",
        json!({
            "project": "demo",
            "session": session,
            "epoch": epoch,
            "manifest": manifest(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["needed"], json!([PUSHED_PATH]));
    // Nothing was indexed before this push, so nothing is being deleted — the
    // guard's preview, answered before a byte of content is uploaded.
    assert_eq!(body["deletes"], 0);

    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["needed"], 0, "nothing left to upload");

    let (status, body) = post(
        router,
        "/v1/push/commit",
        json!({ "project": "demo", "session": session, "epoch": epoch }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["indexed"], 1);
    assert_eq!(body["deleted"], 0);
    assert!(body["generation"].as_u64().unwrap() > 0);

    assert_eq!(
        harness.fixture.indexed_paths(),
        vec![PUSHED_PATH.to_string()]
    );
    assert!(
        !harness.fixture.root.join(PUSHED_PATH).exists(),
        "the pushed file must never have been written to the project root"
    );

    let (status, body) = post(
        router,
        "/v1/search",
        json!({ "project": "demo", "query": "chartreuse" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["results"][0]["path"], PUSHED_PATH);

    // The lease survives its own commit — a pusher pushes again — but nothing
    // is staged any more.
    let (status, body) = send(
        router,
        Request::builder()
            .method("GET")
            .uri("/v1/status?project=demo")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["projects"][0]["push_lease_epoch"], epoch);
    assert_eq!(body["projects"][0].get("push_staged"), None);
}

/// A displaced session cannot publish, and what it staged is discarded rather
/// than applied. Sustained contention is meant to degrade to flapping between
/// whole snapshots — never to a mixture of two.
#[tokio::test]
async fn a_taken_over_session_cannot_commit_what_it_staged() {
    let harness = harness();
    let router = &harness.router;

    let (first, first_epoch) = lease(router).await;
    post(
        router,
        "/v1/push/manifest",
        json!({
            "project": "demo",
            "session": first,
            "epoch": first_epoch,
            "manifest": manifest(),
        }),
    )
    .await;
    let (status, _) = upload(router, &first, first_epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the first session staged its content"
    );

    // A second pusher arrives. Takeover is the decided policy: it does not
    // wait, and the epoch moves.
    let (second, second_epoch) = lease(router).await;
    assert!(
        second_epoch > first_epoch,
        "an acquire bumps the epoch: {second_epoch} vs {first_epoch}"
    );
    assert_ne!(second, first);

    for (route, body) in [
        (
            "/v1/push/manifest",
            json!({
                "project": "demo",
                "session": first,
                "epoch": first_epoch,
                "manifest": manifest(),
            }),
        ),
        (
            "/v1/push/commit",
            json!({ "project": "demo", "session": first, "epoch": first_epoch }),
        ),
    ] {
        let (status, answer) = post(router, route, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{route}: {answer}");
        let message = answer["message"].as_str().unwrap();
        assert!(
            message.contains("took over") && message.contains(&second_epoch.to_string()),
            "{route} must name the takeover and the current epoch: {message}"
        );
    }

    assert!(
        harness.fixture.indexed_paths().is_empty(),
        "a dead session's staged files must never be applied"
    );

    // The successor's own push works, from its own manifest.
    let (status, body) = post(
        router,
        "/v1/push/manifest",
        json!({
            "project": "demo",
            "session": second,
            "epoch": second_epoch,
            "manifest": manifest(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The displaced session's staging area went with it, so its successor
    // uploads the content itself rather than inheriting a byte of it.
    assert_eq!(body["needed"], json!([PUSHED_PATH]));
}

/// A client configured hotter than the receiver's floor is refused by name,
/// and told the number (D-0015: "a receiving daemon enforces a hard minimum
/// push interval and rejects clients configured hotter").
#[tokio::test]
async fn a_manifest_inside_the_minimum_push_interval_is_refused() {
    let harness = harness_with(|fixture| {
        PushLeases::new(
            &fixture.data_dir,
            Duration::from_secs(30),
            Duration::from_secs(600),
        )
    });
    let router = &harness.router;
    let (session, epoch) = lease(router).await;
    let body = json!({
        "project": "demo",
        "session": session,
        "epoch": epoch,
        "manifest": manifest(),
    });

    let (status, first) = post(router, "/v1/push/manifest", body.clone()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the first manifest sets the clock: {first}"
    );

    let (status, answer) = post(router, "/v1/push/manifest", body).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{answer}");
    let message = answer["message"].as_str().unwrap();
    assert!(
        message.contains("600s") && message.contains("min_interval_secs"),
        "the refusal states the floor and where it comes from: {message}"
    );
}
