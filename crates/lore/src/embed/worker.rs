//! The embed worker: a daemon child task that keeps stored vectors caught up
//! with stored chunks.
//!
//! # Lifecycle
//!
//! ```text
//! start ─▶ reconcile fingerprint ─▶ ┌─▶ probe (backoff while unreachable)
//!                                   │        │ ready
//!                                   │        ▼
//!                                   │      drain: chunks_missing_embeddings
//!                                   │             → embed → upsert_embeddings
//!                                   │        │ nothing left
//!                                   │        ▼
//!                                   └──── wait: indexer pulse | 60s tick | cancel
//! ```
//!
//! # Why it cannot starve search
//!
//! Every store touch goes through [`StoreHandle::with`], which runs on the
//! blocking pool and takes the store lock for exactly one call — one batch
//! fetch, one batch upsert. A full backlog is therefore thousands of short
//! lock acquisitions interleaved with `/v1/search`, not one long one. Network
//! time, which dominates, holds no lock at all.
//!
//! # Poison chunks
//!
//! A batch the endpoint answers with a non-retryable 4xx is remembered and
//! never re-sent, because `chunks_missing_embeddings` would otherwise hand it
//! back forever and the entire backlog behind it would never drain. The same
//! failure also marks health unreachable: a 4xx is far more often a
//! configuration error (wrong model id) affecting *every* batch than one bad
//! input, and the next probe distinguishes the two — if the endpoint answers a
//! trivial probe, the problem really was that input and draining resumes past
//! it; if it does not, the daemon is visibly degraded in `/v1/status` instead
//! of quietly poisoning the corpus 64 chunks at a time.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::daemon::store_handle::StoreHandle;
use crate::store::{EmbedCandidate, EmbeddingFingerprint, NewEmbedding, ProjectId};
use crate::types::ChunkId;

use super::client::EmbedClient;
use super::health::Health;
use super::text;

/// Fallback wake-up when no indexer pulse arrives. The pulse is the real
/// trigger; this only covers vectors that went missing without an index pass
/// (a fingerprint reset, an endpoint that came back up).
pub const IDLE_TICK: Duration = Duration::from_secs(60);

/// First re-probe delay after the endpoint is found unreachable.
pub const PROBE_BACKOFF_START: Duration = Duration::from_secs(1);

/// Ceiling on the re-probe interval; a server started an hour later is still
/// picked up within a minute.
pub const PROBE_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Upper bound on remembered poison chunk ids, so a pathological corpus
/// cannot grow this set without limit.
pub const MAX_POISONED: usize = 10_000;

/// Ceiling on one `chunks_missing_embeddings` request, which is widened by
/// the poison count so skipped chunks cannot hide the work behind them.
const MAX_FETCH: usize = 5_000;

/// Normalization tag recorded in the fingerprint. The store L2-normalizes
/// every vector on write, so cosine is a dot product thereafter.
pub const NORMALIZATION: &str = "l2";

/// What one drain pass ended on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drained {
    /// Nothing left to embed.
    Idle,
    /// Stopped early: cancelled, or the endpoint stopped cooperating.
    Interrupted,
}

pub struct EmbedWorker {
    store: StoreHandle,
    client: Arc<EmbedClient>,
    health: Health,
    notify: Arc<Notify>,
    cancel: CancellationToken,
    poisoned: HashSet<(ProjectId, ChunkId)>,
    /// Chunks abandoned this process lifetime; logged, not persisted.
    skipped: usize,
}

impl EmbedWorker {
    pub fn new(
        store: StoreHandle,
        client: Arc<EmbedClient>,
        health: Health,
        notify: Arc<Notify>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            store,
            client,
            health,
            notify,
            cancel,
            poisoned: HashSet::new(),
            skipped: 0,
        }
    }

    /// Chunks abandoned after a non-retryable rejection.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    pub async fn run(mut self) {
        if let Err(err) = self.reconcile_fingerprint().await {
            tracing::error!(error = %err, "could not reconcile the embedding fingerprint; embed worker will not run");
            return;
        }

        let mut backoff = PROBE_BACKOFF_START;
        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            if !self.health.is_ready() {
                if self.probe().await {
                    backoff = PROBE_BACKOFF_START;
                } else {
                    if !self.sleep(backoff).await {
                        break;
                    }
                    backoff = (backoff * 2).min(PROBE_BACKOFF_MAX);
                    continue;
                }
            }

            // A pass that ended on an endpoint problem goes straight back to
            // the probe/backoff arm instead of idling for a full tick.
            if self.drain().await == Drained::Interrupted && !self.health.is_ready() {
                continue;
            }

            tokio::select! {
                () = self.cancel.cancelled() => break,
                () = self.notify.notified() => {}
                _ = tokio::time::sleep(IDLE_TICK) => {}
            }
        }
        tracing::debug!(skipped = self.skipped, "embed worker stopped");
    }

    /// Probe the endpoint and publish the result. Returns readiness.
    pub async fn probe(&self) -> bool {
        match self.client.probe().await {
            Ok(dims) => {
                tracing::debug!(
                    dims,
                    endpoint = self.client.endpoint(),
                    "embedding probe ok"
                );
                self.health
                    .set_ready(self.client.endpoint(), self.client.model());
                true
            }
            Err(err) => {
                self.health
                    .set_unreachable(self.client.endpoint(), err.to_string());
                false
            }
        }
    }

    /// Compare the configured embedding space with the stored one; on a
    /// mismatch discard every vector and record the new identity.
    ///
    /// This is 3.1's `model_id_tag` rule and it is deliberately destructive:
    /// vectors from two different models in one index produce ranking that is
    /// wrong in a way nothing downstream can detect.
    pub async fn reconcile_fingerprint(&self) -> anyhow::Result<()> {
        let want = fingerprint(self.client.settings());
        let target = want.clone();
        let cleared = self
            .store
            .with(move |store| -> crate::store::Result<Option<usize>> {
                if store.embedding_fingerprint()?.as_ref() == Some(&target) {
                    return Ok(None);
                }
                let cleared = store.clear_all_embeddings()?;
                store.set_embedding_fingerprint(&target)?;
                Ok(Some(cleared))
            })
            .await??;

        if let Some(cleared) = cleared {
            tracing::warn!(
                model = %want.model_id,
                dimensions = want.dimensions,
                discarded_vectors = cleared,
                "embedding fingerprint changed; every stored vector was discarded and will be re-embedded"
            );
        }
        Ok(())
    }

    /// Embed until nothing is missing, cancellation arrives, or the endpoint
    /// stops cooperating.
    pub async fn drain(&mut self) -> Drained {
        loop {
            if self.cancel.is_cancelled() {
                return Drained::Interrupted;
            }

            let batch = match self.next_batch().await {
                Some(batch) if !batch.is_empty() => batch,
                Some(_) => return Drained::Idle,
                None => return Drained::Interrupted,
            };

            let prefix = self.client.settings().document_prefix.clone();
            let texts: Vec<String> = batch
                .iter()
                .map(|candidate| text::document_text(&candidate.chunk, &prefix))
                .collect();

            let embedded = tokio::select! {
                () = self.cancel.cancelled() => return Drained::Interrupted,
                result = self.client.embed(&texts) => result,
            };

            match embedded {
                Ok(vectors) => {
                    if !self.store_vectors(&batch, vectors).await {
                        return Drained::Interrupted;
                    }
                }
                Err(err) if err.is_permanent() => {
                    tracing::error!(
                        error = %err,
                        chunks = batch.len(),
                        first_path = %batch[0].chunk.path,
                        "endpoint rejected a batch outright; those chunks stay unembedded"
                    );
                    self.poison(&batch);
                    self.health
                        .set_unreachable(self.client.endpoint(), err.to_string());
                    return Drained::Interrupted;
                }
                Err(err) => {
                    tracing::warn!(error = %err, chunks = batch.len(), "embedding batch failed; will retry later");
                    self.health
                        .set_unreachable(self.client.endpoint(), err.to_string());
                    return Drained::Interrupted;
                }
            }

            // Be a good citizen on the runtime between batches.
            tokio::task::yield_now().await;
        }
    }

    /// `None` on a store failure; an empty vec when the backlog is drained.
    async fn next_batch(&self) -> Option<Vec<EmbedCandidate>> {
        let want = self.client.settings().batch_max_items.max(1);
        // Widened by the poison count: skipped chunks sort first (lowest
        // rowid) and would otherwise fill every request forever.
        let fetch = want.saturating_add(self.poisoned.len()).min(MAX_FETCH);
        let fetched = match self
            .store
            .with(move |s| s.chunks_missing_embeddings(fetch))
            .await
        {
            Ok(Ok(fetched)) => fetched,
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "could not list chunks missing embeddings");
                return None;
            }
            Err(err) => {
                tracing::warn!(error = %err, "store task failed while listing embed candidates");
                return None;
            }
        };
        Some(
            fetched
                .into_iter()
                .filter(|candidate| {
                    !self
                        .poisoned
                        .contains(&(candidate.project, candidate.chunk.id.clone()))
                })
                .take(want)
                .collect(),
        )
    }

    /// Returns false on a store failure (the caller stops this pass).
    async fn store_vectors(&mut self, batch: &[EmbedCandidate], vectors: Vec<Vec<f32>>) -> bool {
        let mut items = Vec::with_capacity(batch.len());
        let mut degenerate = Vec::new();
        for (candidate, vector) in batch.iter().zip(vectors) {
            // The store rejects an unusable vector for the whole transaction;
            // catching it here keeps one bad vector from blocking the batch
            // (and then the batch from blocking the backlog, forever). The
            // predicate is the store's own, so the two cannot drift apart.
            if crate::store::vector::is_usable(&vector) {
                items.push(NewEmbedding {
                    project: candidate.project,
                    chunk_id: candidate.chunk.id.clone(),
                    vector,
                });
            } else {
                degenerate.push(candidate.clone());
            }
        }
        if !degenerate.is_empty() {
            tracing::warn!(
                chunks = degenerate.len(),
                "model returned unusable vectors (empty, zero-length or non-finite); skipping those chunks"
            );
            self.poison(&degenerate);
        }

        match self.store.with(move |s| s.upsert_embeddings(&items)).await {
            Ok(Ok(stored)) => {
                tracing::debug!(stored, "stored embeddings");
                true
            }
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "storing embeddings failed");
                false
            }
            Err(err) => {
                tracing::warn!(error = %err, "store task failed while writing embeddings");
                false
            }
        }
    }

    fn poison(&mut self, batch: &[EmbedCandidate]) {
        for candidate in batch {
            if self.poisoned.len() >= MAX_POISONED {
                tracing::error!(
                    limit = MAX_POISONED,
                    "too many chunks rejected by the embedding endpoint; stopping the skip list"
                );
                break;
            }
            if self
                .poisoned
                .insert((candidate.project, candidate.chunk.id.clone()))
            {
                self.skipped += 1;
            }
        }
    }

    /// Cancellable sleep. False means "cancelled; stop".
    async fn sleep(&self, duration: Duration) -> bool {
        tokio::select! {
            () = self.cancel.cancelled() => false,
            _ = tokio::time::sleep(duration) => true,
        }
    }
}

/// The identity of the embedding space implied by configuration.
pub fn fingerprint(settings: &super::client::EmbedSettings) -> EmbeddingFingerprint {
    EmbeddingFingerprint {
        model_id: settings.model.clone(),
        // 0 means "config did not declare a width"; the model id is then the
        // only thing distinguishing two spaces, which is why changing models
        // without declaring dimensions is still caught.
        dimensions: settings.dimensions.unwrap_or(0),
        query_prefix: settings.query_prefix.clone(),
        document_prefix: settings.document_prefix.clone(),
        normalization: NORMALIZATION.to_string(),
    }
}
