//! The coalescing work queue between the watcher and the indexer.
//!
//! A channel of "index this path" messages is the obvious design and the
//! wrong one: saving a file in Unity or switching a git branch produces
//! thousands of events for hundreds of distinct paths, and a queue that grows
//! with *events* rather than with *distinct paths* is unbounded by
//! construction. So the queue is a set, not a channel — inserting the same
//! path twice costs nothing, and the debouncer's own coalescing is reinforced
//! rather than duplicated.
//!
//! Service is round-robin over project ids rather than always lowest-first:
//! two registered projects share one indexer, and a project whose files churn
//! continuously would otherwise re-enter the set before the next take and
//! starve every higher id indefinitely.
//!
//! Second bound: a project whose pending set exceeds [`MAX_PENDING_PATHS`]
//! collapses to a full rescan. Past a few thousand changed files a rescan is
//! cheaper than per-file bookkeeping anyway (hashing dominates either way),
//! and it caps memory at a constant regardless of how violent the storm is.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::store::ProjectId;

/// Above this many distinct pending paths for one project, degrade to a full
/// rescan of that project.
pub const MAX_PENDING_PATHS: usize = 4096;

/// Outstanding work for one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectWork {
    /// Rescan the whole project; `paths` is then irrelevant.
    pub full: bool,
    /// Project-relative paths to re-examine.
    pub paths: BTreeSet<Utf8PathBuf>,
    /// Apply the rescan even if it trips the mass-delete guard (D-0015).
    /// Only an explicit `lore index --allow-mass-delete` sets it, and only for
    /// the pass it queued: it is per invocation, never a stored setting.
    pub allow_mass_delete: bool,
}

#[derive(Debug, Default)]
struct Inner {
    pending: Mutex<Pending>,
    notify: Notify,
}

/// The pending set plus the round-robin cursor that keeps it fair.
#[derive(Debug, Default)]
struct Pending {
    work: BTreeMap<ProjectId, ProjectWork>,
    /// Last project handed out. The next take resumes *after* it and wraps,
    /// so a project whose files churn continuously cannot hold the indexer
    /// against a quieter project that happens to have a higher id.
    cursor: ProjectId,
}

/// Cheap to clone; every producer and the single consumer share one set.
#[derive(Clone, Debug, Default)]
pub struct IndexQueue {
    inner: Arc<Inner>,
}

impl IndexQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a whole-project rescan, superseding any pending paths.
    pub fn request_full(&self, project: ProjectId) {
        self.queue_full(project, false);
    }

    /// The same rescan, carrying an explicit human's mass-delete override
    /// (D-0015). Separate from [`Self::request_full`] so that no caller can
    /// pass the override by forgetting a `false`.
    pub fn request_full_allowing_mass_delete(&self, project: ProjectId) {
        self.queue_full(project, true);
    }

    fn queue_full(&self, project: ProjectId, allow_mass_delete: bool) {
        {
            let mut pending = self.lock();
            let work = pending.work.entry(project).or_default();
            work.full = true;
            work.paths.clear();
            // Coalescing an override with an ordinary rescan keeps it: the
            // human asked for it, and the queue must not swallow the request
            // just because the watcher happened to ask first.
            work.allow_mass_delete |= allow_mass_delete;
        }
        self.inner.notify.notify_one();
    }

    /// Queue specific project-relative paths.
    pub fn request_paths(&self, project: ProjectId, paths: impl IntoIterator<Item = Utf8PathBuf>) {
        let mut added = false;
        {
            let mut pending = self.lock();
            let work = pending.work.entry(project).or_default();
            for path in paths {
                added = true;
                if work.full {
                    break;
                }
                work.paths.insert(path);
            }
            if work.paths.len() > MAX_PENDING_PATHS {
                tracing::warn!(
                    project_id = project,
                    pending = work.paths.len(),
                    "change storm exceeded the pending-path bound; degrading to a full rescan"
                );
                work.full = true;
                work.paths.clear();
            }
        }
        if added {
            self.inner.notify.notify_one();
        }
    }

    /// Take all outstanding work for one project, round-robin by id.
    /// Non-blocking; `None` when the queue is empty.
    pub fn take(&self) -> Option<(ProjectId, ProjectWork)> {
        let mut pending = self.lock();
        let after = pending.cursor;
        let project = pending
            .work
            .range((Bound::Excluded(after), Bound::Unbounded))
            .next()
            .or_else(|| pending.work.iter().next())
            .map(|(id, _)| *id)?;
        let work = pending.work.remove(&project)?;
        pending.cursor = project;
        Some((project, work))
    }

    /// Await the next unit of work, or `None` once `cancel` fires.
    pub async fn next(&self, cancel: &CancellationToken) -> Option<(ProjectId, ProjectWork)> {
        loop {
            // Register interest *before* checking, so a `request_*` racing
            // with this check cannot be missed. (`Notify` also stores one
            // permit, which covers a notify that lands even earlier.)
            let notified = self.inner.notify.notified();
            if let Some(work) = self.take() {
                return Some(work);
            }
            tokio::select! {
                () = notified => {}
                () = cancel.cancelled() => return None,
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lock().work.is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.inner
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> Utf8PathBuf {
        Utf8PathBuf::from(name)
    }

    #[test]
    fn repeated_paths_coalesce() {
        let queue = IndexQueue::new();
        for _ in 0..1000 {
            queue.request_paths(1, [path("src/a.rs"), path("src/b.rs")]);
        }
        let (project, work) = queue.take().expect("work queued");
        assert_eq!(project, 1);
        assert!(!work.full);
        assert_eq!(work.paths.len(), 2);
        assert!(queue.is_empty());
    }

    #[test]
    fn full_rescan_supersedes_paths_in_both_orders() {
        let queue = IndexQueue::new();
        queue.request_paths(1, [path("a")]);
        queue.request_full(1);
        let (_, work) = queue.take().unwrap();
        assert!(work.full && work.paths.is_empty());

        queue.request_full(2);
        queue.request_paths(2, [path("a")]);
        let (_, work) = queue.take().unwrap();
        assert!(work.full && work.paths.is_empty());
    }

    #[test]
    fn storm_beyond_the_bound_degrades_to_full_rescan() {
        let queue = IndexQueue::new();
        queue.request_paths(
            1,
            (0..MAX_PENDING_PATHS + 1).map(|i| path(&format!("f{i}"))),
        );
        let (_, work) = queue.take().unwrap();
        assert!(work.full, "pending set is bounded");
        assert!(work.paths.is_empty());
    }

    /// A project saving files in a loop re-enters the pending set between
    /// every take. Lowest-id-first service would hand it the indexer forever;
    /// the rotation must reach the quiet project on the very next take.
    #[test]
    fn a_churning_project_cannot_starve_a_higher_id() {
        let queue = IndexQueue::new();
        queue.request_paths(2, [path("quiet.rs")]);

        let mut served = Vec::new();
        for _ in 0..4 {
            queue.request_paths(1, [path("busy.rs")]);
            let (project, work) = queue.take().expect("work queued");
            assert!(!work.paths.is_empty(), "work is never handed out empty");
            served.push(project);
        }
        assert_eq!(served, [1, 2, 1, 1], "service rotates before repeating");
    }

    /// Rotation is scheduling only: everything queued for one project still
    /// arrives in a single take.
    #[test]
    fn rotation_preserves_per_project_coalescing() {
        let queue = IndexQueue::new();
        for _ in 0..100 {
            queue.request_paths(1, [path("a.rs"), path("b.rs")]);
            queue.request_paths(2, [path("c.rs")]);
        }
        let (first, work) = queue.take().unwrap();
        assert_eq!((first, work.paths.len()), (1, 2));
        let (second, work) = queue.take().unwrap();
        assert_eq!((second, work.paths.len()), (2, 1));
        assert!(queue.is_empty());
    }

    #[tokio::test]
    async fn next_wakes_on_work_and_returns_none_on_cancel() {
        let queue = IndexQueue::new();
        let cancel = CancellationToken::new();

        queue.request_paths(7, [path("a")]);
        let (project, _) = queue.next(&cancel).await.expect("queued work");
        assert_eq!(project, 7);

        cancel.cancel();
        assert!(queue.next(&cancel).await.is_none());
    }

    /// The notify permit must survive a `request_*` that lands before any
    /// consumer is waiting — otherwise a quiet project's first edit sits in
    /// the queue until an unrelated event wakes the indexer.
    #[tokio::test]
    async fn work_queued_before_the_consumer_waits_is_still_delivered() {
        let queue = IndexQueue::new();
        let cancel = CancellationToken::new();
        let consumer = {
            let queue = queue.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { queue.next(&cancel).await })
        };
        tokio::task::yield_now().await;
        queue.request_paths(3, [path("late.rs")]);
        let (project, work) = consumer.await.unwrap().expect("delivered");
        assert_eq!(project, 3);
        assert_eq!(work.paths.len(), 1);
    }
}
