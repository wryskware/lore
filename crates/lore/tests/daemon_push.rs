//! The push receiving side (D-0015), driven through the real router with
//! `tower::ServiceExt::oneshot` — the same shape `daemon_http.rs` uses, and for
//! the same reason: anything that binds a socket is testing tokio, not Lore.
//!
//! Two anchors and a floor. The happy path proves the seam actually seams —
//! content that only ever existed in a push request comes back out of `search`,
//! having gone through the untouched apply pipeline. The takeover proves the
//! consistency property the epoch exists for: a session that was displaced
//! cannot publish, and what it staged is never applied.
//!
//! Everything after those is a promise D-0015 makes to a pusher, tested as the
//! pusher experiences it: what an interrupted push has to re-upload, what
//! happens to bytes that disagree with the listing that negotiated them, what a
//! listing that lost its integrity is allowed to delete, and what a refused
//! commit costs. Where a property only exists in a window between two requests
//! — a commit in flight, a lease past its TTL, a daemon that restarted — the
//! window is held open through the same [`PushLeases`] the router serves rather
//! than raced against a timer, because a flaky test of a consistency property
//! is worse than no test of it.

mod daemon_support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use camino::Utf8PathBuf;
use daemon_support::Fixture;
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::{IndexContext, full_scan};
use lore::daemon::push::{self, PushClaim, PushLeases};
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch;
use lore::embed::Embedder;
use lore_core::snapshot::{Manifest, ManifestEntry, PushEpoch, PushSessionId};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Content that exists only inside a push request — never written to the
/// fixture's project root. If `search` finds it, it can only have arrived
/// through the staging area and the apply pipeline.
const PUSHED: &str = "pub fn chartreuse() -> u32 {\n    7\n}\n";
const PUSHED_PATH: &str = "src/pushed.rs";

struct Harness {
    fixture: Fixture,
    router: Router,
    /// The very leases the router serves, so a test can drive the reaper or
    /// occupy a commit slot without racing the routes for it.
    push: PushLeases,
    /// The very pipeline a commit applies through, so a local pass and a push
    /// share one guard record — which is what `status` reports.
    index: IndexContext,
}

fn harness() -> Harness {
    // Neutral: this file is about ingestion, not about authority semantics.
    harness_with(Fixture::neutral("demo"), |fixture| fixture.push_leases())
}

/// The same harness over a caller-supplied fixture and lease policy: the floor
/// and the TTL are the thing under test in two of these, and a restart is a
/// second daemon over the *same* directories.
fn harness_with(fixture: Fixture, leases: impl FnOnce(&Fixture) -> PushLeases) -> Harness {
    let (watch_tx, _watch_rx) = watch::channel();
    let push = leases(&fixture);
    // The real thing: a push commit runs this pipeline, not a copy of it.
    let index = fixture.context();
    let state = AppState {
        store: fixture.store.clone(),
        queue: IndexQueue::new(),
        watch: watch_tx,
        watch_status: watch::WatchStatus::new(),
        index: index.clone(),
        push: push.clone(),
        config: Arc::new(Config::default()),
        embeddings: Embedder::disabled(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        // No chunker plugins: this file is not about them, and an empty
        // registry routes exactly as no registry at all does.
        plugins: std::sync::Arc::new(lore::plugin::PluginRegistry::empty()),
        plugin_diagnostics: std::sync::Arc::new(Vec::new()),
        data_dir: fixture.data_dir.clone(),
        shutdown: fixture.cancel.clone(),
    };
    Harness {
        router: router(state),
        fixture,
        push,
        index,
    }
}

impl Harness {
    /// Where a session's uploads land. Keyed by project *and* epoch, so a test
    /// can watch one session's area survive or vanish independently of its
    /// successor's.
    fn staging_dir(&self, epoch: u64) -> Utf8PathBuf {
        self.fixture
            .data_dir
            .join(push::STAGING_DIR)
            .join(format!("{}-{epoch}", self.fixture.project.id))
    }

    /// The claim the routes build internally, for the two properties that are
    /// only reachable by holding push state while a request runs.
    fn claim(&self, session: &str, epoch: u64) -> PushClaim {
        PushClaim {
            project: self.fixture.project.id,
            name: self.fixture.project.name.clone(),
            session: PushSessionId(session.to_string()),
            epoch: PushEpoch(epoch),
        }
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

/// One manifest entry, hashed and measured exactly as an observer would.
fn entry(path: &str, body: &str) -> ManifestEntry {
    ManifestEntry {
        path: path.into(),
        hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
        size: body.len() as u64,
    }
}

/// A one-file manifest for [`PUSHED`].
fn manifest() -> Manifest {
    Manifest::new(vec![entry(PUSHED_PATH, PUSHED)])
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

/// Step 2: send a listing and learn what the daemon still needs.
async fn negotiate(
    router: &Router,
    session: &str,
    epoch: u64,
    manifest: &Manifest,
) -> (StatusCode, Value) {
    negotiate_allowing(router, session, epoch, manifest, false).await
}

/// The same, carrying the human's per-invocation mass-delete override.
async fn negotiate_allowing(
    router: &Router,
    session: &str,
    epoch: u64,
    manifest: &Manifest,
    allow_mass_delete: bool,
) -> (StatusCode, Value) {
    post(
        router,
        "/v1/push/manifest",
        json!({
            "project": "demo",
            "session": session,
            "epoch": epoch,
            "manifest": manifest,
            "allow_mass_delete": allow_mass_delete,
        }),
    )
    .await
}

/// Step 4: publish everything staged under this session.
async fn commit(router: &Router, session: &str, epoch: u64) -> (StatusCode, Value) {
    post(
        router,
        "/v1/push/commit",
        json!({ "project": "demo", "session": session, "epoch": epoch }),
    )
    .await
}

/// The one project's row from `GET /v1/status`.
async fn project_status(router: &Router) -> Value {
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
    body["projects"][0].clone()
}

/// How many files a staging area holds. Zero is the assertion that a refused
/// upload wrote nothing, which is different from "the commit ignored it".
fn staged_files(dir: &Utf8PathBuf) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| entries.count())
}

/// The error body every refusal carries.
fn message(body: &Value) -> &str {
    body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("a refusal names itself: {body}"))
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
    let harness = harness_with(Fixture::neutral("demo"), |fixture| {
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

// ---------------------------------------------------------------------------
// Negotiation and upload integrity
// ---------------------------------------------------------------------------

const OTHER_PATH: &str = "src/other.rs";
const OTHER: &str = "pub fn cerulean() -> u32 {\n    11\n}\n";
const OTHER_V2: &str = "pub fn cerulean() -> u32 {\n    12\n}\n";

/// An interrupted push resumes at file granularity: a second manifest for the
/// *same* session does not re-request content already staged under a hash that
/// still matches, and does re-request everything else.
///
/// D-0015: "hashes matching committed *or* already-staged content are skipped —
/// file-level resumption falls out for free". What that buys is measured in
/// what a crashed pusher has to re-upload, so the assertion is on `needed`.
#[tokio::test]
async fn an_interrupted_push_resumes_at_file_granularity() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;

    let first = Manifest::new(vec![entry(PUSHED_PATH, PUSHED), entry(OTHER_PATH, OTHER)]);
    let (status, body) = negotiate(router, &session, epoch, &first).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["needed"], json!([OTHER_PATH, PUSHED_PATH]));

    // The push is interrupted here: one of the two files made it.
    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["staged"], 1);
    assert_eq!(body["needed"], 1);

    // The pusher re-observes the project and starts over. `src/pushed.rs` is
    // unchanged and already staged, so it must not be asked for again.
    let second = Manifest::new(vec![
        entry(PUSHED_PATH, PUSHED),
        entry(OTHER_PATH, OTHER_V2),
    ]);
    let (status, body) = negotiate(router, &session, epoch, &second).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([OTHER_PATH]),
        "staged content whose hash still matches is not re-requested"
    );

    // ...and a hash that moved is asked for again, even though that path was
    // staged a moment ago: resumption is per *content*, not per path.
    let third = Manifest::new(vec![entry(PUSHED_PATH, OTHER), entry(OTHER_PATH, OTHER_V2)]);
    let (status, body) = negotiate(router, &session, epoch, &third).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([OTHER_PATH, PUSHED_PATH]),
        "a changed hash discards the staged content it no longer describes"
    );

    // The resumed push finishes normally.
    for (path, content) in [(PUSHED_PATH, OTHER), (OTHER_PATH, OTHER_V2)] {
        let (status, answer) = upload(router, &session, epoch, path, content.as_bytes()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["indexed"], 2);
    assert_eq!(
        harness.fixture.indexed_paths(),
        vec![OTHER_PATH.to_string(), PUSHED_PATH.to_string()]
    );
}

/// Bytes that disagree with the manifest entry that negotiated them are refused
/// by name, stage nothing, and leave the commit refusing as incomplete.
///
/// Both numbers are checked because the manifest carries both: the size is the
/// cheapest possible integrity check and the hash is the one that decides
/// whether this is the content the commit was negotiated over.
#[tokio::test]
async fn an_upload_that_disagrees_with_its_manifest_entry_is_refused_and_never_staged() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;
    let staging = harness.staging_dir(epoch);

    let (status, body) = negotiate(router, &session, epoch, &manifest()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["needed"], json!([PUSHED_PATH]));

    // Same length, different bytes: only the hash can catch this one.
    let mut tampered = PUSHED.as_bytes().to_vec();
    tampered[0] = b'x';
    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, &tampered).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let refusal = message(&body);
    assert!(
        refusal.contains(PUSHED_PATH) && refusal.contains("hashed"),
        "the refusal names the path and the disagreement: {refusal}"
    );

    // Truncated: caught by the size, before a hash is even computed.
    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, &tampered[..4]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let refusal = message(&body);
    assert!(
        refusal.contains("uploaded 4 bytes")
            && refusal.contains(&format!("declared {}", PUSHED.len())),
        "the refusal states both numbers: {refusal}"
    );

    assert_eq!(
        staged_files(&staging),
        0,
        "a refused upload writes nothing into the staging area"
    );

    // Absence is how this protocol spells deletion, so an unstaged needed path
    // must never reach a commit as one.
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let refusal = message(&body);
    assert!(
        refusal.contains("1 of this session's needed path(s)") && refusal.contains(PUSHED_PATH),
        "the commit names what is missing: {refusal}"
    );
    assert!(harness.fixture.indexed_paths().is_empty());

    // The session is not poisoned: the right bytes still finish the push.
    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(staged_files(&staging), 1);
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        harness.fixture.indexed_paths(),
        vec![PUSHED_PATH.to_string()]
    );
}

/// A manifest whose checksum does not describe its entries is refused before it
/// can influence anything — because its *absences* are deletions, and a listing
/// that did not survive the wire is a deletion instruction of unknown
/// provenance rather than a smaller listing.
#[tokio::test]
async fn a_manifest_whose_checksum_does_not_describe_it_is_refused_before_the_diff() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;

    // Give the project something to lose first.
    negotiate(router, &session, epoch, &manifest()).await;
    upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A listing that lost its only entry on the way, still carrying the
    // checksum of what it used to be. Honoured as-is it would delete the
    // project's one file.
    let mut corrupt = manifest();
    corrupt.entries.clear();
    assert!(!corrupt.is_intact());

    let (status, body) = negotiate(router, &session, epoch, &corrupt).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        message(&body).contains("checksum does not describe its entries"),
        "{body}"
    );
    assert_eq!(
        harness.fixture.indexed_paths(),
        vec![PUSHED_PATH.to_string()],
        "a refused manifest deletes nothing"
    );

    // Checked *first*: a corrupt manifest under a handle that was never minted
    // is still refused as corrupt, which is only possible if the checksum is
    // verified before the session, the floor and the diff.
    let (status, body) =
        negotiate(router, "0000000000000000deadbeefdeadbeef", epoch, &corrupt).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        message(&body).contains("checksum does not describe its entries"),
        "{body}"
    );

    // The session itself is unharmed: an intact manifest still negotiates, and
    // asks for nothing, because the committed content already matches.
    let (status, body) = negotiate(router, &session, epoch, &manifest()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["needed"], json!([]));
    assert_eq!(body["deletes"], 0);
}

// ---------------------------------------------------------------------------
// The lease's life: takeover, in-flight commits, expiry, restart
// ---------------------------------------------------------------------------

/// Takeover mid-upload. The displaced session's *uploads* are refused too — not
/// only its manifest and its commit — and the staging area it was filling goes
/// with the lease it belonged to, so its successor inherits not one byte.
#[tokio::test]
async fn a_takeover_mid_upload_refuses_the_displaced_session_and_deletes_its_staging() {
    let harness = harness();
    let router = &harness.router;
    let both = Manifest::new(vec![entry(PUSHED_PATH, PUSHED), entry(OTHER_PATH, OTHER)]);

    let (first, first_epoch) = lease(router).await;
    let staging = harness.staging_dir(first_epoch);
    let (status, body) = negotiate(router, &first, first_epoch, &both).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = upload(router, &first, first_epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"], 1,
        "one file still to go when it is displaced"
    );
    assert_eq!(staged_files(&staging), 1);

    // The successor arrives with the first session halfway through its uploads.
    let (second, second_epoch) = lease(router).await;
    assert!(second_epoch > first_epoch);
    assert!(
        !staging.exists(),
        "a displaced session's staging area is deleted with the lease it belonged to"
    );

    for (what, (status, body)) in [
        (
            "upload",
            upload(router, &first, first_epoch, OTHER_PATH, OTHER.as_bytes()).await,
        ),
        (
            "manifest",
            negotiate(router, &first, first_epoch, &both).await,
        ),
    ] {
        assert_eq!(status, StatusCode::CONFLICT, "{what}: {body}");
        let refusal = message(&body);
        assert!(
            refusal.contains("took over") && refusal.contains(&second_epoch.to_string()),
            "{what} must name the takeover and the current epoch: {refusal}"
        );
    }

    // The successor's own push is unaffected by any of it.
    let (status, body) = negotiate(router, &second, second_epoch, &both).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([OTHER_PATH, PUSHED_PATH]),
        "the successor uploads everything itself"
    );
    for (path, content) in [(PUSHED_PATH, PUSHED), (OTHER_PATH, OTHER)] {
        let (status, answer) =
            upload(router, &second, second_epoch, path, content.as_bytes()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }
    let (status, body) = commit(router, &second, second_epoch).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["indexed"], 2);
    assert_eq!(
        harness.fixture.indexed_paths(),
        vec![OTHER_PATH.to_string(), PUSHED_PATH.to_string()]
    );

    let (status, body) = post(
        router,
        "/v1/search",
        json!({ "project": "demo", "query": "chartreuse" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["results"][0]["path"], PUSHED_PATH);
}

/// Two commits for one project never overlap: the second is refused by name
/// while the first is in flight.
///
/// The apply deliberately runs with the lease lock released, so the window is
/// real; it is held open here by claiming the commit exactly as the route does,
/// which is the only way to observe it without racing a timer.
#[tokio::test]
async fn a_second_commit_while_one_is_in_flight_is_refused_by_name() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;
    negotiate(router, &session, epoch, &manifest()).await;
    upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;

    let claim = harness.claim(&session, epoch);
    let _in_flight = harness
        .push
        .take_commit(&claim)
        .expect("a staged, complete session commits");

    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        message(&body).contains("already in flight"),
        "the refusal names the reason rather than lumping it into `conflict`: {body}"
    );
    assert!(
        harness.fixture.indexed_paths().is_empty(),
        "the refused commit published nothing"
    );

    // The in-flight commit ends without publishing (as a refused apply does),
    // and the retry then goes through on the content already staged.
    harness.push.commit_finished(&claim, false);
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["indexed"], 1);
}

/// A lease that goes quiet past its TTL is reaped: its staging area leaves the
/// disk, `status` stops naming an epoch nobody holds, and the next acquirer
/// gets a higher one.
///
/// TTL zero rather than a slept-through TTL: "quiet longer than its TTL" is
/// then true the instant the reaper looks, so what is under test is the
/// reaping, not this machine's clock.
#[tokio::test]
async fn a_quiet_lease_is_reaped_and_its_staging_area_deleted() {
    let harness = harness_with(Fixture::neutral("demo"), |fixture| {
        PushLeases::new(&fixture.data_dir, Duration::ZERO, Duration::ZERO)
    });
    let router = &harness.router;
    let (session, epoch) = lease(router).await;
    let staging = harness.staging_dir(epoch);
    negotiate(router, &session, epoch, &manifest()).await;
    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(staged_files(&staging), 1);
    assert_eq!(project_status(router).await["push_lease_epoch"], epoch);

    let cancel = CancellationToken::new();
    let reaper = tokio::spawn(push::reap(harness.push.clone(), cancel.clone()));
    // The reaper ticks at half the TTL, floored at a second. Waiting for the
    // effect rather than for a duration keeps a slow machine slow, not flaky.
    for _ in 0..200 {
        if !staging.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cancel.cancel();
    reaper.await.expect("the reaper stops when cancelled");

    assert!(
        !staging.exists(),
        "an expired session's staged content must leave the disk"
    );
    let status_row = project_status(router).await;
    assert_eq!(
        status_row.get("push_lease_epoch"),
        None,
        "status stops naming an epoch nobody holds: {status_row}"
    );
    assert_eq!(status_row.get("push_staged"), None);
    assert!(
        harness.fixture.indexed_paths().is_empty(),
        "reaping discards staged content unapplied"
    );

    // The reaped session is gone, not merely quiet.
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(message(&body).contains("no push lease is held"), "{body}");

    let (_, next_epoch) = lease(router).await;
    assert!(
        next_epoch > epoch,
        "epochs are monotonic across reaping too: {next_epoch} vs {epoch}"
    );
}

/// Staged-but-uncommitted content does not survive a restart. Leases are
/// process state, so anything left in the staging area belongs to a session
/// that can never commit — and content nobody can commit must never be applied.
#[tokio::test]
async fn a_restart_discards_staged_content_no_lease_can_reach() {
    let harness = harness();
    let (session, epoch) = lease(&harness.router).await;
    let staging = harness.staging_dir(epoch);
    negotiate(&harness.router, &session, epoch, &manifest()).await;
    let (status, body) = upload(
        &harness.router,
        &session,
        epoch,
        PUSHED_PATH,
        PUSHED.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(staged_files(&staging), 1);

    // The daemon restarts over the same data directory and the same store.
    let Harness { fixture, .. } = harness;
    let restarted = harness_with(fixture, |fixture| fixture.push_leases());
    let staging_root = restarted.fixture.data_dir.join(push::STAGING_DIR);
    assert!(
        staging.exists(),
        "nothing has cleared it yet — the reset below is what must"
    );
    restarted.push.reset();

    assert!(!staging_root.exists(), "startup wipes every staging area");
    assert!(
        restarted.fixture.indexed_paths().is_empty(),
        "a restart applies nothing: staged content is inert, not pending"
    );

    let (status, body) = commit(&restarted.router, &session, epoch).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let refusal = message(&body);
    assert!(
        refusal.contains("no push lease is held") && refusal.contains("restart"),
        "the refusal tells a pusher what happened and what to do: {refusal}"
    );

    // A fresh lease over the same project starts clean, from a higher epoch:
    // the counter is persisted, so it survives what the leases do not.
    let (next_session, next_epoch) = lease(&restarted.router).await;
    assert!(next_epoch > epoch, "{next_epoch} vs {epoch}");
    let (status, body) = negotiate(&restarted.router, &next_session, next_epoch, &manifest()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([PUSHED_PATH]),
        "the new session inherits nothing from the discarded one"
    );
}

// ---------------------------------------------------------------------------
// The heartbeat
// ---------------------------------------------------------------------------

/// A renewal is proof of life: it resets the TTL that would otherwise have
/// reaped an idle lease.
///
/// The negative control is the same lease, the same TTL and no renewal, so what
/// is under test is the renewal rather than this machine's timing. The TTL is
/// short and the waits clear it by a third, in both directions.
#[tokio::test]
async fn a_renewal_keeps_an_idle_lease_alive_past_its_ttl() {
    const TTL: Duration = Duration::from_millis(300);
    const QUIET: Duration = Duration::from_millis(400);

    let harness = harness_with(Fixture::neutral("demo"), |fixture| {
        PushLeases::new(&fixture.data_dir, TTL, Duration::ZERO)
    });
    let router = &harness.router;
    let (session, epoch) = lease(router).await;
    let staging = harness.staging_dir(epoch);
    negotiate(router, &session, epoch, &manifest()).await;
    upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert!(
        harness.push.reap().is_empty(),
        "a lease that has just been used is not quiet"
    );

    // Idle right through the TTL, then a heartbeat — which is the one thing a
    // pusher with nothing to send is expected to do.
    tokio::time::sleep(QUIET).await;
    let (status, body) = post(
        router,
        "/v1/push/lease/renew",
        json!({ "project": "demo", "session": session, "epoch": epoch }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["session"],
        json!(session),
        "the renewal echoes the lease"
    );
    assert_eq!(body["epoch"], epoch);

    assert!(
        harness.push.reap().is_empty(),
        "the renewal reset the clock: this lease is not quiet any more"
    );
    assert_eq!(staged_files(&staging), 1, "nor was its staging touched");
    assert_eq!(project_status(router).await["push_lease_epoch"], epoch);

    // The same silence again, with no heartbeat this time.
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        harness.push.reap(),
        vec![staging.clone()],
        "an unheartbeaten lease is reaped after the same wait"
    );
}

/// A renewal names its epoch like every other push request, so a session that
/// was taken over cannot quietly resurrect itself by heartbeating — and a
/// renewal for a project nobody holds is refused rather than creating a lease.
#[tokio::test]
async fn a_renewal_from_a_taken_over_session_is_refused_by_name() {
    let harness = harness();
    let router = &harness.router;
    let renew = async |session: &str, epoch: u64| {
        post(
            router,
            "/v1/push/lease/renew",
            json!({ "project": "demo", "session": session, "epoch": epoch }),
        )
        .await
    };

    // Nobody holds this project's lease yet. A heartbeat is not an acquire.
    let (status, body) = renew("9f3a1c2b9f3a1c2b9f3a1c2b9f3a1c2b", 1).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(message(&body).contains("no push lease is held"), "{body}");

    let (first, first_epoch) = lease(router).await;
    let (second, second_epoch) = lease(router).await;
    assert!(second_epoch > first_epoch);

    let (status, body) = renew(&first, first_epoch).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let refusal = message(&body);
    assert!(
        refusal.contains("took over") && refusal.contains(&second_epoch.to_string()),
        "a displaced session learns why its heartbeat failed: {refusal}"
    );
    assert_eq!(
        project_status(router).await["push_lease_epoch"],
        second_epoch,
        "and the refused heartbeat changed nothing about who holds the lease"
    );

    // The holder's own heartbeat works, and re-advertises the cadences.
    let (status, body) = renew(&second, second_epoch).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ttl = body["ttl_secs"].as_u64().expect("a ttl");
    let heartbeat = body["heartbeat_secs"].as_u64().expect("a heartbeat");
    assert!(
        heartbeat > 0 && heartbeat < ttl,
        "the advertised renewal cadence sits comfortably inside the TTL: \
         {heartbeat}s of {ttl}s"
    );
}

// ---------------------------------------------------------------------------
// What a path in a request is allowed to be
// ---------------------------------------------------------------------------

/// A path is a *lookup key*, never a filesystem path.
///
/// Two halves of one property. A path the session's manifest never listed is
/// refused by name — a manifest is the complete listing, so an unlisted upload
/// is not a smaller push but an unnegotiated one. And whatever a client calls
/// a file, staged content is named by the hash the receiver verified rather
/// than by the string the client chose, so no path string ever reaches the
/// filesystem.
///
/// A traversal-shaped path can no longer even be *negotiated* — the manifest
/// backstop refuses the whole listing (see
/// [`a_structurally_impossible_path_refuses_the_whole_manifest`]). This test is
/// the layer beneath that one, and it holds whether or not the layer above
/// does: the upload route is still handed traversal strings here, and they
/// still go nowhere.
#[tokio::test]
async fn a_path_a_manifest_never_listed_is_refused_and_a_traversal_cannot_escape() {
    const SECOND: &str = "pub fn cerulean() -> u32 {\n    9\n}\n";
    const SECOND_PATH: &str = "src/second.rs";

    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;
    let staging = harness.staging_dir(epoch);
    let listing = Manifest::new(vec![entry(PUSHED_PATH, PUSHED), entry(SECOND_PATH, SECOND)]);
    let (status, body) = negotiate(router, &session, epoch, &listing).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for unlisted in [
        "src/never-listed.rs",
        "../escape.txt",
        "../../../escape2.txt",
    ] {
        let (status, body) = upload(router, &session, epoch, unlisted, PUSHED.as_bytes()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{unlisted}: {body}");
        let refusal = message(&body);
        assert!(
            refusal.contains(unlisted) && refusal.contains("not in this session's manifest"),
            "{unlisted}: {refusal}"
        );
    }

    let (status, body) = upload(router, &session, epoch, SECOND_PATH, SECOND.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let staged: Vec<String> = std::fs::read_dir(&staging)
        .expect("the staging area exists")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(staged.len(), 2, "both uploads staged: {staged:?}");
    for name in &staged {
        assert!(
            name.len() == 64 && name.chars().all(|c| c.is_ascii_hexdigit()),
            "staged files are named by their content hash, never by the \
             client's path string: {name}"
        );
    }

    // Nowhere `../escape.txt` could have pointed, at any level above the
    // staging directory it was uploaded into.
    let staging_root = harness.fixture.data_dir.join(push::STAGING_DIR);
    for dir in [&staging, &staging_root, &harness.fixture.data_dir] {
        assert!(
            !dir.join("escape.txt").exists(),
            "an upload escaped the staging area into {dir}"
        );
    }
    assert!(!harness.fixture.root.join("escape.txt").exists());
}

/// An epoch *above* the live lease cannot be a takeover — epochs are minted by
/// the daemon — so it is refused as the fabrication it is, under its own name.
///
/// The distinction is the point: a pusher told "you were taken over" re-acquires
/// and pushes again, which for a fabricated epoch would be an infinite retry of
/// the wrong remedy.
#[tokio::test]
async fn an_epoch_the_daemon_never_minted_is_refused_as_fabricated_not_as_a_takeover() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;
    let ahead = epoch + 1;

    for (route, (status, body)) in [
        (
            "manifest",
            negotiate(router, &session, ahead, &manifest()).await,
        ),
        ("commit", commit(router, &session, ahead).await),
        (
            "upload",
            upload(router, &session, ahead, PUSHED_PATH, PUSHED.as_bytes()).await,
        ),
    ] {
        assert_eq!(status, StatusCode::CONFLICT, "{route}: {body}");
        let refusal = message(&body);
        assert!(
            refusal.contains(&format!("push epoch {ahead} is ahead of"))
                && refusal.contains("never by a pusher"),
            "{route}: {refusal}"
        );
        assert!(
            !refusal.contains("took over"),
            "{route}: a fabricated epoch is not a takeover, and telling a \
             pusher it is sends it to re-acquire forever: {refusal}"
        );
    }

    // The lease is untouched by the attempt: its real epoch still pushes.
    assert_eq!(project_status(router).await["push_lease_epoch"], epoch);
    let (status, body) = negotiate(router, &session, epoch, &manifest()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["needed"], json!([PUSHED_PATH]));
}

// ---------------------------------------------------------------------------
// Deregistration
// ---------------------------------------------------------------------------

/// Deregistering a project takes its push state with it: the staged content of
/// a lease that can no longer commit anything goes immediately rather than
/// waiting for the reaper, and the epoch counter starts over with the new row.
///
/// `PushLeases::forget` is still both correct and necessary — it is what
/// deletes the staged bytes at once and stops `status` naming an epoch nobody
/// can commit. What it no longer has to defend against is a *successor*
/// inheriting the lease: schema v5 made `projects.id` `AUTOINCREMENT`, so a
/// deleted project's id is retired rather than handed to the next
/// registration, and the id-collision class is prevented in the store instead
/// of being cleaned up after in every map keyed by `ProjectId`. This test
/// asserts that too, because "the successor got a different id" is the premise
/// the rest of the reasoning now rests on.
#[tokio::test]
async fn deregistering_a_project_drops_the_push_state_bound_to_it() {
    let harness = harness();
    let router = &harness.router;
    let original_id = harness.fixture.project.id;
    let (session, epoch) = lease(router).await;
    let staging = harness.staging_dir(epoch);
    negotiate(router, &session, epoch, &manifest()).await;
    upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(staged_files(&staging), 1);

    let (status, body) = send(
        router,
        Request::builder()
            .method("DELETE")
            .uri("/v1/projects/demo")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !staging.exists(),
        "staged content for a project that no longer exists is discarded at once"
    );

    // Nothing to push to: identity is the registry binding (D-0016), and the
    // registry no longer has one.
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(message(&body).contains("unknown project `demo`"), "{body}");

    // The same name and root, registered again.
    let (status, body) = post(
        router,
        "/v1/projects",
        json!({ "root": harness.fixture.root.to_string(), "name": "demo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new_id = body["id"].as_i64().expect("a project id");
    assert!(
        new_id > original_id,
        "a re-registration is a new project, never the dead one's id handed \
         back: {original_id} -> {new_id}"
    );

    // The old session named a project id that no longer exists and can never
    // exist again, so the lease it named is gone rather than inherited.
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(message(&body).contains("no push lease is held"), "{body}");

    // A fresh lease starts the epoch counter over, because the counter is a
    // column on the project row and this is a new row.
    let (next_session, next_epoch) = lease(router).await;
    assert_ne!(next_session, session);
    assert_eq!(
        next_epoch, epoch,
        "a new project row carries a new epoch counter, not the old one"
    );
    let (status, body) = negotiate(router, &next_session, next_epoch, &manifest()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([PUSHED_PATH]),
        "and inherits nothing the discarded session staged"
    );
}

/// What the receiver still refuses, and what it deliberately no longer does.
///
/// D-0020 made the receiver **structural only**: it validates the shape of a
/// path, never its content. Two halves, in one test, because the line between
/// them is the decision:
///
/// - `.git/` is refused, structurally and wholesale. It is not an opinion about
///   the project — it is lore's own metadata floor, checked from the same
///   `GIT_DIR` constant the walker's hard floor uses.
/// - `.env` is **accepted**. That is the trusted-client model D-0015 always
///   described and D-0020 finished: what a manifest may otherwise contain is
///   the pusher's business, decided by the pusher's own ignore rules. The
///   accepted trade, stated in D-0020: a bad ignore file can ship a credential,
///   hygiene here is best-effort user responsibility, and an encrypted store is
///   the substantive data-protection measure. In shared deployments, isolation
///   comes from the instance boundary rather than from content policy.
#[tokio::test]
async fn the_receiver_refuses_git_metadata_structurally_and_accepts_content_it_dislikes() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;

    let secret = "[remote \"origin\"]\n    url = git@example.com:private.git\n";
    let sneaky = Manifest::new(vec![entry(".git/config", secret)]);
    let (status, body) = negotiate(router, &session, epoch, &sneaky).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let refusal = message(&body);
    assert!(
        refusal.contains(".git/config") && refusal.contains("`.git` component"),
        "the refusal names the path and the floor: {refusal}"
    );

    // Nothing was negotiated, so nothing can be committed and nothing is
    // indexed — a client sending that shape is not implementing the contract.
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(harness.fixture.indexed_paths().is_empty());

    // The other half: a credential is an ordinary path here. It is asked for,
    // uploaded and indexed like anything else.
    let (session, epoch) = lease(router).await;
    let env = "TOKEN=hunter2\n";
    let manifest = Manifest::new(vec![entry(".env", env), entry(PUSHED_PATH, PUSHED)]);
    let (status, body) = negotiate(router, &session, epoch, &manifest).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([".env", PUSHED_PATH]),
        "the receiver has no opinion about content: {body}"
    );
    upload(router, &session, epoch, ".env", env.as_bytes()).await;
    upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(commit(router, &session, epoch).await.0, StatusCode::OK);
    assert_eq!(harness.fixture.indexed_paths(), [".env", PUSHED_PATH]);
}

/// The other kind of bad entry, answered the same way — and now the only kind
/// there is. A path that could not be a manifest path at all means the *client*
/// is broken, so nothing it listed can be trusted — least of all the absences,
/// which delete files.
///
/// Refused whole, by name, before the project is even resolved. Nothing is
/// staged and nothing is negotiated, so a subsequent commit has no manifest.
#[tokio::test]
async fn a_structurally_impossible_path_refuses_the_whole_manifest() {
    let harness = harness();
    let router = &harness.router;
    let (session, epoch) = lease(router).await;

    for (path, expected) in [
        ("../../../etc/shadow", "contains a `..` component"),
        ("/etc/shadow", "is absolute"),
        ("C:/Windows/System32/config/SAM", "names a drive letter"),
        (r"src\windows.rs", "contains a backslash"),
        ("", "is empty"),
        ("src//doubled.rs", "contains an empty path component"),
    ] {
        // Alongside a perfectly good entry, so this is about the one bad path
        // and not about a listing that happened to be otherwise empty.
        let broken = Manifest::new(vec![entry(PUSHED_PATH, PUSHED), entry(path, "x")]);
        let (status, body) = negotiate(router, &session, epoch, &broken).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path:?}: {body}");
        assert!(message(&body).contains(expected), "{path:?}: {body}");
        assert!(
            message(&body).contains("refusing the whole listing"),
            "{path:?}: {body}"
        );
    }

    // Nothing was negotiated, so nothing can be committed.
    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        message(&body).contains("no manifest has been received"),
        "{body}"
    );
    assert!(harness.fixture.indexed_paths().is_empty());
}

// ---------------------------------------------------------------------------
// The mass-delete guard, on the push path
// ---------------------------------------------------------------------------

/// A manifest that omits most of a populated project is refused at the commit,
/// visibly, and retried with the per-invocation override *without re-uploading
/// a byte* — which is the whole reason a refused commit keeps its staging.
///
/// D-0015: a push deleting more than 50% and more than 100 files is rejected
/// absent an explicit per-invocation override, and a tripped guard is visible
/// in `status`.
#[tokio::test]
async fn a_manifest_that_drops_most_of_a_project_is_refused_until_the_override_retries_it() {
    let harness = harness();
    let router = &harness.router;
    let file = |i: usize| format!("pub fn f{i}() -> u32 {{\n    {i}\n}}\n");

    // Populate through the *local* path, so this test is about the guard rather
    // than about 120 uploads.
    for i in 0..120 {
        harness.fixture.write(&format!("src/f{i}.rs"), file(i));
    }
    full_scan(&harness.index, &harness.fixture.project);
    assert_eq!(harness.fixture.indexed_paths().len(), 120);
    let generation = harness.fixture.generation();

    // What a wrong root or a broken ignore rule looks like on the wire: ten of
    // the project's files, plus one genuinely new one.
    let mut kept: Vec<ManifestEntry> = (110..120)
        .map(|i| entry(&format!("src/f{i}.rs"), &file(i)))
        .collect();
    kept.push(entry(PUSHED_PATH, PUSHED));
    let shrunken = Manifest::new(kept);

    let (session, epoch) = lease(router).await;
    let staging = harness.staging_dir(epoch);
    let (status, body) = negotiate(router, &session, epoch, &shrunken).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([PUSHED_PATH]),
        "the ten survivors are already committed at these hashes"
    );
    assert_eq!(
        body["deletes"], 110,
        "the guard's preview, answered before a byte is uploaded"
    );

    let (status, body) = upload(router, &session, epoch, PUSHED_PATH, PUSHED.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let refusal = message(&body);
    assert!(
        refusal.contains("refused to drop 110 of 120 indexed file(s)")
            && refusal.contains("allow_mass_delete"),
        "the refusal states the numbers and the remedy: {refusal}"
    );

    assert_eq!(
        harness.fixture.indexed_paths().len(),
        120,
        "a refused apply writes nothing at all — neither the deletions nor the new file"
    );
    assert!(
        harness.fixture.generation() > generation,
        "the pass completed and was refused; a refusal is still an observation"
    );
    assert_eq!(
        staged_files(&staging),
        1,
        "the staging area survives a refused commit, or the retry would re-upload"
    );

    let row = project_status(router).await;
    assert_eq!(
        row["mass_delete_guard"],
        json!({ "deletes": 110, "stored": 120 }),
        "an index that stopped tracking its project says so: {row}"
    );
    assert_eq!(row["push_staged"], json!(true), "{row}");

    // The human decides, per invocation, and pays nothing for having been
    // stopped: the same listing with the override attached.
    let (status, body) = negotiate_allowing(router, &session, epoch, &shrunken, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed"],
        json!([]),
        "the retry re-uploads nothing: the staged content is still staged"
    );

    let (status, body) = commit(router, &session, epoch).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"], 110);
    assert_eq!(body["indexed"], 1);
    assert_eq!(harness.fixture.indexed_paths().len(), 11);

    let row = project_status(router).await;
    assert_eq!(
        row.get("mass_delete_guard"),
        None,
        "an apply that succeeded clears the trip: {row}"
    );
    assert_eq!(row.get("push_staged"), None, "{row}");
}
