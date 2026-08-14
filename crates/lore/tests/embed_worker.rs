//! The embed worker: fingerprint discipline, backlog draining, wake-ups.

mod daemon_support;
mod embed_support;

use std::sync::Arc;

use daemon_support::{Fixture, populate_standard_tree};
use embed_support::{POISON, Reply, Stub, settings, until};
use lore::daemon::index::full_scan;
use lore::embed::worker::Drained;
use lore::embed::{EmbedSettings, Embedder, fingerprint};
use lore::store::EmbeddingFingerprint;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct Rig {
    fixture: Fixture,
    stub: Stub,
    embedder: Embedder,
    notify: Arc<Notify>,
    cancel: CancellationToken,
}

impl Rig {
    async fn new(name: &str) -> Self {
        Self::with(name, |_| {}).await
    }

    async fn with(name: &str, tune: impl FnOnce(&mut EmbedSettings)) -> Self {
        let fixture = Fixture::new(name);
        let stub = Stub::start().await;
        let mut config = settings(&stub.base);
        tune(&mut config);
        Self {
            embedder: Embedder::from_settings(config),
            notify: Arc::new(Notify::new()),
            cancel: fixture.cancel.clone(),
            fixture,
            stub,
        }
    }

    fn worker(&self) -> lore::embed::EmbedWorker {
        self.embedder
            .worker(
                self.fixture.store.clone(),
                self.notify.clone(),
                self.cancel.clone(),
            )
            .expect("a configured embedder has a worker")
    }

    fn counts(&self) -> (u64, u64) {
        let status = self
            .fixture
            .store
            .blocking(|store| store.status())
            .expect("status");
        let project = status
            .projects
            .into_iter()
            .find(|p| p.project == self.fixture.project.id)
            .expect("project");
        (project.chunks, project.embedded_chunks)
    }

    fn stored_fingerprint(&self) -> Option<EmbeddingFingerprint> {
        self.fixture
            .store
            .blocking(|store| store.embedding_fingerprint())
            .expect("fingerprint")
    }
}

// ---------------------------------------------------------------------------
// Draining
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_backlog_drains_completely() {
    let rig = Rig::new("demo").await;
    populate_standard_tree(&rig.fixture);
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let (chunks, embedded) = rig.counts();
    assert!(chunks >= 3);
    assert_eq!(embedded, 0, "nothing is embedded before the worker runs");

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await, "stub is healthy");
    assert_eq!(worker.drain().await, Drained::Idle);

    assert_eq!(rig.counts(), (chunks, chunks), "every chunk has a vector");
    // Batching is real: 8 per batch, so several requests were made.
    assert!(rig.stub.state.attempts() >= 2);
    // A second pass finds nothing to do and sends nothing.
    let before = rig.stub.state.attempts();
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.stub.state.attempts(), before);
    rig.stub.shutdown().await;
}

/// The embedded text is the *prefixed* text (3.1), not the raw chunk: the
/// header is what makes a three-line method distinguishable from every other
/// three-line method in the repository.
#[tokio::test]
async fn chunks_are_embedded_with_their_provenance_header() {
    let rig = Rig::with("demo", |settings| {
        settings.document_prefix = "passage: ".into();
    })
    .await;
    rig.fixture
        .write("src/lib.rs", "pub fn alpha() -> u32 {\n    41\n}\n");
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    worker.probe().await;
    worker.drain().await;

    let sent = rig.stub.state.inputs();
    let body = sent
        .iter()
        .find(|input| input.contains("alpha"))
        .expect("the chunk was sent");
    assert!(body.starts_with("passage: rust src/lib.rs "), "{body:?}");
    assert!(body.contains("alpha"), "{body:?}");
    rig.stub.shutdown().await;
}

/// A chunk the endpoint refuses must not be retried forever, and must not
/// block the chunks queued behind it.
#[tokio::test]
async fn a_refused_chunk_is_skipped_and_the_rest_still_drain() {
    let rig = Rig::with("demo", |settings| {
        // One chunk per batch, so exactly the poison chunk is abandoned.
        settings.batch_max_items = 1;
    })
    .await;
    rig.fixture.write("docs/a.md", "# A\n\nFirst document.\n");
    rig.fixture
        .write("docs/b.md", format!("# B\n\n{POISON} document.\n"));
    rig.fixture.write("docs/c.md", "# C\n\nThird document.\n");
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let (chunks, _) = rig.counts();
    assert_eq!(chunks, 3);

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();

    // The rejection stops the pass and marks the endpoint unhealthy — a 4xx
    // is far more often a bad configuration than a bad chunk, and D-0007 says
    // that must be visible rather than silently poisoning the corpus.
    worker.probe().await;
    let mut passes = 0;
    while rig.counts().1 < 2 && passes < 10 {
        if !worker.probe().await {
            break;
        }
        worker.drain().await;
        passes += 1;
    }

    assert_eq!(rig.counts(), (3, 2), "two good chunks, one abandoned");
    assert_eq!(worker.skipped(), 1);
    // And it is genuinely abandoned: further passes do not re-send it.
    let before = rig.stub.state.attempts();
    worker.probe().await;
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.stub.state.attempts(), before + 1, "only the probe");
    rig.stub.shutdown().await;
}

#[tokio::test]
async fn a_transient_failure_leaves_the_backlog_for_later() {
    let rig = Rig::new("demo").await;
    rig.fixture.write("docs/a.md", "# A\n\nFirst document.\n");
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    worker.probe().await;

    // Exhaust exactly the retry budget with 5xx, leaving the stub healthy
    // again afterwards.
    rig.stub
        .state
        .script(std::iter::repeat_n(Reply::Status(500), 4));
    assert_eq!(worker.drain().await, Drained::Interrupted);
    assert_eq!(rig.counts().1, 0);
    assert!(!rig.embedder.health().is_ready(), "degraded, visibly");

    // Once the endpoint recovers the same chunk is embedded — nothing was
    // poisoned by a transient failure.
    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.counts().1, 1);
    assert_eq!(worker.skipped(), 0);
    rig.stub.shutdown().await;
}

// ---------------------------------------------------------------------------
// Fingerprint discipline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_changed_fingerprint_discards_every_stored_vector() {
    let rig = Rig::new("demo").await;
    populate_standard_tree(&rig.fixture);
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    worker.probe().await;
    worker.drain().await;
    let (chunks, embedded) = rig.counts();
    assert_eq!(embedded, chunks);
    assert_eq!(
        rig.stored_fingerprint().as_ref(),
        Some(&fingerprint(rig.embedder.client().unwrap().settings()))
    );

    // Same configuration ⇒ the vectors survive.
    worker.reconcile_fingerprint().await.unwrap();
    assert_eq!(rig.counts().1, embedded, "an unchanged space keeps vectors");

    // A different prefix is a different embedding space, so every vector goes.
    let mut changed = settings(&rig.stub.base);
    changed.query_prefix = "query: ".into();
    let other = Embedder::from_settings(changed);
    let moved = other
        .worker(
            rig.fixture.store.clone(),
            rig.notify.clone(),
            rig.cancel.clone(),
        )
        .unwrap();
    moved.reconcile_fingerprint().await.unwrap();

    assert_eq!(rig.counts(), (chunks, 0), "the old vectors are gone");
    let stored = rig
        .stored_fingerprint()
        .expect("a new identity was recorded");
    assert_eq!(stored.query_prefix, "query: ");
    assert_eq!(stored.normalization, "l2");
    rig.stub.shutdown().await;
}

/// With no endpoint there is no worker, so nothing may touch stored state —
/// including the fingerprint. A lexical-only daemon must be able to run over
/// a database that was previously embedded without destroying it.
#[tokio::test]
async fn an_unconfigured_daemon_leaves_the_store_alone() {
    let rig = Rig::new("demo").await;
    populate_standard_tree(&rig.fixture);
    full_scan(&rig.fixture.context(), &rig.fixture.project);
    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    worker.probe().await;
    worker.drain().await;
    let before = rig.counts();
    let recorded = rig.stored_fingerprint();
    assert!(before.1 > 0 && recorded.is_some());

    let lexical_only = Embedder::new(&lore::config::EmbeddingsConfig::default());
    assert!(
        lexical_only
            .worker(
                rig.fixture.store.clone(),
                rig.notify.clone(),
                rig.cancel.clone()
            )
            .is_none(),
        "no endpoint ⇒ no worker at all"
    );

    assert_eq!(rig.counts(), before);
    assert_eq!(rig.stored_fingerprint(), recorded);
    rig.stub.shutdown().await;
}

// ---------------------------------------------------------------------------
// The running task
// ---------------------------------------------------------------------------

/// The indexer's end-of-pass pulse is what makes new files searchable by
/// vector promptly; the fallback tick is a minute away, so anything observed
/// within a second here can only have come from the pulse.
#[tokio::test]
async fn the_indexer_pulse_wakes_the_worker_for_new_files() {
    let rig = Rig::new("demo").await;
    rig.fixture.write("docs/a.md", "# A\n\nFirst document.\n");

    // The worker shares the context's Notify, exactly as `daemon::run` wires
    // it — this is the seam under test, not a stand-in for it.
    let ctx = rig.fixture.context();
    let worker = rig
        .embedder
        .worker(
            rig.fixture.store.clone(),
            ctx.embed_notify.clone(),
            rig.cancel.clone(),
        )
        .unwrap();
    let task = tokio::spawn(worker.run());

    let project = rig.fixture.project.clone();
    let first = ctx.clone();
    tokio::task::spawn_blocking(move || full_scan(&first, &project))
        .await
        .unwrap();
    until("the first file to be embedded", || rig.counts().1 == 1).await;

    // A new file lands; the pass that indexes it pulses the worker.
    rig.fixture.write("docs/b.md", "# B\n\nSecond document.\n");
    let project = rig.fixture.project.clone();
    let second = ctx.clone();
    tokio::task::spawn_blocking(move || full_scan(&second, &project))
        .await
        .unwrap();
    until("the new file to be embedded", || rig.counts() == (2, 2)).await;

    rig.cancel.cancel();
    task.await.expect("the worker stops on cancellation");
    rig.stub.shutdown().await;
}

/// A dead endpoint must not turn into a hot loop, and must recover on its own
/// once the server comes back.
#[tokio::test]
async fn the_worker_waits_for_an_endpoint_that_is_not_there_yet() {
    let fixture = Fixture::new("demo");
    fixture.write("docs/a.md", "# A\n\nFirst document.\n");
    full_scan(&fixture.context(), &fixture.project);

    // Claim a port, then release it: nothing answers there yet.
    let held = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = held.local_addr().unwrap().port();
    drop(held);

    let mut config = settings(&format!("http://127.0.0.1:{port}/v1"));
    config.retry.max_attempts = 1;
    config.request_timeout = std::time::Duration::from_millis(300);
    let embedder = Embedder::from_settings(config);

    let cancel = fixture.cancel.clone();
    let worker = embedder
        .worker(
            fixture.store.clone(),
            Arc::new(Notify::new()),
            cancel.clone(),
        )
        .unwrap();
    let task = tokio::spawn(worker.run());

    until("health to report the endpoint as unreachable", || {
        matches!(
            embedder.status(),
            lore_core::EmbeddingStatus::Unreachable { .. }
        )
    })
    .await;
    // Still degraded, still no vectors, still running.
    assert!(!task.is_finished());

    cancel.cancel();
    task.await.expect("cancellation is prompt");
}
