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
//!                                   └──── wait: indexer pulse | probe request
//!                                               | 60s tick | cancel
//! ```
//!
//! The probe request is [`Health::request_probe`], raised by a query embedding
//! that timed out (#5). Such a query publishes no verdict of its own — a model
//! reloading after an idle timeout is indistinguishable from a dead server at
//! that distance — so the worker goes and asks, now rather than at the next
//! idle tick. It is the one wake-up that probes even while health says
//! `Ready`, because doubting a `Ready` is exactly what it is for.
//!
//! # Why it cannot starve search
//!
//! Every store touch goes through [`StoreHandle::with`], which runs on the
//! blocking pool and takes the store lock for exactly one call — one batch
//! fetch, one batch upsert. A full backlog is therefore thousands of short
//! lock acquisitions interleaved with `/v1/search`, not one long one. Network
//! time, which dominates, holds no lock at all. Concurrent batches do not
//! change that: they are more of the same short independent calls, which the
//! store's mutex serializes as it does every other caller.
//!
//! # Concurrency
//!
//! `concurrency` batches are in flight at once (`1` = the serial pipeline).
//! A local server serves several requests at a time, and one batch at a time
//! leaves it idle for every round-trip and every upsert. A batch whose text
//! overflows the wire budget fans out further: [`EmbedClient::embed`] sends
//! its wire batches concurrently, so the server-side queue depth is
//! `concurrency` × the wire batches per worker batch.
//!
//! Claiming stays single-threaded. Chunks only leave
//! `chunks_missing_embeddings` when their batch is upserted, so a second
//! claimer would hand the same chunk to two batches; instead the drain loop is
//! the only caller of [`EmbedWorker::next_batch`] and records the claim before
//! it awaits anything. Claims are filtered exactly like poison, and live only
//! for the pass — a pass never returns while a batch it issued is still out,
//! so an abandoned claim cannot outlive it.
//!
//! # Poison chunks
//!
//! A batch the endpoint answers with a non-retryable 4xx is held back, because
//! `chunks_missing_embeddings` would otherwise hand it straight back and the
//! entire backlog behind it would never drain. The same failure also marks
//! health unreachable: a 4xx is far more often a configuration error (wrong
//! model id) affecting *every* batch than one bad input, and the next probe
//! distinguishes the two — if the endpoint answers a trivial probe, the
//! problem really was that input and draining resumes past it; if it does not,
//! the daemon is visibly degraded in `/v1/status` instead of quietly poisoning
//! the corpus 64 chunks at a time.
//!
//! Held back, though, is not the same as condemned. A 4xx is the endpoint's
//! *opinion*, and it can be wrong: Ollama on Windows answers a valid request
//! with a 400 whose body is a dial error when the machine runs out of
//! ephemeral ports, which cost 64 healthy chunks their vectors for a whole
//! process lifetime (#6). So poisoning is graded, and nothing here is
//! permanent:
//!
//! ```text
//!   1st rejection ─▶ suspected: retried once after POISON_RETRY_DELAY
//!   2nd rejection ─▶ abandoned: counted in `skipped`, retried after POISON_TTL
//! ```
//!
//! One delayed retry costs one request and settles the question; the expiry
//! costs two requests per abandoned batch per [`POISON_TTL`] and means a
//! misclassification heals on its own instead of surviving until the daemon
//! restarts. A genuinely permanent rejection therefore stays negligible — it
//! is a couple of requests every ten-odd minutes — while still being retried
//! often enough that "poisoned" never means "written off".

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::daemon::store_handle::StoreHandle;
use crate::store::{EmbedCandidate, EmbeddingFingerprint, NewEmbedding, ProjectId};
use crate::types::ChunkId;

use super::client::EmbedClient;
use super::health::{Health, Ticket};
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

/// How long a once-rejected chunk waits for its retry. Long enough for the
/// transport failures a 4xx can wrap — port exhaustion, a proxy blip, a
/// half-loaded model — to clear, and short enough that the retry still lands
/// inside the same drain of a large backlog.
pub const POISON_RETRY_DELAY: Duration = Duration::from_secs(60);

/// How long an abandoned chunk stays abandoned before it is offered again.
/// Bounds how long a misclassification can cost recall: minutes, not the
/// lifetime of the process.
pub const POISON_TTL: Duration = Duration::from_secs(12 * 60);

/// Rejections a chunk survives before it is abandoned. The first is a
/// suspicion; the second is a verdict.
const ABANDON_STRIKES: u32 = 2;

/// Rows hydrated per `chunks_missing_embeddings` page. Poisoned chunks are
/// filtered after the query, so pages — not one widened fetch — are what
/// carries the scan past them; this only bounds how many full chunk rows are
/// in memory at once. `MAX_POISONED / PAGE_ROWS` bounds the extra pages a
/// fully poisoned prefix can cost.
pub const PAGE_ROWS: usize = 512;

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
    poisoned: HashMap<(ProjectId, ChunkId), Poison>,
    /// Chunks abandoned this process lifetime; logged, not persisted.
    skipped: usize,
    page_rows: usize,
    retry_delay: Duration,
    poison_ttl: Duration,
}

/// What the worker remembers about a chunk the endpoint would not embed.
#[derive(Debug)]
struct Poison {
    /// Rejections seen for this chunk. At [`ABANDON_STRIKES`] it is abandoned
    /// rather than merely suspected.
    strikes: u32,
    /// When it may be offered to the endpoint again.
    retry_at: Instant,
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
            poisoned: HashMap::new(),
            skipped: 0,
            page_rows: PAGE_ROWS,
            retry_delay: POISON_RETRY_DELAY,
            poison_ttl: POISON_TTL,
        }
    }

    /// Shrink the fetch page. Exists so a test can exercise the paging walk
    /// past a fully poisoned page without seeding [`PAGE_ROWS`] rows; nothing
    /// in the daemon calls it.
    pub fn set_page_rows(&mut self, rows: usize) {
        self.page_rows = rows.max(1);
    }

    /// Shorten the two poison windows. Exists so a test can watch the
    /// retry-then-abandon cadence and the expiry without waiting minutes;
    /// nothing in the daemon calls it.
    pub fn set_poison_windows(&mut self, retry_delay: Duration, ttl: Duration) {
        self.retry_delay = retry_delay;
        self.poison_ttl = ttl;
    }

    /// Chunks abandoned this process lifetime: rejected, retried once, and
    /// rejected again. They are not gone for good — each is offered again once
    /// its [`POISON_TTL`] is up — so this is the count of chunks the worker
    /// has given up on at least once, not of chunks it will never send.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    pub async fn run(mut self) {
        if let Err(err) = self.reconcile_fingerprint().await {
            tracing::error!(error = %err, "could not reconcile the embedding fingerprint; embed worker will not run");
            return;
        }

        let mut backoff = PROBE_BACKOFF_START;
        // Someone doubts the stored verdict — a query that timed out — so the
        // next turn of the loop probes even if health currently reads `Ready`.
        let mut recheck = false;
        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            if recheck || !self.health.is_ready() {
                recheck = false;
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

            // Any pass that leaves health non-ready goes straight back to the
            // probe/backoff arm instead of idling for a full tick — including
            // an `Idle` one, because health can be demoted by a *query* while
            // the worker has nothing to do, and D-0007 degradation that
            // outlives the outage by a tick is a lie in `/v1/status`.
            self.drain().await;
            if !self.health.is_ready() {
                continue;
            }

            tokio::select! {
                () = self.cancel.cancelled() => break,
                () = self.notify.notified() => {}
                () = self.health.probe_requested() => recheck = true,
                _ = tokio::time::sleep(IDLE_TICK) => {}
            }
        }
        tracing::debug!(skipped = self.skipped, "embed worker stopped");
    }

    /// Probe the endpoint and publish the result. Returns readiness.
    pub async fn probe(&self) -> bool {
        let ticket = self.health.ticket();
        match self.client.probe().await {
            Ok(dims) => {
                tracing::debug!(
                    dims,
                    endpoint = self.client.endpoint(),
                    "embedding probe ok"
                );
                ticket.set_ready(self.client.endpoint(), self.client.model());
                true
            }
            Err(err) => {
                ticket.set_unreachable(self.client.endpoint(), err.to_string());
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
    ///
    /// Issues up to `concurrency` batches before waiting on any of them. Once
    /// something goes wrong no further batch is issued, but the ones already
    /// out are awaited rather than abandoned — their vectors are as good as
    /// any other batch's, and a claim must not outlive the pass that made it.
    pub async fn drain(&mut self) -> Drained {
        let limit = self.client.settings().concurrency.max(1);
        let mut in_flight: JoinSet<Batch> = JoinSet::new();
        let mut claimed = Claimed::new();
        // Why this pass stopped issuing, once it has.
        let mut stop: Option<Drained> = None;

        loop {
            while stop.is_none() && in_flight.len() < limit {
                if self.cancel.is_cancelled() {
                    stop = Some(Drained::Interrupted);
                    break;
                }
                match self.next_batch(&claimed).await {
                    Some(batch) if !batch.is_empty() => {
                        self.issue(&mut in_flight, &mut claimed, batch)
                    }
                    // Nothing left *that is not already claimed*: the batches
                    // still out may well be the last of the backlog.
                    Some(_) => break,
                    None => stop = Some(Drained::Interrupted),
                }
            }

            // Reached only with the walk exhausted or `stop` set, so `Idle`
            // here really does mean "backlog empty and nothing in flight".
            let Some(joined) = in_flight.join_next().await else {
                return stop.unwrap_or(Drained::Idle);
            };
            let outcome = match joined {
                Ok(batch) => self.settle(batch, &mut claimed),
                Err(err) => {
                    tracing::error!(error = %err, "an embed batch task did not finish");
                    Some(Drained::Interrupted)
                }
            };
            if let Some(outcome) = outcome {
                stop.get_or_insert(outcome);
            }
        }
    }

    /// Claim `batch` and put it on the wire.
    fn issue(
        &self,
        in_flight: &mut JoinSet<Batch>,
        claimed: &mut Claimed,
        batch: Vec<EmbedCandidate>,
    ) {
        claimed.extend(batch.iter().map(key));
        // Claimed before the request, so a batch that took seconds to fail
        // cannot demote health a later probe has already restored. One ticket
        // per batch: concurrent batches are independent observations.
        let ticket = self.health.ticket();
        in_flight.spawn(embed_batch(
            self.store.clone(),
            self.client.clone(),
            ticket,
            self.cancel.clone(),
            batch,
        ));
    }

    /// Release a finished batch's claims and record what it did. `Some` when
    /// this pass must stop issuing.
    fn settle(&mut self, batch: Batch, claimed: &mut Claimed) -> Option<Drained> {
        for candidate in &batch.candidates {
            claimed.remove(&key(candidate));
        }
        match batch.outcome {
            Outcome::Stored { degenerate } => {
                // No retry: the endpoint answered, and the answer was a vector
                // the store cannot use. Sending the same text buys the same
                // vector, so this goes straight to abandoned — the TTL is the
                // only second chance a model that has been fixed needs.
                self.poison(&degenerate, ABANDON_STRIKES);
                None
            }
            Outcome::Rejected => {
                self.poison(&batch.candidates, 1);
                Some(Drained::Interrupted)
            }
            Outcome::Failed | Outcome::StoreFailed | Outcome::Cancelled => {
                Some(Drained::Interrupted)
            }
        }
    }

    /// `None` on a store failure; an empty vec **only** when the query walked
    /// off the end of the missing set.
    ///
    /// Poisoned and in-flight chunks are filtered after the query, so a page
    /// that filters away entirely is not an empty backlog — it is a page to
    /// step over. The rowid cursor makes that distinction; without it the
    /// oldest poisoned rows fill every request forever and everything behind
    /// them starves. The walk terminates because each page either yields work
    /// or consumes rows from two bounded sets: poison, capped at
    /// [`MAX_POISONED`], and the claims of at most `concurrency` batches.
    /// Poison is judged against one instant taken here, so the filtered set
    /// cannot grow mid-walk.
    async fn next_batch(&self, claimed: &Claimed) -> Option<Vec<EmbedCandidate>> {
        let now = Instant::now();
        let want = self.client.settings().batch_max_items.max(1);
        // Independent of `want`: the walk fills a batch larger than one page
        // from several pages, so hydrated rows stay bounded whatever the
        // configured batch size is.
        let page = self.page_rows;
        let mut batch = Vec::with_capacity(want.min(page));
        let mut cursor = 0i64;

        loop {
            let fetched = match self
                .store
                .with(move |s| s.chunks_missing_embeddings(cursor, page))
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
            let end_of_set = fetched.len() < page;

            for candidate in fetched {
                cursor = cursor.max(candidate.rowid);
                let key = key(&candidate);
                if !self.held_back(&key, now) && !claimed.contains(&key) {
                    batch.push(candidate);
                    if batch.len() == want {
                        return Some(batch);
                    }
                }
            }

            if end_of_set {
                return Some(batch);
            }
        }
    }

    /// Is this chunk serving a poison window right now?
    fn held_back(&self, key: &(ProjectId, ChunkId), now: Instant) -> bool {
        self.poisoned
            .get(key)
            .is_some_and(|poison| now < poison.retry_at)
    }

    /// Record `strikes` rejections against every chunk in `batch`, and set
    /// when each may be offered again: [`POISON_RETRY_DELAY`] while it is
    /// merely suspected, [`POISON_TTL`] once it is abandoned.
    ///
    /// `skipped` counts each chunk the first time it crosses into abandoned,
    /// so a chunk re-rejected after its TTL expires is not counted twice.
    fn poison(&mut self, batch: &[EmbedCandidate], strikes: u32) {
        let now = Instant::now();
        let (retry_delay, ttl) = (self.retry_delay, self.poison_ttl);
        if self.poisoned.len() >= MAX_POISONED {
            // Suspicions that have already had their retry are spent: the
            // chunk was either embedded or is about to earn a fresh strike.
            // Dropping them keeps a transport blip that touched thousands of
            // healthy chunks from filling the map and leaving no room to
            // remember a genuinely bad one.
            self.poisoned
                .retain(|_, poison| poison.strikes >= ABANDON_STRIKES || now < poison.retry_at);
        }

        for candidate in batch {
            let key = key(candidate);
            if !self.poisoned.contains_key(&key) && self.poisoned.len() >= MAX_POISONED {
                tracing::error!(
                    limit = MAX_POISONED,
                    "too many chunks rejected by the embedding endpoint; stopping the skip list"
                );
                break;
            }
            let entry = self.poisoned.entry(key).or_insert(Poison {
                strikes: 0,
                retry_at: now,
            });
            let before = entry.strikes;
            entry.strikes += strikes;
            entry.retry_at = now
                + match entry.strikes >= ABANDON_STRIKES {
                    true => ttl,
                    false => retry_delay,
                };
            if before < ABANDON_STRIKES && entry.strikes >= ABANDON_STRIKES {
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

/// Chunks a batch in flight has taken responsibility for. Same shape as the
/// poison set, and filtered at the same point, so `next_batch` cannot hand a
/// chunk to a second batch while the first is still out.
type Claimed = HashSet<(ProjectId, ChunkId)>;

fn key(candidate: &EmbedCandidate) -> (ProjectId, ChunkId) {
    (candidate.project, candidate.chunk.id.clone())
}

/// A finished batch, handed back to the drain loop.
struct Batch {
    candidates: Vec<EmbedCandidate>,
    outcome: Outcome,
}

enum Outcome {
    /// Written, apart from vectors the store would have refused.
    Stored { degenerate: Vec<EmbedCandidate> },
    /// The store itself refused the write.
    StoreFailed,
    /// A 4xx: the endpoint says this exact request will never succeed. Taken
    /// as a suspicion the first time and as a verdict the second — see the
    /// poison grading in the module documentation.
    Rejected,
    /// Unreachable or still failing after every retry; retry the chunks later.
    Failed,
    /// Cancelled before the endpoint answered — nothing observed, so nothing
    /// published to health either.
    Cancelled,
}

/// One batch, start to finish, as its own task so several overlap on the
/// endpoint.
///
/// Health is published from here rather than from the drain loop because the
/// [`Ticket`] is the point: it was claimed before this request started, and a
/// verdict from a batch that took seconds to fail must not land on top of a
/// probe that has since said the endpoint is back.
async fn embed_batch(
    store: StoreHandle,
    client: Arc<EmbedClient>,
    ticket: Ticket,
    cancel: CancellationToken,
    candidates: Vec<EmbedCandidate>,
) -> Batch {
    let prefix = &client.settings().document_prefix;
    let texts: Vec<String> = candidates
        .iter()
        .map(|candidate| text::document_text(&candidate.chunk, prefix))
        .collect();

    let embedded = tokio::select! {
        () = cancel.cancelled() => {
            return Batch { candidates, outcome: Outcome::Cancelled };
        }
        result = client.embed(&texts) => result,
    };

    let outcome = match embedded {
        Ok(vectors) => store_vectors(&store, &candidates, vectors).await,
        Err(err) if err.is_permanent() => {
            tracing::error!(
                error = %err,
                chunks = candidates.len(),
                first_path = %candidates[0].chunk.path,
                "endpoint rejected a batch outright; those chunks are held back and retried once"
            );
            ticket.set_unreachable(client.endpoint(), err.to_string());
            Outcome::Rejected
        }
        Err(err) => {
            tracing::warn!(error = %err, chunks = candidates.len(), "embedding batch failed; will retry later");
            ticket.set_unreachable(client.endpoint(), err.to_string());
            Outcome::Failed
        }
    };
    Batch {
        candidates,
        outcome,
    }
}

/// One short [`StoreHandle::with`] call, whatever else is in flight.
async fn store_vectors(
    store: &StoreHandle,
    batch: &[EmbedCandidate],
    vectors: Vec<Vec<f32>>,
) -> Outcome {
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
    }

    match store.with(move |s| s.upsert_embeddings(&items)).await {
        Ok(Ok(stored)) => {
            tracing::debug!(stored, "stored embeddings");
            Outcome::Stored { degenerate }
        }
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "storing embeddings failed");
            Outcome::StoreFailed
        }
        Err(err) => {
            tracing::warn!(error = %err, "store task failed while writing embeddings");
            Outcome::StoreFailed
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
        max_embed_bytes: settings.max_embed_bytes as u32,
    }
}
