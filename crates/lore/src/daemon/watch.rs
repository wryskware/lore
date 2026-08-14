//! Filesystem watching → coalesced index work.
//!
//! One debouncer (`notify` + `notify-debouncer-full`, ~800 ms) with one
//! recursive watch per registered project root. A debouncer per project would
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
//! Own-write feedback: the data directory (SQLite + WAL + handshake, written
//! constantly) normally lives outside every project root, but a root *could*
//! be an ancestor of it, so it is filtered explicitly here as well as in the
//! walker.

use std::time::Duration;

use camino::Utf8Path;
use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::store::Project;

use super::paths;
use super::queue::IndexQueue;
use super::walk;

/// Quiet period before a burst of events is delivered. Long enough to absorb
/// an editor's write-temp-then-rename dance and a Unity asset import, short
/// enough that a save feels instant.
pub const DEBOUNCE: Duration = Duration::from_millis(800);

/// Instructions to the watcher task.
#[derive(Debug, Clone)]
pub enum WatchCommand {
    /// Start watching a project root (idempotent).
    Watch(Project),
}

pub type WatchSender = mpsc::UnboundedSender<WatchCommand>;
pub type WatchReceiver = mpsc::UnboundedReceiver<WatchCommand>;

pub fn channel() -> (WatchSender, WatchReceiver) {
    mpsc::unbounded_channel()
}

/// Run the watcher pump until cancelled.
///
/// Returns an error only if the platform watcher itself cannot be created;
/// per-root watch failures are logged and skipped, because one unwatchable
/// project must not cost the daemon its other projects.
pub async fn run(
    mut commands: WatchReceiver,
    queue: IndexQueue,
    data_dir: camino::Utf8PathBuf,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let (events_tx, mut events) = mpsc::unbounded_channel::<DebounceEventResult>();
    // The debouncer calls this from its own thread; an unbounded send is the
    // only non-blocking option and the queue downstream is what bounds memory.
    let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
        let _ = events_tx.send(result);
    })?;

    let mut watched: Vec<Project> = Vec::new();

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            command = commands.recv() => match command {
                Some(WatchCommand::Watch(project)) => {
                    if watched.iter().any(|p| p.id == project.id) {
                        continue;
                    }
                    match debouncer.watch(project.root.as_std_path(), RecursiveMode::Recursive) {
                        Ok(()) => {
                            tracing::info!(project = %project.name, root = %project.root, "watching");
                            watched.push(project);
                        }
                        Err(err) => tracing::error!(
                            project = %project.name, root = %project.root, error = %err,
                            "cannot watch project root; changes will only be seen on an explicit reindex"
                        ),
                    }
                }
                None => break,
            },
            batch = events.recv() => match batch {
                Some(batch) => handle_batch(batch, &watched, &queue, &data_dir),
                None => break,
            },
        }
    }

    debouncer.stop_nonblocking();
    tracing::debug!("watcher stopped");
    Ok(())
}

fn handle_batch(
    batch: DebounceEventResult,
    watched: &[Project],
    queue: &IndexQueue,
    data_dir: &Utf8Path,
) {
    let events = match batch {
        Ok(events) => events,
        Err(errors) => {
            // Watch errors mean the event stream is no longer trustworthy for
            // the affected roots; the only safe recovery is a rescan.
            for error in &errors {
                tracing::warn!(error = %error, "watcher error; scheduling rescan");
            }
            for project in watched {
                queue.request_full(project.id);
            }
            return;
        }
    };

    for event in events {
        if event.kind.is_access() {
            continue;
        }
        // Buffer overflow / platform-signalled loss: we cannot know what we
        // missed, so re-derive the truth from disk.
        if event.need_rescan() {
            tracing::warn!("watcher signalled rescan (event backlog overflow)");
            for project in watched {
                queue.request_full(project.id);
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
            let Some((project, rel)) = watched
                .iter()
                .find_map(|p| paths::relative_to(&p.root, abs).map(|rel| (p, rel)))
            else {
                continue;
            };
            if walk::is_excluded_rel(&rel) {
                // Exception: the ignore rules themselves changed, so what is
                // indexable changed with them — only a rescan can tell how.
                if rel.file_name() == Some(".gitignore") {
                    tracing::info!(project = %project.name, "ignore rules changed; scheduling rescan");
                    queue.request_full(project.id);
                }
                continue;
            }
            queue.request_paths(project.id, [rel]);
        }
    }
}
