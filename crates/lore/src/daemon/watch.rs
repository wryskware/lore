//! Filesystem watching → coalesced index work.
//!
//! One debouncer (`notify` + `notify-debouncer-full`, tens of seconds — see
//! [`crate::config::DEFAULT_WATCH_DEBOUNCE_SECS`]) with one recursive watch
//! per registered project root. A debouncer per project would
//! mean a background thread per project for no benefit — the debouncer
//! already tracks paths independently, and a single event stream keeps
//! ordering sane when roots nest.
//!
//! The pump is deliberately dumb. It does not stat, read, or classify: it
//! maps each event path to `(project, relative path)`, drops anything
//! obviously irrelevant, and hands the rest to the coalescing queue. Every
//! real decision — does this file still exist, does it still pass the ignore
//! rules, is it a directory — is made once, later, by the indexer on the
//! blocking pool. A watcher callback that does IO is a watcher that drops
//! events under load.
//!
//! Three properties the pump owes the rest of the daemon, each of which was
//! a real defect first:
//!
//! - **Every containing project is served.** Roots may legitimately nest
//!   (`C:\repo` and `C:\repo\packages\game` are two projects), so an event is
//!   routed to *all* roots that contain it. Routing to the first match leaves
//!   the other project's index permanently stale.
//! - **The callback boundary is bounded.** The debouncer's thread must never
//!   block, so it `try_send`s into a bounded channel; a full channel drops the
//!   detailed batch and sets an overflow bit instead of growing memory with
//!   raw `PathBuf`s. The pump answers the bit with a full rescan, so detail is
//!   lost but correctness is not.
//! - **Desired watches outlive failed ones.** A watch that cannot be armed
//!   (reconnecting volume, transient Windows error) stays desired and is
//!   retried with bounded backoff, and its state is visible in `/v1/status`
//!   rather than living only in one startup log line.
//!
//! Own-write feedback: the data directory (SQLite + WAL + handshake, written
//! constantly) normally lives outside every project root, but a root *could*
//! be an ancestor of it, so it is filtered explicitly here as well as in the
//! walker.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Context;
use camino::{Utf8Path, Utf8PathBuf};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache, new_debouncer,
};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use lore_core::WatchState;

use crate::store::{Project, ProjectId};

use super::paths;
use super::queue::IndexQueue;
use super::walk;

/// Debounced batches the pump may fall behind by before detail is dropped.
///
/// This is a memory bound, not a latency knob: one batch can carry thousands
/// of `PathBuf`s, and the pump's work per batch is microseconds of string
/// normalization. Sixty-four batches is far more slack than a healthy pump
/// ever uses, and small enough that a branch switch or Unity import cannot
/// grow the process without limit before the overflow bit trips.
pub const EVENT_CAPACITY: usize = 64;

/// Instructions to the watcher task.
#[derive(Debug, Clone)]
pub enum WatchCommand {
    /// Start watching a project root (idempotent).
    Watch(Project),
    /// Stop watching a deregistered project (idempotent, and silent about a
    /// project it never had). The root travels inside the pump's own table
    /// rather than in the message: by the time this is sent the project is
    /// already gone from the store, so the pump is the last place that still
    /// knows which path to disarm.
    Unwatch(ProjectId),
}

pub type WatchSender = mpsc::UnboundedSender<WatchCommand>;
pub type WatchReceiver = mpsc::UnboundedReceiver<WatchCommand>;

pub fn channel() -> (WatchSender, WatchReceiver) {
    mpsc::unbounded_channel()
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Per-project watcher coverage, shared read-only with `/v1/status`.
///
/// The wire enum is reused rather than mirrored: there is exactly one consumer
/// and a private twin would only add a mapping to keep in sync.
#[derive(Clone, Debug, Default)]
pub struct WatchStatus {
    states: Arc<Mutex<BTreeMap<ProjectId, WatchState>>>,
}

impl WatchStatus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Coverage for one project; `Unknown` until the watcher has an opinion
    /// (a project registered microseconds ago, or one the watcher never
    /// heard about).
    pub fn of(&self, project: ProjectId) -> WatchState {
        self.lock()
            .get(&project)
            .copied()
            .unwrap_or(WatchState::Unknown)
    }

    fn set(&self, project: ProjectId, state: WatchState) {
        self.lock().insert(project, state);
    }

    /// Drop a deregistered project's coverage. Leaving the entry would keep a
    /// forgotten project's watch state alive for whatever row later inherits
    /// its id.
    pub fn forget(&self, project: ProjectId) {
        self.lock().remove(&project);
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<ProjectId, WatchState>> {
        self.states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// The bounded callback boundary
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Overflow {
    dropped: AtomicBool,
    wake: Notify,
}

/// The debouncer's end of the callback boundary.
#[derive(Clone, Debug)]
pub struct EventSink {
    tx: mpsc::Sender<DebounceEventResult>,
    overflow: Arc<Overflow>,
}

impl EventSink {
    /// Hand one debounced batch to the pump. Never blocks — it is called from
    /// the debouncer's own thread, which must stay free to keep reading the
    /// platform's event buffer.
    ///
    /// A full channel means the pump is behind a storm. Dropping the batch and
    /// raising the overflow bit costs detail; keeping it would cost memory
    /// proportional to the storm, and the pump can always re-derive the truth
    /// from disk.
    pub fn send(&self, batch: DebounceEventResult) {
        if self.tx.try_send(batch).is_err() {
            self.overflow.dropped.store(true, Ordering::Release);
            self.overflow.wake.notify_one();
        }
    }
}

/// The pump's end of the callback boundary.
#[derive(Debug)]
pub struct EventStream {
    events: mpsc::Receiver<DebounceEventResult>,
    overflow: Arc<Overflow>,
}

pub fn event_channel() -> (EventSink, EventStream) {
    let (tx, events) = mpsc::channel(EVENT_CAPACITY);
    let overflow = Arc::new(Overflow::default());
    (
        EventSink {
            tx,
            overflow: overflow.clone(),
        },
        EventStream { events, overflow },
    )
}

// ---------------------------------------------------------------------------
// The platform watcher, behind a seam
// ---------------------------------------------------------------------------

/// The arming half of the platform watcher.
///
/// Behind a trait so routing, retry and overflow are testable without real
/// filesystem notifications or timing luck; the event half is the channel
/// above, which a test can feed directly.
pub trait WatchBackend: Send + 'static {
    /// Arm a recursive watch on `root`.
    fn arm(&mut self, root: &Utf8Path) -> anyhow::Result<()>;

    /// Drop any existing watch on `root`; best effort, errors are expected
    /// and ignored.
    ///
    /// Called before a *re*-arm: `notify`'s Windows backend overwrites its
    /// watch-table entry without closing the previous directory handle, so
    /// re-arming a still-live watch would leak one per attempt.
    fn disarm(&mut self, root: &Utf8Path);

    /// Release the backend; called once, when the pump stops.
    fn stop(&mut self);
}

/// The real backend: one debouncer, one recursive watch per project root.
pub struct NotifyBackend {
    debouncer: Option<Debouncer<RecommendedWatcher, RecommendedCache>>,
}

impl NotifyBackend {
    pub fn new(sink: EventSink, debounce: Duration) -> anyhow::Result<Self> {
        let debouncer = new_debouncer(debounce, None, move |batch: DebounceEventResult| {
            sink.send(batch);
        })?;
        Ok(Self {
            debouncer: Some(debouncer),
        })
    }
}

impl WatchBackend for NotifyBackend {
    fn arm(&mut self, root: &Utf8Path) -> anyhow::Result<()> {
        let debouncer = self
            .debouncer
            .as_mut()
            .context("the debouncer is already stopped")?;
        debouncer.watch(root.as_std_path(), RecursiveMode::Recursive)?;
        Ok(())
    }

    fn disarm(&mut self, root: &Utf8Path) {
        if let Some(debouncer) = self.debouncer.as_mut() {
            let _ = debouncer.unwatch(root.as_std_path());
        }
    }

    fn stop(&mut self) {
        if let Some(debouncer) = self.debouncer.take() {
            debouncer.stop_nonblocking();
        }
    }
}

// ---------------------------------------------------------------------------
// Desired vs armed watches
// ---------------------------------------------------------------------------

/// Bounded exponential backoff for arming a watch.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Delay before the second attempt; doubles from there.
    pub first: Duration,
    /// Ceiling for the doubling.
    pub max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // A reconnecting volume is usually back within seconds; a genuinely
        // absent one must not be probed every half second forever.
        Self {
            first: Duration::from_millis(500),
            max: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    fn delay(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        self.first.saturating_mul(1 << doublings).min(self.max)
    }
}

/// A pending arm attempt.
#[derive(Debug)]
struct Retry {
    due: Instant,
    /// Consecutive failed attempts; 0 for a watch that has not been tried yet.
    failures: u32,
}

/// One project the daemon has been told to watch.
#[derive(Debug)]
struct Desired {
    project: Project,
    /// `None` once armed.
    retry: Option<Retry>,
}

/// Desired watches, and which of them are actually armed.
///
/// Keeping the two apart is the whole point: a project that failed to arm is
/// still desired, still retried, and still visible.
#[derive(Debug)]
struct Watches {
    projects: BTreeMap<ProjectId, Desired>,
    policy: RetryPolicy,
    status: WatchStatus,
}

impl Watches {
    fn new(policy: RetryPolicy, status: WatchStatus) -> Self {
        Self {
            projects: BTreeMap::new(),
            policy,
            status,
        }
    }

    /// Record a project we want watched, due to be armed immediately.
    ///
    /// Idempotent by id: a repeated registration must not disturb a live watch
    /// or reset a backoff that is already in progress.
    fn want(&mut self, project: Project) {
        if self.projects.contains_key(&project.id) {
            return;
        }
        let id = project.id;
        self.projects.insert(
            id,
            Desired {
                project,
                retry: Some(Retry {
                    due: Instant::now(),
                    failures: 0,
                }),
            },
        );
        self.status.set(id, WatchState::Retrying);
    }

    /// Forget a deregistered project: disarm its root and stop reporting it.
    ///
    /// Disarming is unconditional rather than "only if armed" — the backend's
    /// `disarm` is best-effort by contract, and a watch that is mid-retry has
    /// no armed handle to distinguish anyway.
    fn forget(&mut self, backend: &mut impl WatchBackend, project: ProjectId) {
        let Some(desired) = self.projects.remove(&project) else {
            return;
        };
        backend.disarm(&desired.project.root);
        self.status.forget(project);
        tracing::info!(
            project = %desired.project.name,
            root = %desired.project.root,
            "no longer watching; the project was deregistered"
        );
    }

    /// Arm every watch whose attempt is due. Failures reschedule themselves.
    fn arm_due(&mut self, backend: &mut impl WatchBackend, now: Instant) {
        for desired in self.projects.values_mut() {
            let Some(retry) = &desired.retry else {
                continue;
            };
            if retry.due > now {
                continue;
            }
            let failures = retry.failures;
            let id = desired.project.id;
            let root = desired.project.root.clone();
            if failures > 0 {
                backend.disarm(&root);
            }
            match backend.arm(&root) {
                Ok(()) => {
                    tracing::info!(project = %desired.project.name, root = %root, "watching");
                    desired.retry = None;
                    self.status.set(id, WatchState::Armed);
                }
                Err(err) => {
                    let failures = failures + 1;
                    let delay = self.policy.delay(failures);
                    tracing::warn!(
                        project = %desired.project.name, root = %root, error = %err,
                        failures, retry_in_ms = delay.as_millis(),
                        "cannot watch project root; retrying (until then its changes are only seen on an explicit reindex)"
                    );
                    desired.retry = Some(Retry {
                        due: now + delay,
                        failures,
                    });
                    self.status.set(id, WatchState::Retrying);
                }
            }
        }
    }

    /// When [`Self::arm_due`] next has something to do.
    fn next_due(&self) -> Option<Instant> {
        self.projects
            .values()
            .filter_map(|desired| desired.retry.as_ref().map(|retry| retry.due))
            .min()
    }

    fn ids(&self) -> Vec<ProjectId> {
        self.projects.keys().copied().collect()
    }

    fn len(&self) -> usize {
        self.projects.len()
    }

    /// Every watched project containing `abs`, with the project-relative path.
    ///
    /// Projects still waiting to be armed are included: their events can reach
    /// us through an armed ancestor's recursive watch, and the work is theirs
    /// either way.
    fn containing(&self, abs: &Utf8Path) -> Vec<(&Project, Utf8PathBuf)> {
        self.projects
            .values()
            .filter_map(|desired| {
                paths::relative_to(&desired.project.root, abs).map(|rel| (&desired.project, rel))
            })
            .collect()
    }

    /// Move an armed watch back to the retry set after the platform
    /// invalidated it. A watch already awaiting retry keeps its backoff.
    fn invalidate(&mut self, project: ProjectId, now: Instant) {
        let Some(desired) = self.projects.get_mut(&project) else {
            return;
        };
        if desired.retry.is_some() {
            return;
        }
        desired.retry = Some(Retry {
            due: now + self.policy.delay(1),
            failures: 1,
        });
        self.status.set(project, WatchState::Retrying);
    }

    /// Which projects an error batch is about.
    ///
    /// Errors carry the paths they concern; when they carry none — or none we
    /// recognize — attribution is unknowable and every watched project has to
    /// be treated as suspect.
    fn affected_by(&self, errors: &[notify::Error]) -> Vec<ProjectId> {
        let mut affected = BTreeSet::new();
        for error in errors {
            for path in &error.paths {
                let Some(path) = Utf8Path::from_path(path) else {
                    continue;
                };
                for desired in self.projects.values() {
                    let root = &desired.project.root;
                    // Either direction counts: the error may name the root
                    // itself, something inside it, or an ancestor volume.
                    if paths::is_within(root, path) || paths::is_within(path, root) {
                        affected.insert(desired.project.id);
                    }
                }
            }
        }
        if affected.is_empty() {
            self.ids()
        } else {
            affected.into_iter().collect()
        }
    }
}

// ---------------------------------------------------------------------------
// The pump
// ---------------------------------------------------------------------------

/// Everything the pump needs that is not shared daemon plumbing.
pub struct Watcher<B: WatchBackend> {
    pub backend: B,
    pub events: EventStream,
    pub retry: RetryPolicy,
    pub status: WatchStatus,
}

/// Run the watcher pump until cancelled.
///
/// Returns an error only if the platform watcher itself cannot be created;
/// a root that cannot be armed is retried in the background, because one
/// unwatchable project must not cost the daemon its other projects.
pub async fn run(
    commands: WatchReceiver,
    queue: IndexQueue,
    data_dir: Utf8PathBuf,
    status: WatchStatus,
    debounce: Duration,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let (sink, events) = event_channel();
    let backend = NotifyBackend::new(sink, debounce)?;
    run_with(
        Watcher {
            backend,
            events,
            retry: RetryPolicy::default(),
            status,
        },
        commands,
        queue,
        data_dir,
        cancel,
    )
    .await
}

/// The pump proper, over an injectable backend and event stream.
pub async fn run_with<B: WatchBackend>(
    watcher: Watcher<B>,
    mut commands: WatchReceiver,
    queue: IndexQueue,
    data_dir: Utf8PathBuf,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let Watcher {
        mut backend,
        events,
        retry,
        status,
    } = watcher;
    let EventStream {
        mut events,
        overflow,
    } = events;
    let mut watches = Watches::new(retry, status);

    loop {
        // A dropped batch is a known unknown, and the only honest answer is to
        // re-derive the truth from disk. Attribution is impossible at the
        // callback — it never sees project roots — so every watched project
        // pays for the storm.
        if overflow.dropped.swap(false, Ordering::Acquire) {
            tracing::warn!(
                projects = watches.len(),
                "watcher event backlog overflowed; rescanning every watched project"
            );
            for project in watches.ids() {
                queue.request_full(project);
            }
        }
        watches.arm_due(&mut backend, Instant::now());
        let due = watches.next_due();

        tokio::select! {
            () = cancel.cancelled() => break,
            () = overflow.wake.notified() => {}
            () = sleep_until(due) => {}
            command = commands.recv() => match command {
                Some(WatchCommand::Watch(project)) => watches.want(project),
                Some(WatchCommand::Unwatch(project)) => watches.forget(&mut backend, project),
                None => break,
            },
            batch = events.recv() => match batch {
                Some(Ok(events)) => route(events, &watches, &queue, &data_dir),
                Some(Err(errors)) => recover(&errors, &mut watches, &queue),
                None => break,
            },
        }
    }

    backend.stop();
    tracing::debug!("watcher stopped");
    Ok(())
}

/// `sleep_until`, or never when there is nothing to wait for.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Turn one debounced batch into queue work.
fn route(events: Vec<DebouncedEvent>, watches: &Watches, queue: &IndexQueue, data_dir: &Utf8Path) {
    for event in events {
        if event.kind.is_access() {
            continue;
        }
        // Buffer overflow / platform-signalled loss: we cannot know what we
        // missed, so re-derive the truth from disk.
        if event.need_rescan() {
            tracing::warn!("watcher signalled rescan (event backlog overflow)");
            for project in watches.ids() {
                queue.request_full(project);
            }
            continue;
        }

        for path in &event.paths {
            let Some(abs) = Utf8Path::from_path(path) else {
                continue;
            };
            if paths::is_within(data_dir, abs) {
                continue;
            }
            // Every containing project, not just the first: nested roots are
            // two legitimate projects, and first-match leaves one of them
            // deterministically stale.
            for (project, rel) in watches.containing(abs) {
                // Policy first, before anything below can drop the event: these
                // files are not content, and an edit to one changes the meaning
                // of the files that are.
                //
                // - the ignore rules changed, so what is indexable changed with
                //   them — only a rescan can tell how. Both files, because
                //   `.loreignore` is the one a user is actually invited to edit;
                // - `.lore.toml` changed, so whether this repo has authority
                //   semantics at all may have changed (D-0012), and the
                //   project's Markdown has to be re-chunked under the new
                //   profile. Only at the root: a nested copy is not
                //   configuration.
                let name = rel.file_name();
                let ignore_rules =
                    name == Some(".gitignore") || name == Some(walk::LORE_IGNORE_FILE);
                let repo_config = rel.as_str() == crate::repo_config::REPO_CONFIG_FILE;
                if ignore_rules || repo_config {
                    tracing::info!(
                        project = %project.name,
                        file = %rel,
                        "repository policy changed; scheduling rescan"
                    );
                    queue.request_full(project.id);
                    continue;
                }
                // The hard floor is all that is rejected on the name alone —
                // and git writes to it constantly (an index lock, a ref update,
                // a reflog append), so it is worth rejecting cheaply. Every
                // other exclusion is an overridable rule (D-0020), so a
                // dot-file shortcut here would drop the events for a `.env`
                // some `.loreignore` deliberately re-included, and the file
                // would go stale in the index until the next full scan.
                if walk::is_git_metadata(&rel) {
                    continue;
                }
                queue.request_paths(project.id, [rel]);
            }
        }
    }
}

/// A watcher backend error means the event stream is no longer trustworthy
/// for the affected roots. Rescanning recovers the index; re-arming recovers
/// live indexing, and skipping it would leave the project silently frozen
/// until the next registration.
fn recover(errors: &[notify::Error], watches: &mut Watches, queue: &IndexQueue) {
    let affected = watches.affected_by(errors);
    for error in errors {
        tracing::warn!(
            error = %error,
            projects = affected.len(),
            "watcher error; scheduling rescan and re-arming"
        );
    }
    let now = Instant::now();
    for project in affected {
        queue.request_full(project);
        watches.invalidate(project, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_stops_at_the_ceiling() {
        let policy = RetryPolicy {
            first: Duration::from_millis(500),
            max: Duration::from_secs(30),
        };
        assert_eq!(policy.delay(1), Duration::from_millis(500));
        assert_eq!(policy.delay(2), Duration::from_secs(1));
        assert_eq!(policy.delay(3), Duration::from_secs(2));
        assert_eq!(policy.delay(7), Duration::from_secs(30));
        // No overflow panic however long a volume stays away.
        assert_eq!(policy.delay(u32::MAX), Duration::from_secs(30));
    }
}
