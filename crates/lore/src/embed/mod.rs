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
//!  /v1/search ───────┘  embed_query (5s cap, fail→lexical)│ read by /v1/status
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
pub use health::{Health, Ticket};
pub use worker::{EmbedWorker, fingerprint, fingerprint_of};

/// Ceiling on embedding a *query*. Search is interactive: past this,
/// lexical-only results now beat hybrid results later (D-0007's degradation is
/// the designed behaviour, not an emergency).
///
/// Five seconds rather than three because exceeding it no longer costs
/// anything but this one query's recall (see [`Embedder::embed_query`]): the
/// only thing a shorter ceiling buys is a faster answer, and the only thing it
/// costs is the vector arm on every query that lands while a model is
/// reloading after an idle timeout, or behind a worker batch on a busy local
/// server. Deliberately not configurable — a knob whose worst setting is
/// invisible (silently lexical-only answers) is worse than a constant that is
/// merely imperfect.
pub const QUERY_EMBED_TIMEOUT: Duration = Duration::from_secs(5);

/// The daemon's embedding capability: a client (or not) plus shared health.
#[derive(Clone, Debug)]
pub struct Embedder {
    client: Option<Arc<EmbedClient>>,
    health: Health,
    query_timeout: Duration,
}

impl Embedder {
    fn assembled(client: Option<Arc<EmbedClient>>, health: Health) -> Self {
        Self {
            client,
            health,
            query_timeout: QUERY_EMBED_TIMEOUT,
        }
    }

    /// Build from configuration. Never fails: a broken configuration becomes
    /// a *visible* unreachable state, because refusing to start the daemon
    /// over an optional feature would be worse than running without it.
    pub fn new(config: &EmbeddingsConfig) -> Self {
        let health = Health::new(config_status(config));
        let Some(settings) = EmbedSettings::from_config(config) else {
            return Self::assembled(None, health);
        };
        if config.model.is_none() {
            tracing::info!(
                model = settings.model.as_str(),
                "no embedding model configured; sending the default model id"
            );
        }
        match EmbedClient::new(settings.clone()) {
            Ok(client) => Self::assembled(Some(Arc::new(client)), health),
            Err(err) => {
                health.set_unreachable(&settings.endpoint, err.to_string());
                Self::assembled(None, health)
            }
        }
    }

    /// An embedder that will never produce vectors — the lexical-only daemon.
    pub fn disabled() -> Self {
        Self::assembled(None, Health::new(EmbeddingStatus::Unconfigured))
    }

    /// Test/embedding-side seam: build directly from settings, bypassing the
    /// config file.
    pub fn from_settings(settings: EmbedSettings) -> Self {
        let health = Health::new(EmbeddingStatus::Unreachable {
            endpoint: settings.endpoint.clone(),
            error: "endpoint not probed yet; search is lexical-only until it answers".to_string(),
        });
        match EmbedClient::new(settings.clone()) {
            Ok(client) => Self::assembled(Some(Arc::new(client)), health),
            Err(err) => {
                health.set_unreachable(&settings.endpoint, err.to_string());
                Self::assembled(None, health)
            }
        }
    }

    /// Shorten the query-embed ceiling. Exists so a test can drive the timeout
    /// path in milliseconds instead of seconds; the daemon always runs with
    /// [`QUERY_EMBED_TIMEOUT`].
    pub fn set_query_timeout(&mut self, timeout: Duration) {
        self.query_timeout = timeout;
    }

    /// What `GET /v1/status` reports.
    pub fn status(&self) -> EmbeddingStatus {
        self.health.status()
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    /// Chunks the embed worker has abandoned this process lifetime — what
    /// `GET /v1/status` reports alongside [`Self::status`]. Zero when there is
    /// no worker at all, which is the truth: nothing has been given up on.
    pub fn abandoned_chunks(&self) -> u64 {
        self.health.abandoned()
    }

    pub fn client(&self) -> Option<&Arc<EmbedClient>> {
        self.client.as_ref()
    }

    /// Probe now and publish the result. Returns readiness.
    pub async fn refresh(&self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        let ticket = self.health.ticket();
        match client.probe().await {
            Ok(_) => {
                ticket.set_ready(client.endpoint(), client.model());
                true
            }
            Err(err) => {
                ticket.set_unreachable(client.endpoint(), err.to_string());
                false
            }
        }
    }

    /// Embed a search query, or `None` — which the search path reads as "run
    /// lexical-only for this request".
    ///
    /// A *failure* here — refused, unreachable, still failing after every
    /// retry — also demotes health, so a server that died between the last
    /// probe and this request costs one slow search rather than one slow
    /// search per query until the worker notices. The demotion goes through a
    /// ticket taken before the request: this is the slowest health writer in
    /// the daemon, and a five-second-old verdict must not overwrite a probe
    /// that has since said the endpoint is back. It also raises
    /// [`Health::request_probe`], because publishing a demotion is not the
    /// same as arranging for anyone to revisit it — see the note at the
    /// demotion itself.
    ///
    /// A *timeout* does not demote, and that asymmetry is the point. Running
    /// out of this request's patience is an observation about the deadline,
    /// not about the endpoint: a model reloading after an idle timeout looks
    /// exactly like one that has died, and demoting on it made the first
    /// search after an idle period flap the daemon between `Ready` and
    /// `Unreachable` (#5). The query still degrades to lexical-only, visibly,
    /// via the `lexical_only` flag on the response. What replaces the demotion
    /// is [`Health::request_probe`]: the worker probes immediately instead of
    /// waiting out its idle tick, so an endpoint that really is gone is
    /// reported within one probe rather than one minute — and because that
    /// probe *is* an embedding call, it doubles as the warm-up that makes the
    /// next query fast.
    pub async fn embed_query(&self, query: &str) -> Option<Vec<f32>> {
        let client = self.client.as_ref()?;
        if !self.health.is_ready() || query.trim().is_empty() {
            return None;
        }
        let text = text::query_text(query, &client.settings().query_prefix);
        let ticket = self.health.ticket();
        match tokio::time::timeout(self.query_timeout, client.embed(&[text])).await {
            Ok(Ok(vectors)) => vectors.into_iter().next(),
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "query embedding failed; this search is lexical-only");
                ticket.set_unreachable(client.endpoint(), err.to_string());
                // Publishing the demotion is not the same as acting on it: a
                // worker parked in its select is watching the indexer pulse,
                // the probe request, the idle tick and cancellation — a health
                // write wakes none of them. Without this ask the endpoint stays
                // reported unreachable, and every search stays lexical-only,
                // until the 60s fallback tick.
                self.health.request_probe();
                None
            }
            Err(_) => {
                tracing::debug!(
                    timeout_ms = self.query_timeout.as_millis() as u64,
                    "query embedding timed out; this search is lexical-only and the endpoint will be re-probed"
                );
                self.health.request_probe();
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
        ..Default::default()
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
