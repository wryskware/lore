//! Shared embedding health, the thing `GET /v1/status` reports.
//!
//! D-0007 says an absent or unhealthy endpoint degrades search to lexical-only
//! *visibly*. That makes health a first-class piece of daemon state rather
//! than an implementation detail of the worker: the status handler, the search
//! path and the embed worker all read it, and two of them write it.
//!
//! Mechanism is a `std::sync::RwLock` behind an `Arc`. Reads are a handful of
//! nanoseconds and the guard never crosses an `.await` (every accessor takes
//! and drops it inside one synchronous call), so no async lock is warranted.
//! Transitions are logged once per *change*, not per probe — the worker
//! re-probes an unreachable endpoint forever, and a log line per attempt would
//! bury everything else.
//!
//! # Why observations are versioned
//!
//! Health is written by concurrent requests of very different durations: a
//! probe answers in milliseconds, a query embedding may stall for three. Plain
//! last-writer-wins therefore lets a *stale* verdict land last — an older
//! query's timeout overwriting a newer probe's `Ready` — and D-0007 requires
//! the reported state to be the current one, not merely the most recently
//! written one. A writer about to make a slow observation takes a [`Ticket`]
//! *before* the request; publishing through a ticket that a newer observation
//! has already overtaken is dropped.
//!
//! # Why a request to re-probe lives here
//!
//! Some observations are too weak to publish but too strong to ignore: a query
//! embedding that ran out of *its* patience says nothing about the endpoint —
//! a model reloading after an idle timeout looks exactly like one that has
//! died — but it is ample reason to go and ask. [`Health::request_probe`] is
//! that ask, and it belongs next to the state it doubts: both sides of the
//! question already hold this handle, so nothing new has to be threaded from
//! the search path to the worker.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::Notify;

use lore_core::EmbeddingStatus;

#[derive(Debug)]
struct Observed {
    status: EmbeddingStatus,
    /// Ticket of the writer that published `status`.
    epoch: u64,
}

#[derive(Debug)]
struct Inner {
    observed: RwLock<Observed>,
    /// Ticket dispenser. Monotonic for the life of the process.
    next: AtomicU64,
    /// Raised when someone wants the stored verdict re-checked now.
    probe_request: Notify,
}

#[derive(Clone, Debug)]
pub struct Health {
    inner: Arc<Inner>,
}

/// A claim on a future health observation, taken **before** the request that
/// will produce it. Publishing through it is a no-op once a newer observation
/// has landed.
#[derive(Clone, Debug)]
pub struct Ticket {
    health: Health,
    epoch: u64,
}

impl Ticket {
    pub fn set_ready(&self, endpoint: &str, model: &str) {
        self.health.set(self.epoch, ready(endpoint, model));
    }

    pub fn set_unreachable(&self, endpoint: &str, error: impl Into<String>) {
        self.health.set(self.epoch, unreachable(endpoint, error));
    }
}

impl Health {
    pub fn new(initial: EmbeddingStatus) -> Self {
        Self {
            inner: Arc::new(Inner {
                observed: RwLock::new(Observed {
                    status: initial,
                    epoch: 0,
                }),
                next: AtomicU64::new(0),
                probe_request: Notify::new(),
            }),
        }
    }

    pub fn status(&self) -> EmbeddingStatus {
        self.read().status.clone()
    }

    /// True only when vectors may participate in ranking.
    pub fn is_ready(&self) -> bool {
        matches!(self.read().status, EmbeddingStatus::Ready { .. })
    }

    /// Claim the next epoch. Take this before starting the request whose
    /// outcome it will publish, never after that request completes.
    pub fn ticket(&self) -> Ticket {
        Ticket {
            health: self.clone(),
            epoch: self.claim(),
        }
    }

    /// Publish now. For observations that cannot be stale: configuration-time
    /// state, and results already in hand when the write happens.
    pub fn set_ready(&self, endpoint: &str, model: &str) {
        self.set(self.claim(), ready(endpoint, model));
    }

    pub fn set_unreachable(&self, endpoint: &str, error: impl Into<String>) {
        self.set(self.claim(), unreachable(endpoint, error));
    }

    /// Ask whoever probes this endpoint to do so now, without publishing an
    /// opinion of your own. Cheap and idempotent: requests made while the
    /// prober is busy collapse into the one it will honour next.
    pub fn request_probe(&self) {
        self.inner.probe_request.notify_one();
    }

    /// Resolves once [`Health::request_probe`] has been called. A request
    /// raised while nobody is waiting is remembered, so one made during a
    /// drain is honoured at the end of it rather than lost.
    pub async fn probe_requested(&self) {
        self.inner.probe_request.notified().await;
    }

    fn claim(&self) -> u64 {
        self.inner.next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Store `next` unless a strictly newer observation already landed. Equal
    /// epochs win so one ticket may publish more than once.
    fn set(&self, epoch: u64, next: EmbeddingStatus) {
        let mut guard = self.write();
        if epoch < guard.epoch {
            tracing::trace!(
                epoch,
                stored = guard.epoch,
                "discarded a health observation older than the stored one"
            );
            return;
        }
        if class(&guard.status) != class(&next) {
            match &next {
                EmbeddingStatus::Ready { endpoint, model } => {
                    tracing::info!(
                        endpoint,
                        model,
                        "embedding endpoint is healthy; hybrid search enabled"
                    )
                }
                EmbeddingStatus::Unreachable { endpoint, error } => tracing::warn!(
                    endpoint,
                    error,
                    "embedding endpoint unavailable; search degrades to lexical-only (D-0007)"
                ),
                EmbeddingStatus::Unconfigured => {
                    tracing::info!("no embedding endpoint configured; search is lexical-only")
                }
            }
        }
        *guard = Observed {
            status: next,
            epoch,
        };
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Observed> {
        self.inner.observed.read().unwrap_or_else(|poisoned| {
            tracing::error!("embedding health lock was poisoned; continuing");
            poisoned.into_inner()
        })
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Observed> {
        self.inner.observed.write().unwrap_or_else(|poisoned| {
            tracing::error!("embedding health lock was poisoned; continuing");
            poisoned.into_inner()
        })
    }
}

fn ready(endpoint: &str, model: &str) -> EmbeddingStatus {
    EmbeddingStatus::Ready {
        endpoint: endpoint.to_string(),
        model: model.to_string(),
    }
}

fn unreachable(endpoint: &str, error: impl Into<String>) -> EmbeddingStatus {
    EmbeddingStatus::Unreachable {
        endpoint: endpoint.to_string(),
        error: error.into(),
    }
}

/// Which of the three states this is, ignoring the payload. Used so a changed
/// *error string* on a still-unreachable endpoint does not re-log.
fn class(status: &EmbeddingStatus) -> u8 {
    match status {
        EmbeddingStatus::Unconfigured => 0,
        EmbeddingStatus::Unreachable { .. } => 1,
        EmbeddingStatus::Ready { .. } => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_are_observable() {
        let health = Health::new(EmbeddingStatus::Unconfigured);
        assert!(!health.is_ready());

        health.set_ready("http://127.0.0.1:8080/v1", "jina");
        assert!(health.is_ready());
        match health.status() {
            EmbeddingStatus::Ready { endpoint, model } => {
                assert_eq!(endpoint, "http://127.0.0.1:8080/v1");
                assert_eq!(model, "jina");
            }
            other => panic!("expected ready, got {other:?}"),
        }

        health.set_unreachable("http://127.0.0.1:8080/v1", "connection refused");
        assert!(!health.is_ready());
    }

    /// The S2#8 race, in the order it happens: a long query starts, a batch
    /// failure and then a successful probe both complete, and only then does
    /// the query's own failure arrive. It must not resurrect the batch's.
    #[test]
    fn an_older_observation_cannot_overwrite_a_newer_one() {
        let health = Health::new(EmbeddingStatus::Unconfigured);
        let query = health.ticket();

        let batch = health.ticket();
        batch.set_unreachable("http://e/v1", "batch failed");
        let probe = health.ticket();
        probe.set_ready("http://e/v1", "jina");

        query.set_unreachable("http://e/v1", "query embedding failed");
        assert!(health.is_ready(), "a stale failure demoted a newer probe");

        // The newest ticket still wins, so a real outage is still reported.
        health.ticket().set_unreachable("http://e/v1", "gone");
        assert!(!health.is_ready());
    }

    /// A re-probe request is remembered when nobody is waiting for it — the
    /// worker is usually mid-drain when a query gives up — and several
    /// requests cost one probe, not one each.
    #[tokio::test]
    async fn a_probe_request_survives_having_no_listener() {
        let health = Health::new(EmbeddingStatus::Unconfigured);
        health.request_probe();
        health.request_probe();

        tokio::time::timeout(std::time::Duration::from_secs(1), health.probe_requested())
            .await
            .expect("the request made before anyone waited was remembered");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                health.probe_requested()
            )
            .await
            .is_err(),
            "two requests must not cost two probes"
        );
    }

    /// A ticket may publish repeatedly (a retrying writer), and an unversioned
    /// `set_*` is always the newest observation.
    #[test]
    fn a_ticket_may_republish_and_direct_sets_always_win() {
        let health = Health::new(EmbeddingStatus::Unconfigured);
        let ticket = health.ticket();
        ticket.set_unreachable("http://e/v1", "first");
        ticket.set_ready("http://e/v1", "jina");
        assert!(health.is_ready());

        health.set_unreachable("http://e/v1", "config error");
        assert!(!health.is_ready());
        ticket.set_ready("http://e/v1", "jina");
        assert!(!health.is_ready(), "the older ticket lost, as it should");
    }
}
