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

use std::sync::{Arc, RwLock};

use lore_core::EmbeddingStatus;

#[derive(Clone, Debug)]
pub struct Health {
    inner: Arc<RwLock<EmbeddingStatus>>,
}

impl Health {
    pub fn new(initial: EmbeddingStatus) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial)),
        }
    }

    pub fn status(&self) -> EmbeddingStatus {
        self.read().clone()
    }

    /// True only when vectors may participate in ranking.
    pub fn is_ready(&self) -> bool {
        matches!(*self.read(), EmbeddingStatus::Ready { .. })
    }

    pub fn set_ready(&self, endpoint: &str, model: &str) {
        self.set(EmbeddingStatus::Ready {
            endpoint: endpoint.to_string(),
            model: model.to_string(),
        });
    }

    pub fn set_unreachable(&self, endpoint: &str, error: impl Into<String>) {
        self.set(EmbeddingStatus::Unreachable {
            endpoint: endpoint.to_string(),
            error: error.into(),
        });
    }

    fn set(&self, next: EmbeddingStatus) {
        let mut guard = self.write();
        if class(&guard) != class(&next) {
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
        *guard = next;
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, EmbeddingStatus> {
        self.inner.read().unwrap_or_else(|poisoned| {
            tracing::error!("embedding health lock was poisoned; continuing");
            poisoned.into_inner()
        })
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, EmbeddingStatus> {
        self.inner.write().unwrap_or_else(|poisoned| {
            tracing::error!("embedding health lock was poisoned; continuing");
            poisoned.into_inner()
        })
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
}
