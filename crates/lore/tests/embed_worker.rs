//! The embed worker: fingerprint discipline, backlog draining, wake-ups.

mod daemon_support;
mod embed_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use daemon_support::{Fixture, populate_standard_tree};
use embed_support::{DIMS, POISON, Reply, Stub, settings, until};
use lore::daemon::index::full_scan;
use lore::embed::worker::Drained;
use lore::embed::{EmbedSettings, Embedder, fingerprint_of};
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
/// block the chunks queued behind it. It does get exactly one retry first —
/// see [`a_refused_chunk_is_retried_once_before_it_is_abandoned`] — which the
/// long retry window here keeps out of the way.
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
    // Short enough that the retry lands inside this test, long enough that it
    // cannot land in the middle of a pass and confuse the counts below.
    worker.set_poison_windows(Duration::from_millis(20), Duration::from_secs(600));
    worker.reconcile_fingerprint().await.unwrap();

    // The rejection stops the pass and marks the endpoint unhealthy — a 4xx
    // is far more often a bad configuration than a bad chunk, and D-0007 says
    // that must be visible rather than silently poisoning the corpus.
    worker.probe().await;
    let mut passes = 0;
    while (rig.counts().1 < 2 || worker.skipped() < 1) && passes < 10 {
        if !worker.probe().await {
            break;
        }
        worker.drain().await;
        tokio::time::sleep(Duration::from_millis(25)).await;
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

/// Poison is filtered *after* the store query, so a fetch page that filters
/// away entirely used to read as "backlog drained" and everything behind it
/// starved forever while status stayed Ready. The page size is shrunk here so
/// the boundary is reachable without seeding thousands of rows; the mechanism
/// under test is the same one production uses at [`PAGE_ROWS`].
#[tokio::test]
async fn poisoned_fetch_window_does_not_report_idle() {
    const PAGE: usize = 8;

    let rig = Rig::with("demo", |settings| settings.batch_max_items = 1).await;
    for i in 0..PAGE {
        rig.fixture.write(
            &format!("docs/p{i}.md"),
            format!("# P{i}\n\n{POISON} document {i}.\n"),
        );
    }
    full_scan(&rig.fixture.context(), &rig.fixture.project);
    // Indexed after the poison files, so its rowid is above every one of them.
    rig.fixture
        .write("docs/good.md", "# Good\n\nA perfectly fine document.\n");
    full_scan(&rig.fixture.context(), &rig.fixture.project);
    assert_eq!(rig.counts().0 as usize, PAGE + 1);

    let mut worker = rig.worker();
    worker.set_page_rows(PAGE);
    // A retry window short enough that the second strike lands on the next
    // pass: this test is about the paging walk, not the retry cadence, so it
    // wants every chunk in the page fully abandoned as quickly as possible.
    worker.set_poison_windows(Duration::ZERO, Duration::from_secs(600));
    worker.reconcile_fingerprint().await.unwrap();

    // Fill exactly one fetch page with poison. Two passes each: a rejection is
    // a suspicion the first time and a verdict the second.
    for _ in 0..2 * PAGE {
        assert!(worker.probe().await);
        assert_eq!(worker.drain().await, Drained::Interrupted);
    }
    assert_eq!(worker.skipped(), PAGE);
    assert_eq!(rig.counts().1, 0, "nothing good has been reached yet");

    // The first page is now entirely poisoned. That is a page to step over,
    // not the end of the missing set.
    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(
        rig.counts().1,
        1,
        "the chunk behind the poisoned page must still be embedded"
    );
    assert!(
        rig.stub
            .state
            .inputs()
            .iter()
            .any(|input| input.contains("perfectly fine")),
        "the good chunk was never sent to the endpoint"
    );

    // And it is genuinely done: no further work, no further requests.
    let before = rig.stub.state.attempts();
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.stub.state.attempts(), before);
    rig.stub.shutdown().await;
}

/// #6: a 4xx is the endpoint's opinion, and Ollama on Windows has been seen to
/// answer a valid request with a 400 wrapping a dial error. So a rejection
/// buys a delayed retry before it costs the chunk its vector — and a chunk
/// that is genuinely refused is abandoned on the second telling, not retried
/// forever.
#[tokio::test]
async fn a_refused_chunk_is_retried_once_before_it_is_abandoned() {
    const RETRY_IN: Duration = Duration::from_millis(50);

    let rig = Rig::with("demo", |settings| settings.batch_max_items = 1).await;
    // Lowest rowid, so it is the batch every pass reaches first.
    rig.fixture
        .write("docs/a.md", format!("# A\n\n{POISON} document.\n"));
    rig.fixture.write("docs/b.md", "# B\n\nSecond document.\n");
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let refused = |rig: &Rig| {
        rig.stub
            .state
            .inputs()
            .iter()
            .filter(|input| input.contains(POISON))
            .count()
    };

    let mut worker = rig.worker();
    worker.set_poison_windows(RETRY_IN, Duration::from_secs(600));
    worker.reconcile_fingerprint().await.unwrap();

    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Interrupted);
    assert_eq!(refused(&rig), 1, "the chunk was sent once");
    assert_eq!(worker.skipped(), 0, "one rejection is only a suspicion");

    // Inside the retry window it is held back, exactly like an abandoned one:
    // a rejection stops a pass, so without this the retry would be immediate
    // and the endpoint would see the same doomed batch in a tight loop.
    assert!(worker.probe().await);
    worker.drain().await;
    assert_eq!(refused(&rig), 1, "the retry was not delayed");
    assert_eq!(rig.counts(), (2, 1), "the chunk behind it still drained");

    // Past the window it gets its one retry, is refused again, and is now
    // abandoned for the whole (long) TTL.
    tokio::time::sleep(RETRY_IN * 2).await;
    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Interrupted);
    assert_eq!(refused(&rig), 2, "the delayed retry never happened");
    assert_eq!(worker.skipped(), 1, "the second rejection is the verdict");

    tokio::time::sleep(RETRY_IN * 2).await;
    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(refused(&rig), 2, "an abandoned chunk was sent a third time");
    rig.stub.shutdown().await;
}

/// …and abandonment expires. The misclassification that started #6 poisoned 64
/// healthy chunks for the life of the process; it must instead cost them one
/// TTL. Same failure, same chunk — the only thing that changes is the clock.
#[tokio::test]
async fn an_abandoned_chunk_is_offered_again_once_its_poison_expires() {
    const WINDOW: Duration = Duration::from_millis(50);

    let rig = Rig::with("demo", |settings| settings.batch_max_items = 1).await;
    rig.fixture.write("docs/a.md", "# A\n\nFirst document.\n");
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let mut worker = rig.worker();
    // The TTL is the longer of the two so the "still abandoned" check below
    // cannot lose a race with its own expiry on a busy machine.
    worker.set_poison_windows(WINDOW, WINDOW * 4);
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    // Two rejections: enough to abandon it, and nothing wrong with the chunk.
    // Nothing probes in between — a probe would eat a scripted reply.
    rig.stub
        .state
        .script(std::iter::repeat_n(Reply::Status(400), 2));

    assert_eq!(worker.drain().await, Drained::Interrupted);
    tokio::time::sleep(WINDOW * 2).await;
    assert_eq!(worker.drain().await, Drained::Interrupted);
    assert_eq!(worker.skipped(), 1, "abandoned after its retry");
    assert_eq!(rig.counts(), (1, 0));

    // Still abandoned while the TTL runs: nothing is sent.
    let sent = rig.stub.state.inputs().len();
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.stub.state.inputs().len(), sent, "the TTL was ignored");

    // Once it expires the chunk is ordinary work again, and the endpoint —
    // which was never really refusing it — embeds it.
    tokio::time::sleep(WINDOW * 8).await;
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.counts(), (1, 1), "the chunk never recovered");
    rig.stub.shutdown().await;
}

/// The worker screens vectors before handing them to the store, and the store
/// aborts the *whole* transaction for one it will not normalize. If the two
/// disagree by an epsilon, the batch retries forever with health still Ready —
/// so the screen must be the store's own predicate, checked here through the
/// real write path rather than against a copy of it.
///
/// Empty and non-finite vectors cannot reach this path (the client rejects an
/// empty vector outright, and JSON cannot carry NaN or infinity); the store's
/// own table covers those.
#[tokio::test]
async fn worker_and_store_agree_on_usable_vectors_at_the_threshold() {
    let cases: [(&str, Vec<f32>, bool); 6] = [
        ("zero", vec![0.0, 0.0], false),
        ("far below epsilon", vec![1e-10, 0.0], false),
        ("just below epsilon", vec![f32::EPSILON / 2.0, 0.0], false),
        ("at epsilon", vec![f32::EPSILON, 0.0], false),
        ("just above epsilon", vec![f32::EPSILON * 2.0, 0.0], true),
        ("unit", vec![1.0, 0.0], true),
    ];

    for (label, vector, storable) in cases {
        let rig = Rig::new("demo").await;
        rig.fixture.write("docs/a.md", "# A\n\nFirst document.\n");
        full_scan(&rig.fixture.context(), &rig.fixture.project);
        assert_eq!(rig.counts().0, 1, "{label}");

        let mut worker = rig.worker();
        worker.reconcile_fingerprint().await.unwrap();
        assert!(worker.probe().await, "{label}");
        rig.stub.state.script([Reply::Vectors(vec![vector])]);

        // A vector the worker accepts but the store rejects fails the upsert,
        // which surfaces as `Interrupted` and an unchanged count.
        assert_eq!(
            worker.drain().await,
            Drained::Idle,
            "{label} wedged a drain"
        );
        assert_eq!(rig.counts().1, u64::from(storable), "{label}");
        assert_eq!(worker.skipped(), usize::from(!storable), "{label}");
        rig.stub.shutdown().await;
    }
}

#[tokio::test]
async fn one_unusable_vector_does_not_block_its_batch() {
    let rig = Rig::new("demo").await;
    for name in ["a", "b", "c"] {
        rig.fixture.write(
            &format!("docs/{name}.md"),
            format!("# {name}\n\nDocument.\n"),
        );
    }
    full_scan(&rig.fixture.context(), &rig.fixture.project);
    assert_eq!(rig.counts().0, 3);

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    // One vector the store will not normalize, among two it will. The default
    // batch size is 8, so all three travel together.
    rig.stub.state.script([Reply::Vectors(vec![
        vec![1e-10, 0.0],
        vec![1.0, 0.0],
        vec![0.0, 1.0],
    ])]);

    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.counts(), (3, 2), "the valid peers were stored");
    assert_eq!(worker.skipped(), 1);
    rig.stub.shutdown().await;
}

// ---------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------

/// Seed `count` distinct documents, one chunk each, in rowid order.
fn seed(rig: &Rig, count: usize) {
    for i in 0..count {
        rig.fixture.write(
            &format!("docs/d{i:02}.md"),
            format!("# D{i:02}\n\nDocument number {i}.\n"),
        );
    }
    full_scan(&rig.fixture.context(), &rig.fixture.project);
    assert_eq!(rig.counts().0 as usize, count, "one chunk per seeded file");
}

/// A local server answers several requests at once; one batch at a time leaves
/// those slots idle for every round-trip. Overlap is measured *server-side*,
/// because that is the only place the question has an honest answer.
#[tokio::test]
async fn batches_overlap_on_the_endpoint() {
    const CHUNKS: usize = 16;
    const LIMIT: usize = 4;

    let rig = Rig::with("demo", |settings| {
        settings.batch_max_items = 1;
        settings.concurrency = LIMIT;
    })
    .await;
    seed(&rig, CHUNKS);
    // Long enough that batches issued back to back are provably on the wire
    // together rather than merely quick.
    rig.stub.state.set_delay(Duration::from_millis(50));

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.counts().1 as usize, CHUNKS, "every chunk still lands");

    let peak = rig.stub.state.max_in_flight();
    assert!(peak > 1, "batches never overlapped (peak {peak})");
    assert!(peak <= LIMIT, "{peak} in flight with concurrency = {LIMIT}");
    rig.stub.shutdown().await;
}

/// The claim rule, stated as an observable: a chunk stays in
/// `chunks_missing_embeddings` until its batch is upserted, so a second
/// claimer would send it again — paying twice for a vector and racing two
/// writes onto one row. Several batches *and* several fetch pages, because the
/// paging walk restarts from rowid 0 and would otherwise re-offer whatever is
/// still in flight.
#[tokio::test]
async fn no_chunk_is_sent_to_the_endpoint_twice() {
    const CHUNKS: usize = 20;

    let rig = Rig::with("demo", |settings| {
        settings.batch_max_items = 2;
        settings.concurrency = 4;
    })
    .await;
    seed(&rig, CHUNKS);
    rig.stub.state.set_delay(Duration::from_millis(20));

    let mut worker = rig.worker();
    worker.set_page_rows(4);
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Idle);
    assert_eq!(rig.counts().1 as usize, CHUNKS);

    let sent = rig.stub.state.inputs();
    let unique: std::collections::HashSet<&String> = sent.iter().collect();
    assert_eq!(unique.len(), sent.len(), "an input was embedded twice");
    // 20 chunks, 2 per batch, plus the probe.
    assert_eq!(sent.len(), CHUNKS + 1);
    assert_eq!(rig.stub.state.batches().len(), CHUNKS / 2 + 1);
    rig.stub.shutdown().await;
}

/// A 4xx while other batches are on the wire. The rejected batch is poison and
/// the pass stops, but the peers already in flight are finished rather than
/// thrown away — their vectors cost the same GPU time either way, and D-0007
/// wants the failure visible, not the progress lost.
#[tokio::test]
async fn a_rejected_batch_stops_the_pass_without_discarding_its_peers() {
    const CHUNKS: usize = 12;
    const LIMIT: usize = 4;

    let rig = Rig::with("demo", |settings| {
        settings.batch_max_items = 1;
        settings.concurrency = LIMIT;
    })
    .await;
    seed(&rig, CHUNKS);
    rig.stub.state.set_delay(Duration::from_millis(30));

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    // Refuse the first batch to reach the endpoint — which of them it is does
    // not matter, only that three peers are on the wire behind it.
    rig.stub.state.script([Reply::Status(400)]);

    assert_eq!(worker.drain().await, Drained::Interrupted);
    assert!(rig.stub.state.max_in_flight() > 1, "nothing was concurrent");
    // Suspected, not yet abandoned — one rejection buys a delayed retry, and
    // the default window outlasts this test, so the chunk stays held back
    // either way.
    assert_eq!(worker.skipped(), 0, "one rejection is not a verdict");
    assert!(
        rig.counts().1 >= (LIMIT - 1) as u64,
        "the peers of the rejected batch were discarded: {:?}",
        rig.counts()
    );
    assert!(
        rig.stub.state.batches().len() < CHUNKS,
        "the rejection did not stop the pass issuing"
    );
    assert!(!rig.embedder.health().is_ready(), "degraded, visibly");

    // The rest still drains once the endpoint answers again, and the refused
    // batch is never offered a second time.
    let mut passes = 0;
    while worker.probe().await && worker.drain().await != Drained::Idle && passes < 10 {
        passes += 1;
    }
    assert_eq!(rig.counts(), (CHUNKS as u64, CHUNKS as u64 - 1));
    let sent = rig.stub.state.inputs();
    let chunks_sent: Vec<&String> = sent
        .iter()
        .filter(|input| input.contains("Document number"))
        .collect();
    let unique: std::collections::HashSet<&&String> = chunks_sent.iter().collect();
    assert_eq!(
        unique.len(),
        chunks_sent.len(),
        "a chunk was embedded twice"
    );
    assert_eq!(chunks_sent.len(), CHUNKS, "the refused batch was re-sent");
    rig.stub.shutdown().await;
}

/// Shutdown must not wait out the requests already on the wire.
#[tokio::test]
async fn cancellation_settles_a_concurrent_drain_promptly() {
    const CHUNKS: usize = 24;
    const DELAY: Duration = Duration::from_millis(500);

    let rig = Rig::with("demo", |settings| {
        settings.batch_max_items = 1;
        settings.concurrency = 4;
    })
    .await;
    seed(&rig, CHUNKS);
    rig.stub.state.set_delay(DELAY);

    let task = tokio::spawn(rig.worker().run());
    until("several batches to be on the wire", || {
        rig.stub.state.in_flight() > 1
    })
    .await;

    let cancelled = Instant::now();
    rig.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the worker stopped without waiting for the endpoint")
        .expect("the worker task did not panic");
    assert!(
        cancelled.elapsed() < DELAY / 2,
        "shutdown waited {:?} on in-flight requests",
        cancelled.elapsed()
    );
    rig.stub.shutdown().await;
}

/// `concurrency = 1` is the pipeline as it was: one request on the wire at a
/// time, one batch per chunk, nothing overlapping.
#[tokio::test]
async fn concurrency_of_one_is_the_serial_pipeline() {
    const CHUNKS: usize = 8;

    let rig = Rig::with("demo", |settings| {
        settings.batch_max_items = 1;
        settings.concurrency = 1;
    })
    .await;
    seed(&rig, CHUNKS);
    rig.stub.state.set_delay(Duration::from_millis(10));

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    assert_eq!(worker.drain().await, Drained::Idle);

    assert_eq!(rig.counts().1 as usize, CHUNKS);
    assert_eq!(rig.stub.state.max_in_flight(), 1, "requests overlapped");
    assert_eq!(rig.stub.state.batches().len(), CHUNKS + 1, "probe + 8");
    rig.stub.shutdown().await;
}

// ---------------------------------------------------------------------------
// Health observations
// ---------------------------------------------------------------------------

/// A query embedding can stall for seconds while a probe answers in
/// milliseconds. The slow one's verdict must not land on top of the fast one's
/// — D-0007 wants the *current* state reported, not the last one written. Its
/// timeout no longer publishes at all (#5), but everything else it can observe
/// still does, and still from a ticket claimed before the request.
#[tokio::test]
async fn a_stale_failure_cannot_demote_a_newer_probe() {
    let rig = Rig::new("demo").await;
    let worker = rig.worker();
    let health = rig.embedder.health();

    // Claimed before the request, exactly as `Embedder::embed_query` does.
    let in_flight = health.ticket();
    assert!(worker.probe().await);
    assert!(health.is_ready());

    in_flight.set_unreachable(&rig.stub.base, "query embedding failed: connection reset");
    assert!(
        health.is_ready(),
        "a stale failure demoted a newer successful probe"
    );
    assert!(matches!(
        rig.embedder.status(),
        lore_core::EmbeddingStatus::Ready { .. }
    ));

    // A verdict claimed *after* the probe still wins, so real outages surface.
    health
        .ticket()
        .set_unreachable(&rig.stub.base, "connection refused");
    assert!(!health.is_ready());
    rig.stub.shutdown().await;
}

/// A pass that finds nothing to do while health says the endpoint is down must
/// re-probe, not sleep: the fallback tick is a minute away and every search in
/// between is forced lexical-only on stale information.
#[tokio::test]
async fn an_idle_pass_reprobes_a_stale_unreachable_instead_of_sleeping() {
    const CHUNKS: usize = 12;

    let rig = Rig::with("demo", |settings| settings.batch_max_items = 1).await;
    for i in 0..CHUNKS {
        rig.fixture.write(
            &format!("docs/f{i}.md"),
            format!("# F{i}\n\nDocument {i}.\n"),
        );
    }
    full_scan(&rig.fixture.context(), &rig.fixture.project);
    assert_eq!(rig.counts().0 as usize, CHUNKS);
    // One request per chunk, slow enough that the demotion below lands while
    // the pass is provably still running.
    rig.stub
        .state
        .set_delay(std::time::Duration::from_millis(30));

    let task = tokio::spawn(rig.worker().run());

    until("the drain to start", || rig.counts().1 >= 1).await;
    rig.embedder
        .health()
        .set_unreachable(&rig.stub.base, "a query saw a blip");

    // Nothing pulses the worker and IDLE_TICK is 60s, so recovery inside the
    // helper's deadline can only have come from the end-of-pass re-probe.
    until("health to recover without a pulse", || {
        rig.embedder.health().is_ready()
    })
    .await;
    assert_eq!(rig.counts().1 as usize, CHUNKS);

    rig.cancel.cancel();
    task.await.expect("the worker stops on cancellation");
    rig.stub.shutdown().await;
}

/// #5: the first search after the endpoint idles out its model pays a reload
/// the query has no patience for. Running out of *this request's* patience is
/// an observation about the deadline, not about the endpoint — a reloading
/// model and a dead server look identical from here — so health survives it,
/// and the worker is asked to go and settle the question now rather than at
/// the next idle tick a minute away.
#[tokio::test]
async fn a_query_timeout_leaves_health_alone_and_asks_for_a_probe() {
    // Nothing is indexed, so the only requests this worker makes are probes.
    let mut rig = Rig::new("demo").await;
    rig.embedder.set_query_timeout(Duration::from_millis(50));
    // Slower than the query will wait, faster than a probe will.
    rig.stub.state.set_delay(Duration::from_millis(250));

    let probes = || {
        rig.stub
            .state
            .inputs()
            .iter()
            .filter(|input| input.contains(lore::embed::client::PROBE_INPUT))
            .count()
    };

    let task = tokio::spawn(rig.worker().run());
    until("the worker's first probe to land", || {
        rig.embedder.health().is_ready()
    })
    .await;
    let before = probes();

    assert!(
        rig.embedder.embed_query("results ordering").await.is_none(),
        "the query outlasted its own deadline, so it has no vector"
    );
    assert!(
        rig.embedder.health().is_ready(),
        "a query timeout demoted the endpoint: {:?}",
        rig.embedder.status()
    );

    // Nothing pulses the worker and IDLE_TICK is 60s, so a probe arriving
    // inside the helper's deadline can only be the one the timeout asked for.
    until("the requested probe", || probes() > before).await;
    assert!(
        rig.embedder.health().is_ready(),
        "the probe answered, so the endpoint was never unhealthy"
    );

    rig.cancel.cancel();
    task.await.expect("the worker stops on cancellation");
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
    // The stub declares no `dimensions`, so the identity carries the width the
    // probe observed — not the `0` that would leave model id alone to tell two
    // spaces apart (#9).
    assert_eq!(
        rig.stored_fingerprint().as_ref(),
        Some(&fingerprint_of(
            rig.embedder.client().unwrap().settings(),
            Some(DIMS)
        ))
    );

    // Same configuration ⇒ the vectors survive. Including through a reconcile
    // that has not probed yet: an unobserved width must not read as a changed
    // one, or every start before the first probe would empty the index.
    worker.reconcile_fingerprint().await.unwrap();
    assert_eq!(rig.counts().1, embedded, "an unchanged space keeps vectors");
    assert_eq!(
        rig.stored_fingerprint().unwrap().dimensions as usize,
        DIMS,
        "and the recorded width is not forgotten"
    );

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

/// Issue #9: with no `dimensions` in the config, model identity used to rest
/// on the model id alone — so a GGUF swapped in behind the same name mixed two
/// vector spaces in one index, silently. The probed width is folded into the
/// fingerprint, which makes the swap visible whenever it changes the width.
#[tokio::test]
async fn a_model_swapped_behind_the_same_name_is_caught_by_its_width() {
    let rig = Rig::new("demo").await;
    populate_standard_tree(&rig.fixture);
    full_scan(&rig.fixture.context(), &rig.fixture.project);
    assert!(
        rig.embedder
            .client()
            .unwrap()
            .settings()
            .dimensions
            .is_none(),
        "this is the undeclared-width case"
    );

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    worker.drain().await;
    let (chunks, embedded) = rig.counts();
    assert_eq!(embedded, chunks);
    assert_eq!(rig.stored_fingerprint().unwrap().dimensions as usize, DIMS);

    // Same endpoint, same model id, different weights: the next probe answers
    // with a wider vector. Nothing else about the configuration moved.
    rig.stub
        .state
        .script([Reply::Vectors(vec![vec![0.5; DIMS * 2]])]);
    assert!(worker.probe().await);

    assert_eq!(
        rig.counts(),
        (chunks, 0),
        "vectors from the old space must not survive the swap"
    );
    assert_eq!(
        rig.stored_fingerprint().unwrap().dimensions as usize,
        DIMS * 2
    );
    rig.stub.shutdown().await;
}

/// The upgrade path for a store embedded before widths were recorded: its
/// fingerprint says `dimensions: 0`, its vectors are all one width, and the
/// only honest reading of that is "this is the same space, now described
/// properly". Re-embedding a whole corpus to learn nothing would be a tax on
/// every existing user.
#[tokio::test]
async fn a_stored_identity_with_no_width_adopts_the_probed_one_and_keeps_its_vectors() {
    let rig = Rig::new("demo").await;
    populate_standard_tree(&rig.fixture);
    full_scan(&rig.fixture.context(), &rig.fixture.project);

    let mut worker = rig.worker();
    worker.reconcile_fingerprint().await.unwrap();
    assert!(worker.probe().await);
    worker.drain().await;
    let (chunks, embedded) = rig.counts();
    assert_eq!(embedded, chunks);

    // Rewind the stored identity to what a pre-#9 daemon would have written.
    let legacy = EmbeddingFingerprint {
        dimensions: 0,
        ..rig.stored_fingerprint().unwrap()
    };
    rig.fixture
        .store
        .blocking(|store| store.set_embedding_fingerprint(&legacy))
        .unwrap();

    assert!(worker.probe().await);
    assert_eq!(
        rig.counts(),
        (chunks, embedded),
        "an identity that merely gained a width keeps its vectors"
    );
    assert_eq!(rig.stored_fingerprint().unwrap().dimensions as usize, DIMS);
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
