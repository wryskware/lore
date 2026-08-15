//! The single owner of index state (D-0003, D-0007).
//!
//! [`crate::store::Store`] is synchronous, `Send` but not `Sync`, and every
//! mutating method takes `&mut self`. That is a feature: the borrow checker
//! enforces write serialization. The daemon's job is to give that type
//! exactly one home and never block a runtime thread on it.
//!
//! Mechanism: `Arc<std::sync::Mutex<Store>>`, entered only from
//! [`tokio::task::spawn_blocking`]. `Mutex<T>` is `Sync` whenever `T: Send`,
//! so the handle is freely cloneable into every task and axum handler, while
//! the guard — which is *not* `Send` — is created and dropped inside one
//! blocking closure and can never be held across an `.await`. That is a
//! compile-time guarantee, not a convention.
//!
//! Granularity is one lock acquisition per store call, not per index pass:
//! a full scan of a large project would otherwise starve `/v1/search` for
//! its entire duration. The indexer therefore runs *inside* one blocking
//! task (so a pass is not thousands of task spawns) but takes and releases
//! the lock per file, which is why [`StoreHandle::blocking`] exists next to
//! [`StoreHandle::with`].
//!
//! Alternative considered and rejected: a dedicated owner task fed by a
//! command channel. It needs an enum arm (or a boxed closure) per store call,
//! and buys nothing here — the mutex already serializes writes, and
//! `spawn_blocking` already keeps the reactor free.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use super::ownership::OwnershipGuard;
use crate::store::Store;

#[derive(Clone)]
pub struct StoreHandle {
    inner: Arc<Mutex<Store>>,
    /// The process-lifetime ownership lock (D-0003), when this handle is the
    /// daemon's. It rides with every clone — including one captured by a
    /// blocking closure a shutdown timeout left behind — so the lock cannot
    /// be released while any code that can still write the store exists.
    /// `None` in unit-test fixtures that own their temp dir outright.
    /// Never read — being dropped last is its entire job.
    _owner: Option<Arc<OwnershipGuard>>,
}

impl std::fmt::Debug for StoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreHandle").finish_non_exhaustive()
    }
}

impl StoreHandle {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let path = db_path.as_ref();
        let store =
            Store::open(path).with_context(|| format!("opening store at {}", path.display()))?;
        Ok(Self::new(store))
    }

    pub fn new(store: Store) -> Self {
        Self {
            inner: Arc::new(Mutex::new(store)),
            _owner: None,
        }
    }

    /// Attach the daemon's ownership lock so it lives exactly as long as the
    /// last handle that can reach the store.
    pub fn with_owner(mut self, owner: OwnershipGuard) -> Self {
        self._owner = Some(Arc::new(owner));
        self
    }

    /// Run `f` against the store on the current (blocking) thread.
    ///
    /// Only call this from inside `spawn_blocking` or a plain thread — never
    /// from async context, where it would park a runtime worker.
    ///
    /// Lock poisoning is recovered rather than propagated: a panic inside one
    /// store call (a bug we want to see in logs) must not take the whole
    /// daemon's index offline. SQLite state itself is transaction-protected,
    /// so the connection remains usable.
    pub fn blocking<T>(&self, f: impl FnOnce(&mut Store) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::error!("store mutex was poisoned by a panicking call; continuing");
            poisoned.into_inner()
        });
        f(&mut guard)
    }

    /// Async entry point: hand `f` to the blocking pool.
    ///
    /// Panics inside `f` surface as an error rather than aborting the caller.
    pub async fn with<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Store) -> T + Send + 'static,
        T: Send + 'static,
    {
        let handle = self.clone();
        tokio::task::spawn_blocking(move || handle.blocking(f))
            .await
            .context("store task failed")
    }
}
