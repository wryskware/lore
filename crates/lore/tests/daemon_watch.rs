//! Watcher end-to-end. These are the only tests in the package that depend
//! on real filesystem notifications and real time, so they are deliberately
//! few, and every wait is a poll loop with a generous ceiling rather than a
//! fixed sleep — a slow CI box should make them slower, never flakier.

use std::time::Duration;

use camino::Utf8PathBuf;
use lore::daemon::index::{self, IndexContext};
use lore::daemon::paths::canonicalize_root;
use lore::daemon::queue::IndexQueue;
use lore::daemon::store_handle::StoreHandle;
use lore::daemon::watch::{self, DEBOUNCE, WatchCommand};
use lore::store::Project;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Ceiling for "the watcher should have noticed by now". Two orders of
/// magnitude above the debounce window.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long to wait before concluding that nothing happened. Must comfortably
/// exceed the debounce window, but a wrong answer here only makes the test
/// weaker, never flaky.
const QUIET: Duration = Duration::from_secs(5);

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
            project: Project {
                id,
                root: root.clone(),
                name: "demo".into(),
            },
            _dir: dir,
            root,
            data_dir,
            store,
            queue: IndexQueue::new(),
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

    /// Start the watcher and wait for the platform watch to arm.
    ///
    /// The returned sender must be held by the caller: dropping it closes the
    /// command channel, which the watcher treats as shutdown.
    #[must_use]
    async fn start_watcher(&self) -> watch::WatchSender {
        let (tx, rx) = watch::channel();
        let queue = self.queue.clone();
        let data_dir = self.data_dir.clone();
        let cancel = self.cancel.clone();
        tokio::spawn(async move { watch::run(rx, queue, data_dir, cancel).await });
        tx.send(WatchCommand::Watch(self.project.clone()))
            .expect("watcher accepted the command");
        // The command is handled asynchronously; give the platform watcher a
        // moment to arm before the test makes changes it must not miss.
        tokio::time::sleep(DEBOUNCE).await;
        tx
    }
}

async fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out after {PATIENCE:?} waiting for: {label}");
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
