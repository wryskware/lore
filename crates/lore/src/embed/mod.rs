//! Local embeddings: client, health, background worker, text construction.
//!
//! Canon this module exists to satisfy:
//!
//! - **D-0003** — embeddings are *local only*. The endpoint is an
//!   OpenAI-compatible server on loopback; there is no cloud provider path and
//!   reqwest is pinned without TLS to make that structural rather than
//!   aspirational.
//! - **D-0007** — an absent or unhealthy endpoint degrades search to
//!   lexical-only, and the degradation is visible in `GET /v1/status`. Nothing
//!   here ever fails a search; it only ever declines to contribute vectors.
//! - **3.1** — prefixed embedding text, provider config shape, and a persisted
//!   fingerprint that forces an explicit re-embed rather than silent mixing of
//!   two vector spaces.
//!
//! # Shape
//!
//! ```text
//!            ┌──────────── Embedder (cloned into AppState) ───────────┐
//!            │  Option<Arc<EmbedClient>>          Health (RwLock)     │
//!            └───────┬───────────────────────────────────┬────────────┘
//!  /v1/search ───────┘  embed_query (3s cap, fail→lexical)│ read by /v1/status
//!  daemon child ─── EmbedWorker::run ─────────────────────┘ written by probes
//! ```
//!
//! [`Embedder`] is the only type the rest of the daemon needs: it is cheap to
//! clone, safe to hold in `AppState`, and answers "are vectors available?"
//! without knowing anything about HTTP.

pub mod client;
pub mod health;
pub mod text;
pub mod worker;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use lore_core::EmbeddingStatus;

use crate::config::EmbeddingsConfig;
use crate::daemon::store_handle::StoreHandle;

pub use client::{EmbedClient, EmbedError, EmbedSettings, RetryPolicy};
pub use health::Health;
pub use worker::{EmbedWorker, fingerprint};

/// Ceiling on embedding a *query*. Search is interactive: three seconds is
/// already a long time to wait, and past it lexical-only results now beat
/// hybrid results later (D-0007's degradation is the designed behaviour, not
/// an emergency).
pub const QUERY_EMBED_TIMEOUT: Duration = Duration::from_secs(3);

/// The daemon's embedding capability: a client (or not) plus shared health.
#[derive(Clone, Debug)]
pub struct Embedder {
    client: Option<Arc<EmbedClient>>,
    health: Health,
}

impl Embedder {
    /// Build from configuration. Never fails: a broken configuration becomes
    /// a *visible* unreachable state, because refusing to start the daemon
    /// over an optional feature would be worse than running without it.
    pub fn new(config: &EmbeddingsConfig) -> Self {
        let health = Health::new(config_status(config));
        let Some(settings) = EmbedSettings::from_config(config) else {
            return Self {
                client: None,
                health,
            };
        };
        if config.model.is_none() {
            tracing::info!(
                model = settings.model.as_str(),
                "no embedding model configured; sending the default model id"
            );
        }
        match EmbedClient::new(settings.clone()) {
            Ok(client) => Self {
                client: Some(Arc::new(client)),
                health,
            },
            Err(err) => {
                health.set_unreachable(&settings.endpoint, err.to_string());
                Self {
                    client: None,
                    health,
                }
            }
        }
    }

    /// An embedder that will never produce vectors — the lexical-only daemon.
    pub fn disabled() -> Self {
        Self {
            client: None,
            health: Health::new(EmbeddingStatus::Unconfigured),
        }
    }

    /// Test/embedding-side seam: build directly from settings, bypassing the
    /// config file.
    pub fn from_settings(settings: EmbedSettings) -> Self {
        let health = Health::new(EmbeddingStatus::Unreachable {
            endpoint: settings.endpoint.clone(),
            error: "endpoint not probed yet; search is lexical-only until it answers".to_string(),
        });
        match EmbedClient::new(settings.clone()) {
            Ok(client) => Self {
                client: Some(Arc::new(client)),
                health,
            },
            Err(err) => {
                health.set_unreachable(&settings.endpoint, err.to_string());
                Self {
                    client: None,
                    health,
                }
            }
        }
    }

    /// What `GET /v1/status` reports.
    pub fn status(&self) -> EmbeddingStatus {
        self.health.status()
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    pub fn client(&self) -> Option<&Arc<EmbedClient>> {
        self.client.as_ref()
    }

    /// Probe now and publish the result. Returns readiness.
    pub async fn refresh(&self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        match client.probe().await {
            Ok(_) => {
                self.health.set_ready(client.endpoint(), client.model());
                true
            }
            Err(err) => {
                self.health
                    .set_unreachable(client.endpoint(), err.to_string());
                false
            }
        }
    }

    /// Embed a search query, or `None` — which the search path reads as "run
    /// lexical-only for this request".
    ///
    /// A failure here also demotes health, so a server that died between the
    /// last probe and this request costs one slow search rather than one slow
    /// search per query until the worker notices.
    pub async fn embed_query(&self, query: &str) -> Option<Vec<f32>> {
        let client = self.client.as_ref()?;
        if !self.health.is_ready() || query.trim().is_empty() {
            return None;
        }
        let text = text::query_text(query, &client.settings().query_prefix);
        match tokio::time::timeout(QUERY_EMBED_TIMEOUT, client.embed(&[text])).await {
            Ok(Ok(vectors)) => vectors.into_iter().next(),
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "query embedding failed; this search is lexical-only");
                self.health
                    .set_unreachable(client.endpoint(), err.to_string());
                None
            }
            Err(_) => {
                tracing::debug!(
                    timeout_ms = QUERY_EMBED_TIMEOUT.as_millis() as u64,
                    "query embedding timed out; this search is lexical-only"
                );
                self.health.set_unreachable(
                    client.endpoint(),
                    format!(
                        "query embedding timed out after {}ms",
                        QUERY_EMBED_TIMEOUT.as_millis()
                    ),
                );
                None
            }
        }
    }

    /// The background worker, or `None` when there is nothing to run.
    pub fn worker(
        &self,
        store: StoreHandle,
        notify: Arc<Notify>,
        cancel: CancellationToken,
    ) -> Option<EmbedWorker> {
        let client = self.client.clone()?;
        Some(EmbedWorker::new(
            store,
            client,
            self.health.clone(),
            notify,
            cancel,
        ))
    }
}

/// Pre-probe status implied by configuration alone.
fn config_status(config: &EmbeddingsConfig) -> EmbeddingStatus {
    crate::config::Config {
        embeddings: config.clone(),
    }
    .embedding_status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unconfigured_embedder_is_inert() {
        let embedder = Embedder::new(&EmbeddingsConfig::default());
        assert!(matches!(embedder.status(), EmbeddingStatus::Unconfigured));
        assert!(embedder.embed_query("anything").await.is_none());
        assert!(!embedder.refresh().await);
        assert!(embedder.client().is_none());
        assert!(Embedder::disabled().client().is_none());
    }

    #[tokio::test]
    async fn a_configured_endpoint_starts_visibly_degraded_not_ready() {
        let config = EmbeddingsConfig {
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            ..EmbeddingsConfig::default()
        };
        let embedder = Embedder::new(&config);
        match embedder.status() {
            EmbeddingStatus::Unreachable { endpoint, error } => {
                assert_eq!(endpoint, "http://127.0.0.1:9/v1");
                assert!(!error.is_empty());
            }
            other => panic!("expected a visibly degraded start, got {other:?}"),
        }
        // Not ready ⇒ no query embedding is even attempted.
        assert!(embedder.embed_query("anything").await.is_none());
    }
}
