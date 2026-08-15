//! The daemon: one process, one owner of index state (D-0003, D-0007).
//!
//! ```text
//!            ┌──────────────── CancellationToken + TaskTracker ─────────────┐
//!            │                                                              │
//!  ctrl-c ──▶│  http (axum, 127.0.0.1:0)  ──┐                               │
//!            │  heartbeat (daemon.json)     ├──▶ StoreHandle (Mutex<Store>) │
//!            │  watcher pump ──▶ IndexQueue ─┤         on spawn_blocking    │
//!            │  indexer      ◀──────────────┤                               │
//!            │  embed worker ◀── Notify ────┘                               │
//!            └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! Startup order is not arbitrary:
//! 1. `ownership::acquire` — the kernel-held lock that makes this process
//!    the one owner of the data dir; every other step assumes it.
//! 2. open the store — fail before publishing a port nobody can use. The
//!    lock rides inside the [`StoreHandle`], so it outlives every task and
//!    closure that can still write, however shutdown goes.
//! 3. bind `127.0.0.1:0` — the OS assigns the port.
//! 4. publish `daemon.json` — only now is the daemon discoverable, and by
//!    then everything it advertises actually works.
//! 5. seed watches and full scans — after the API is up, so `lore status`
//!    answers immediately on a cold, large project instead of timing out.

pub mod expand;
pub mod handshake;
pub mod http;
pub mod index;
pub mod ownership;
pub mod paths;
pub mod queue;
pub mod search;
pub mod store_handle;
pub mod walk;
pub mod watch;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::Config;
use crate::embed::Embedder;
use crate::store::Project;

pub use handshake::Handshake;
pub use paths::{DATA_DIR_ENV, data_dir};
pub use store_handle::StoreHandle;

/// How long shutdown waits for in-flight work before giving up on it.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Read the running daemon's handshake record — the discovery entry point for
/// the CLI and `lore-mcp`, which must never open the database themselves.
///
/// `Ok(None)` means no daemon has ever published here. A record that exists
/// may still be stale; [`handshake::is_fresh`] and [`handshake::probe`] are
/// the two ways to ask.
pub fn discover(data_dir: &camino::Utf8Path) -> Result<Option<Handshake>> {
    handshake::read(data_dir)
}

/// Resolve a project by name first, then by numeric id.
///
/// Name-first because that is what a human types and what `search` results
/// echo back; the id path exists so scripts have a stable handle when names
/// change. A project literally named "7" therefore shadows id 7 — acceptable,
/// and the alternative (id-first) would let a rename silently retarget a
/// human's command.
pub fn resolve_project<'a>(projects: &'a [Project], key: &str) -> Option<&'a Project> {
    projects.iter().find(|p| p.name == key).or_else(|| {
        key.parse::<i64>()
            .ok()
            .and_then(|id| projects.iter().find(|p| p.id == id))
    })
}

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub data_dir: Utf8PathBuf,
    /// Additional shutdown trigger, equivalent to ctrl-c.
    ///
    /// The binary leaves this at its default and relies on the signal; having
    /// the seam means the full startup→serve→shutdown lifecycle is testable
    /// without a console, which is otherwise unreachable on Windows from a
    /// test harness.
    pub shutdown: CancellationToken,
}

impl DaemonOptions {
    pub fn new(data_dir: Utf8PathBuf) -> Self {
        Self {
            data_dir,
            shutdown: CancellationToken::new(),
        }
    }
}

/// Run the daemon in the foreground until ctrl-c.
pub async fn run(options: DaemonOptions) -> Result<()> {
    let data_dir = options.data_dir;
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating data dir {data_dir}"))?;

    let config = Arc::new(Config::load(&data_dir)?);
    let owner = ownership::acquire(&data_dir)?;
    // Holding the lock proves any existing record's writer is gone (or
    // exited without withdrawing it); it is ours to replace.
    if let Ok(Some(stale)) = handshake::read(&data_dir) {
        tracing::warn!(
            stale_pid = stale.pid,
            stale_port = stale.port,
            "replacing a previous run's discovery record"
        );
    }

    let store = StoreHandle::open(data_dir.join(paths::DB_FILE))?.with_owner(owner);

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("binding loopback listener")?;
    let port = listener.local_addr()?.port();

    let record = handshake::for_this_process(port, handshake::unix_now());
    handshake::write(&data_dir, &record)?;
    tracing::info!(
        pid = record.pid,
        port,
        api_version = record.api_version,
        version = %record.daemon_version,
        data_dir = %data_dir,
        "lore daemon listening"
    );

    let cancel = options.shutdown;
    let tracker = TaskTracker::new();
    let queue = queue::IndexQueue::new();
    let (watch_tx, watch_rx) = watch::channel();

    let ctx = index::IndexContext::new(store.clone(), data_dir.clone(), cancel.clone());
    let embed_notify = ctx.embed_notify.clone();
    tracker.spawn(index::run(ctx, queue.clone()));

    // Embeddings are optional (D-0007): with no endpoint configured there is
    // simply no worker, and `Embedder` reports Unconfigured forever.
    let embeddings = Embedder::new(&config.embeddings);
    if let Some(worker) = embeddings.worker(store.clone(), embed_notify, cancel.clone()) {
        tracker.spawn(worker.run());
    }

    {
        let queue = queue.clone();
        let data_dir = data_dir.clone();
        let cancel = cancel.clone();
        tracker.spawn(async move {
            if let Err(err) = watch::run(watch_rx, queue, data_dir, cancel).await {
                tracing::error!(error = %err, "watcher could not start; no live reindexing");
            }
        });
    }

    tracker.spawn(heartbeat(data_dir.clone(), record.clone(), cancel.clone()));

    let state = http::AppState {
        store: store.clone(),
        queue: queue.clone(),
        watch: watch_tx.clone(),
        config,
        embeddings,
        data_dir: data_dir.clone(),
    };
    let router = http::router(state);
    {
        let cancel = cancel.clone();
        tracker.spawn(async move {
            let served = axum::serve(listener, router)
                .with_graceful_shutdown(async move { cancel.cancelled().await })
                .await;
            if let Err(err) = served {
                tracing::error!(error = %err, "http server stopped");
            }
        });
    }

    // Seed: everything already registered gets watched and rescanned. A
    // daemon that was down while the disk changed is indistinguishable from a
    // first run, so both take the same path.
    match store.with(|store| store.list_projects()).await? {
        Ok(projects) => {
            tracing::info!(count = projects.len(), "restoring registered projects");
            for project in projects {
                let _ = watch_tx.send(watch::WatchCommand::Watch(project.clone()));
                queue.request_full(project.id);
            }
        }
        Err(err) => tracing::error!(error = %err, "could not list projects at startup"),
    }

    tokio::select! {
        result = tokio::signal::ctrl_c() => match result {
            Ok(()) => tracing::info!("shutdown requested"),
            Err(err) => tracing::error!(error = %err, "cannot listen for ctrl-c; shutting down"),
        },
        () = cancel.cancelled() => {}
    }

    cancel.cancel();
    tracker.close();
    if tokio::time::timeout(SHUTDOWN_GRACE, tracker.wait())
        .await
        .is_err()
    {
        // Safe to give up on: any straggler that can still write carries the
        // ownership lock inside its StoreHandle, so no successor can open
        // the store until it actually stops.
        tracing::warn!(
            grace_secs = SHUTDOWN_GRACE.as_secs(),
            "shutdown deadline passed with tasks still running; exiting anyway"
        );
    }

    // Withdrawing only unpublishes discovery; ownership ends when the last
    // StoreHandle drops. Only our own record is ours to remove.
    match handshake::remove_if_owned_by(&data_dir, record.pid) {
        Ok(true) => tracing::info!("handshake withdrawn"),
        Ok(false) => tracing::warn!("handshake file is no longer ours; leaving it in place"),
        Err(err) => tracing::warn!(error = %err, "could not withdraw handshake"),
    }
    Ok(())
}

/// Republish the handshake with a fresh `heartbeat_at` so a *live* daemon is
/// never mistaken for a crashed one.
async fn heartbeat(data_dir: Utf8PathBuf, record: Handshake, cancel: CancellationToken) {
    let mut ticker = tokio::time::interval(handshake::HEARTBEAT_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // the first tick completes immediately
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {
                let mut beat = record.clone();
                beat.heartbeat_at = handshake::unix_now();
                if let Err(err) = handshake::write(&data_dir, &beat) {
                    tracing::warn!(error = %err, "heartbeat write failed");
                }
            }
        }
    }
    tracing::debug!("heartbeat stopped");
}
