//! Watcher behavior, in two layers.
//!
//! The first layer drives the real platform watcher end to end: real files,
//! real notifications, real time. Those are the only tests in the package that
//! depend on filesystem notifications, so they are deliberately few, and every
//! wait is a poll loop with a generous ceiling rather than a fixed sleep — a
//! slow CI box should make them slower, never flakier.
//!
//! The second layer drives the pump through its injectable seam
//! ([`watch::WatchBackend`] plus [`watch::event_channel`]). Overlapping roots,
//! a refused arm, a backend error and a callback overflow are all states the
//! platform will not produce on demand; without the seam they are untestable,
//! and untested is exactly how they got broken.

mod daemon_support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use camino::{Utf8Path, Utf8PathBuf};
use daemon_support::Fixture;
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::{self, IndexContext};
use lore::daemon::paths::canonicalize_root;
use lore::daemon::queue::{IndexQueue, ProjectWork};
use lore::daemon::store_handle::StoreHandle;
use lore::daemon::watch::{
    self, EVENT_CAPACITY, EventSink, RetryPolicy, WatchBackend, WatchCommand, WatchSender,
    WatchStatus, Watcher,
};
use lore::embed::Embedder;
use lore::store::{Project, ProjectId};
use lore_core::WatchState;
use notify_debouncer_full::DebouncedEvent;
use serde_json::Value;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Ceiling for "the watcher should have noticed by now". Two orders of
/// magnitude above the debounce window.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long to wait before concluding that nothing happened. Must comfortably
/// exceed the debounce window, but a wrong answer here only makes the test
/// weaker, never flaky.
const QUIET: Duration = Duration::from_secs(5);

async fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out after {PATIENCE:?} waiting for: {label}");
}

// ---------------------------------------------------------------------------
// Layer 1: the real platform watcher
// ---------------------------------------------------------------------------

struct Env {
    _dir: TempDir,
    root: Utf8PathBuf,
    /// Deliberately *inside* the project root: the daemon's own writes are
    /// then indistinguishable from project edits to the platform watcher,
    /// which is exactly the feedback loop the filter has to break.
    data_dir: Utf8PathBuf,
    store: StoreHandle,
    project: Project,
    queue: IndexQueue,
    status: WatchStatus,
    cancel: CancellationToken,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = canonicalize_root(dir.path()).expect("canonical root");
        let data_dir = root.join("lore-data");
        std::fs::create_dir_all(&data_dir).unwrap();

        let store = StoreHandle::open(data_dir.join("lore.db")).expect("open store");
        let id = {
            let root = root.clone();
            store
                .blocking(move |store| store.register_project(&root, "demo"))
                .expect("register")
        };

        Self {
            project: project(id, root.as_str(), "demo"),
            _dir: dir,
            root,
            data_dir,
            store,
            queue: IndexQueue::new(),
            status: WatchStatus::new(),
            cancel: CancellationToken::new(),
        }
    }

    fn context(&self) -> IndexContext {
        IndexContext::new(
            self.store.clone(),
            self.data_dir.clone(),
            self.cancel.clone(),
        )
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn indexed_paths(&self) -> Vec<String> {
        let id = self.project.id;
        let mut paths: Vec<String> = self
            .store
            .blocking(move |store| store.list_files(id))
            .expect("list files")
            .into_iter()
            .map(|record| record.path.into_string())
            .collect();
        paths.sort();
        paths
    }

    /// Start the watcher and wait for the platform watch to actually arm.
    ///
    /// Readiness is the reported watcher state, not a sleep: `notify`'s
    /// Windows backend acknowledges the watch before `arm` returns, so
    /// `Armed` means events from here on cannot be missed. A fixed sleep
    /// would only be a guess about the same moment.
    ///
    /// The returned sender must be held by the caller: dropping it closes the
    /// command channel, which the watcher treats as shutdown.
    #[must_use]
    async fn start_watcher(&self) -> WatchSender {
        let (tx, rx) = watch::channel();
        let queue = self.queue.clone();
        let data_dir = self.data_dir.clone();
        let status = self.status.clone();
        let cancel = self.cancel.clone();
        tokio::spawn(async move { watch::run(rx, queue, data_dir, status, cancel).await });
        tx.send(WatchCommand::Watch(self.project.clone()))
            .expect("watcher accepted the command");
        let id = self.project.id;
        wait_until("the platform watch to arm", || {
            self.status.of(id) == WatchState::Armed
        })
        .await;
        tx
    }
}

/// The whole point of the daemon: edit a file, and search sees it, with no
/// explicit reindex anywhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_created_after_startup_is_indexed_automatically() {
    let env = Env::new();
    env.write("README.md", "# demo\n");
    index::full_scan(&env.context(), &env.project);
    assert_eq!(env.indexed_paths(), ["README.md"]);

    tokio::spawn(index::run(env.context(), env.queue.clone()));
    let _watch = env.start_watcher().await;

    env.write(
        "src/new_module.rs",
        "pub fn brand_new() -> u32 {\n    7\n}\n",
    );
    wait_until("src/new_module.rs to be indexed", || {
        env.indexed_paths()
            .contains(&"src/new_module.rs".to_string())
    })
    .await;

    // …and an edit to it, too: the second event for a path must not be
    // swallowed by the first one's debounce bookkeeping.
    env.write(
        "src/new_module.rs",
        "pub fn brand_new() -> u32 {\n    distinctivetoken()\n}\n",
    );
    let id = env.project.id;
    wait_until("the edit to be reindexed", || {
        env.store
            .blocking(|store| {
                store.lexical_search(
                    "distinctivetoken",
                    &lore::store::SearchFilter::project(id),
                    5,
                )
            })
            .map(|hits| !hits.is_empty())
            .unwrap_or(false)
    })
    .await;

    env.cancel.cancel();
}

/// The daemon writes to its data directory constantly (SQLite, WAL,
/// heartbeat). If those writes reached the indexer it would index its own
/// database, which would produce more writes — an unbounded loop. Here the
/// data directory sits *inside* the watched root, so only the explicit filter
/// prevents it.
#[tokio::test(flavor = "multi_thread")]
async fn the_daemons_own_writes_never_become_index_work() {
    let env = Env::new();
    // No indexer task: the queue is the assertion surface, so "nothing was
    // queued" is deterministic rather than a race against a consumer.
    let _watch = env.start_watcher().await;

    for i in 0..20 {
        std::fs::write(env.data_dir.join(format!("lore.db-wal{i}")), "noise").unwrap();
        std::fs::write(
            env.data_dir.join("daemon.json"),
            format!("{{\"beat\":{i}}}"),
        )
        .unwrap();
    }
    tokio::time::sleep(QUIET).await;
    assert!(
        env.queue.is_empty(),
        "the daemon's own writes were queued as index work"
    );

    // Control: the watcher is alive and the queue does receive real edits.
    env.write("src/real.rs", "pub fn real() {}\n");
    wait_until("a real project edit to be queued", || !env.queue.is_empty()).await;
    let (project, work) = env.queue.take().unwrap();
    assert_eq!(project, env.project.id);
    assert!(
        work.full || work.paths.iter().any(|p| p.as_str() == "src/real.rs"),
        "queued work should name the edited file: {work:?}"
    );

    env.cancel.cancel();
}

// ---------------------------------------------------------------------------
// Layer 2: the injectable seam
// ---------------------------------------------------------------------------

/// A watch backend that arms nothing, records everything, and can be told to
/// refuse — the states a real volume produces only when it feels like it.
#[derive(Clone, Default)]
struct FakeBackend {
    inner: Arc<Mutex<FakeState>>,
}

#[derive(Default)]
struct FakeState {
    arms: Vec<Utf8PathBuf>,
    disarms: Vec<Utf8PathBuf>,
    refusing: bool,
    stopped: bool,
}

impl FakeBackend {
    fn refusing() -> Self {
        let backend = Self::default();
        backend.refuse();
        backend
    }

    fn refuse(&self) {
        self.lock().refusing = true;
    }

    fn allow(&self) {
        self.lock().refusing = false;
    }

    fn arms(&self) -> usize {
        self.lock().arms.len()
    }

    fn disarms(&self) -> usize {
        self.lock().disarms.len()
    }

    fn stopped(&self) -> bool {
        self.lock().stopped
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.inner.lock().expect("fake backend mutex")
    }
}

impl WatchBackend for FakeBackend {
    fn arm(&mut self, root: &Utf8Path) -> anyhow::Result<()> {
        let mut state = self.lock();
        state.arms.push(root.to_owned());
        if state.refusing {
            anyhow::bail!("the fake backend is refusing to arm {root}");
        }
        Ok(())
    }

    fn disarm(&mut self, root: &Utf8Path) {
        self.lock().disarms.push(root.to_owned());
    }

    fn stop(&mut self) {
        self.lock().stopped = true;
    }
}

/// Retry fast enough that the tests never wait on a real backoff, while still
/// exercising the same scheduling path as the shipped policy.
fn brisk_retry() -> RetryPolicy {
    RetryPolicy {
        first: Duration::from_millis(1),
        max: Duration::from_millis(1),
    }
}

/// The pump, its fake backend, and direct access to both ends of the
/// callback boundary.
struct Seam {
    /// Held: dropping the sender closes the command channel, which the pump
    /// treats as shutdown.
    commands: WatchSender,
    sink: EventSink,
    queue: IndexQueue,
    status: WatchStatus,
    cancel: CancellationToken,
    served: BTreeMap<ProjectId, ProjectWork>,
}

impl Seam {
    fn new(backend: FakeBackend) -> Self {
        Self::with_data_dir(backend, Utf8PathBuf::from(r"C:\data\lore"))
    }

    fn with_data_dir(backend: FakeBackend, data_dir: Utf8PathBuf) -> Self {
        let (commands, rx) = watch::channel();
        let (sink, events) = watch::event_channel();
        let queue = IndexQueue::new();
        let status = WatchStatus::new();
        let cancel = CancellationToken::new();
        tokio::spawn(watch::run_with(
            Watcher {
                backend,
                events,
                retry: brisk_retry(),
                status: status.clone(),
            },
            rx,
            queue.clone(),
            data_dir,
            cancel.clone(),
        ));
        Self {
            commands,
            sink,
            queue,
            status,
            cancel,
            served: BTreeMap::new(),
        }
    }

    fn want(&self, project: &Project) {
        self.commands
            .send(WatchCommand::Watch(project.clone()))
            .expect("the pump accepted the command");
    }

    async fn wait_armed(&self, project: ProjectId) {
        wait_until("the watch to arm", || {
            self.status.of(project) == WatchState::Armed
        })
        .await;
    }

    /// Deliver one modification event naming `paths`, as the debouncer would.
    fn emit(&self, paths: &[&str]) {
        let mut event = notify::Event::new(notify::EventKind::Modify(
            notify::event::ModifyKind::Data(notify::event::DataChange::Any),
        ));
        for path in paths {
            event = event.add_path(std::path::PathBuf::from(path));
        }
        self.sink.send(Ok(vec![DebouncedEvent::new(
            event,
            std::time::Instant::now(),
        )]));
    }

    /// Deliver a backend error naming `path`, as a failing watch would.
    fn emit_error(&self, path: &str) {
        let error =
            notify::Error::generic("the watch went away").add_path(std::path::PathBuf::from(path));
        self.sink.send(Err(vec![error]));
    }

    /// Move everything currently queued into [`Self::served`], merging
    /// repeated takes for one project.
    fn collect(&mut self) {
        while let Some((project, work)) = self.queue.take() {
            let entry = self.served.entry(project).or_default();
            entry.full |= work.full;
            entry.paths.extend(work.paths);
        }
    }

    fn forget_served(&mut self) {
        self.collect();
        self.served.clear();
    }

    fn paths_for(&self, project: ProjectId) -> Vec<String> {
        self.served
            .get(&project)
            .map(|work| work.paths.iter().map(|p| p.to_string()).collect())
            .unwrap_or_default()
    }

    fn full_for(&self, project: ProjectId) -> bool {
        self.served.get(&project).is_some_and(|work| work.full)
    }
}

impl Drop for Seam {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn project(id: ProjectId, root: &str, name: &str) -> Project {
    Project {
        id,
        root: Utf8PathBuf::from(root),
        name: name.to_string(),
        key: name.to_string(),
        kind: lore::types::SourceKind::Repo,
    }
}

/// Registering `C:\repo` and `C:\repo\packages\game` is legitimate — a
/// monorepo and one package inside it — and an edit under the nested root
/// belongs to both indexes. Routing to the first containing root instead left
/// whichever project was registered second permanently stale, so both
/// registration orders are checked.
#[tokio::test]
async fn an_edit_under_a_nested_root_reaches_every_containing_project() {
    let outer = project(1, r"C:\repo", "outer");
    let inner = project(2, r"C:\repo\packages\game", "inner");

    for order in [[&outer, &inner], [&inner, &outer]] {
        let mut seam = Seam::new(FakeBackend::default());
        for project in order {
            seam.want(project);
            seam.wait_armed(project.id).await;
        }

        seam.emit(&[r"C:\repo\packages\game\Player.cs"]);
        wait_until("both projects to be queued", || {
            seam.collect();
            seam.served.len() == 2
        })
        .await;

        assert_eq!(
            seam.paths_for(1),
            ["packages/game/Player.cs"],
            "the outer project sees the edit at its own relative path (order {:?})",
            order.map(|p| p.id),
        );
        assert_eq!(
            seam.paths_for(2),
            ["Player.cs"],
            "the nested project sees the same edit at its own relative path (order {:?})",
            order.map(|p| p.id),
        );
    }
}

/// A reconnecting volume or a transient Windows error refuses the arm once.
/// Giving up there froze live indexing for that project for the lifetime of
/// the daemon, and said so in exactly one log line.
#[tokio::test]
async fn a_refused_arm_is_retried_and_reported_until_it_succeeds() {
    let backend = FakeBackend::refusing();
    let seam = Seam::new(backend.clone());
    let project = project(1, r"C:\repo", "demo");
    seam.want(&project);

    wait_until("the refused arm to be reported", || {
        seam.status.of(1) == WatchState::Retrying
    })
    .await;
    // Not one attempt and a log line: the desired watch outlives the failure.
    wait_until("further arm attempts", || backend.arms() >= 3).await;

    backend.allow();
    wait_until("the retry to succeed", || {
        seam.status.of(1) == WatchState::Armed
    })
    .await;
    assert!(
        backend.disarms() >= 1,
        "a re-arm drops the previous watch first, or notify leaks a directory handle per attempt"
    );
}

/// A backend error means the event stream for that root is no longer
/// trustworthy. The rescan repairs the index; without the re-arm the project
/// would then sit there, correct once and frozen forever.
#[tokio::test]
async fn a_backend_error_queues_a_rescan_and_re_arms_the_watch() {
    let backend = FakeBackend::default();
    let mut seam = Seam::new(backend.clone());
    let project = project(1, r"C:\repo", "demo");
    seam.want(&project);
    seam.wait_armed(1).await;
    let arms_before = backend.arms();

    // Refuse the re-arm so the un-armed interval is observable rather than a
    // microsecond-wide race.
    backend.refuse();
    seam.emit_error(r"C:\repo");

    wait_until("a full rescan of the affected project", || {
        seam.collect();
        seam.full_for(1)
    })
    .await;
    wait_until("status to report the watch as not armed", || {
        seam.status.of(1) == WatchState::Retrying
    })
    .await;
    wait_until("a re-arm attempt", || backend.arms() > arms_before).await;

    backend.allow();
    wait_until("the watch to come back", || {
        seam.status.of(1) == WatchState::Armed
    })
    .await;
}

/// Current-thread on purpose: with no await point in the flood loop the pump
/// cannot drain, so the bound is reached deterministically rather than by
/// out-running a concurrent consumer.
#[tokio::test(flavor = "current_thread")]
async fn a_callback_overflow_collapses_to_a_full_rescan_and_detail_resumes() {
    let backend = FakeBackend::default();
    let mut seam = Seam::new(backend.clone());
    let project = project(1, r"C:\repo", "demo");
    seam.want(&project);
    seam.wait_armed(1).await;
    seam.forget_served();

    // Empty batches carry no paths, so any work that appears can only have
    // come from the overflow bit — not from the events that got through.
    for _ in 0..(EVENT_CAPACITY * 4) {
        seam.sink.send(Ok(Vec::new()));
    }

    wait_until("the dropped batches to become a full rescan", || {
        seam.collect();
        seam.full_for(1)
    })
    .await;
    seam.forget_served();

    // The channel is not poisoned by the overflow: detail resumes.
    seam.emit(&[r"C:\repo\src\after.rs"]);
    wait_until("detailed events to resume", || {
        seam.collect();
        !seam.paths_for(1).is_empty()
    })
    .await;
    assert_eq!(seam.paths_for(1), ["src/after.rs"]);
}

/// Cancellation, not a dropped channel: the daemon's shutdown path cancels
/// the token while every sender is still alive.
#[tokio::test]
async fn the_pump_releases_the_backend_on_cancellation() {
    let backend = FakeBackend::default();
    let seam = Seam::new(backend.clone());
    let project = project(1, r"C:\repo", "demo");
    seam.want(&project);
    seam.wait_armed(1).await;

    seam.cancel.cancel();
    wait_until("the backend to be released", || backend.stopped()).await;
}

/// A watch that is not armed is a degraded daemon, and D-0007's rule for
/// degradation is that it has to be visible rather than silent.
#[tokio::test]
async fn v1_status_reports_per_project_watcher_state() {
    let fixture = Fixture::new("demo");
    let backend = FakeBackend::refusing();
    let seam = Seam::with_data_dir(backend.clone(), fixture.data_dir.clone());

    let state = AppState {
        store: fixture.store.clone(),
        queue: seam.queue.clone(),
        watch: seam.commands.clone(),
        watch_status: seam.status.clone(),
        config: Arc::new(Config::default()),
        embeddings: Embedder::disabled(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        data_dir: fixture.data_dir.clone(),
    };
    let router = router(state);

    seam.want(&fixture.project);
    wait_until("the refused arm to reach status", || {
        seam.status.of(fixture.project.id) == WatchState::Retrying
    })
    .await;
    assert_eq!(
        status_json(&router).await["projects"][0]["watch"],
        "retrying"
    );

    backend.allow();
    seam.wait_armed(fixture.project.id).await;
    assert_eq!(status_json(&router).await["projects"][0]["watch"], "armed");
}

async fn status_json(router: &Router) -> Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router never fails");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).expect("status is JSON")
}
