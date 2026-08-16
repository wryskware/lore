//! OpenAI-compatible embeddings client, aimed at a *local* server
//! (llama-server, LM Studio, Ollama's compat shim) — D-0003 makes cloud
//! providers non-options, which is also why plain HTTP is enough and reqwest
//! is pinned without TLS.
//!
//! Wire shape:
//!
//! ```text
//! POST {endpoint}/embeddings
//! {"model": "...", "input": ["text", ...]}
//! -> {"data": [{"index": 0, "embedding": [f32, ...]}, ...]}
//! ```
//!
//! Three behaviours here are load-bearing rather than incidental:
//!
//! - **Order is restored from `index`, never assumed.** The response is
//!   allowed to arrive in any order, and a vector attached to the wrong chunk
//!   is a silent, permanent corruption of the index — the worst possible
//!   failure class for a retrieval system.
//! - **Batches are bounded twice**, by item count *and* by total bytes. Item
//!   count alone lets 64 four-kilobyte chunks become a quarter-megabyte
//!   request that a small local server answers with a 500 or a timeout.
//!   A call that splits into several wire batches sends them **concurrently**
//!   — they are independent requests, and sending them one after another
//!   leaves the server idle for every round trip between them.
//! - **Failures are classified, not merely counted.** Transient (network,
//!   408, 429, 5xx) means retry with backoff; anything else 4xx means *this
//!   request will never succeed*, so retrying it forever would wedge the
//!   pipeline behind one poison input. The one exception is a 4xx whose
//!   *body* is a transport error the endpoint failed to translate — see
//!   [`TRANSPORT_SIGNATURES`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::EmbeddingsConfig;

use super::text::truncate_bytes;

/// Default total request-text budget for one wire batch
/// ([`EmbedSettings::batch_max_bytes`]). Independent of item count: the point
/// is to keep a single POST small enough that a modest local server answers
/// it comfortably. A GPU-backed server working through a big backlog wants a
/// budget several times larger (fewer, fuller round trips), which is why this
/// is a config default and not a ceiling.
pub const BATCH_MAX_BYTES: usize = 64 * 1024;

/// Floor for the configured wire-batch budget. Purely a sanity clamp — an
/// item bigger than the budget still gets its own batch, so any value makes
/// progress; below this the split is all overhead.
pub const MIN_BATCH_MAX_BYTES: usize = 1024;

/// Attempts per batch, including the first. Four attempts with the default
/// backoff spans a few seconds — long enough to ride out a model reload,
/// short enough that shutdown does not wait on it.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 4;

/// First retry delay; doubled per attempt and jittered.
pub const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);

/// Ceiling for any single wait, including one asked for by `Retry-After`.
/// The daemon's shutdown grace is 10s; a retry that outlasts it is a hang.
pub const RETRY_MAX_DELAY: Duration = Duration::from_secs(5);

/// Per-request ceiling. Generous: a cold local model can take tens of seconds
/// to load before it answers the very first embedding call.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Ceiling on *establishing* the connection, separate from the generous
/// whole-request budget. A server that is slow to embed is normal; a port
/// nobody is listening on is not, and on Windows an unanswered SYN can hang
/// for seconds before the stack gives up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Sent as the probe input. Deliberately trivial and content-free. Public so
/// a test can tell a probe apart from real work in a server's request log.
pub const PROBE_INPUT: &str = "lore health probe";

/// Substrings that betray a *transport* failure inside an error body,
/// whatever status the endpoint wrapped it in. Ollama on Windows answers a
/// perfectly valid request with HTTP 400 carrying a Go dial error when the
/// machine has run out of ephemeral ports; taken at face value that poisons a
/// batch of entirely healthy chunks for the life of the process.
///
/// Matching one of these only buys the request the ordinary retry budget — if
/// every attempt still fails, the verdict reverts to what the status said,
/// because a genuinely permanent 4xx that merely *mentions* "timeout" must not
/// become a chunk the worker retries forever.
const TRANSPORT_SIGNATURES: [&str; 4] = ["dial", "connectex", "bind", "timeout"];

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// The endpoint answered, and its answer says this request is never going
    /// to work: an unknown model, a malformed body, an input it refuses, a
    /// response shape we cannot read.
    #[error("embedding endpoint rejected the request: {0}")]
    Permanent(String),
    /// Unreachable, timed out, or still failing after every retry.
    #[error("embedding endpoint unreachable: {0}")]
    Transient(String),
}

impl EmbedError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, EmbedError::Permanent(_))
    }
}

/// Retry shape, extracted so tests can run the real retry logic in
/// milliseconds instead of seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: RETRY_BASE_DELAY,
            max_delay: RETRY_MAX_DELAY,
        }
    }
}

/// Everything the client needs, resolved from [`EmbeddingsConfig`] once.
///
/// `Debug` is hand-written rather than derived so `api_key` cannot reach a log
/// line by accident — a derived one would print it the first time anyone adds
/// `?settings` to a tracing call.
#[derive(Clone, PartialEq, Eq)]
pub struct EmbedSettings {
    /// Base URL with no trailing slash, e.g. `http://127.0.0.1:8080/v1`.
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Expected vector width. `None` = accept whatever the server returns.
    pub dimensions: Option<u32>,
    pub query_prefix: String,
    pub document_prefix: String,
    pub batch_max_items: usize,
    /// Byte budget for one wire batch; an `embed` call over more text than
    /// this fans out into concurrent POSTs.
    pub batch_max_bytes: usize,
    /// Batches the worker may have in flight at once. Read by
    /// [`crate::embed::worker`], not by the client. Total concurrent POSTs is
    /// this times the wire batches one worker batch splits into.
    pub concurrency: usize,
    /// Per-input byte budget; every input is clipped to this before the wire.
    pub max_embed_bytes: usize,
    pub retry: RetryPolicy,
    pub request_timeout: Duration,
}

impl std::fmt::Debug for EmbedSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedSettings")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("dimensions", &self.dimensions)
            .field("query_prefix", &self.query_prefix)
            .field("document_prefix", &self.document_prefix)
            .field("batch_max_items", &self.batch_max_items)
            .field("batch_max_bytes", &self.batch_max_bytes)
            .field("concurrency", &self.concurrency)
            .field("max_embed_bytes", &self.max_embed_bytes)
            .field("retry", &self.retry)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl EmbedSettings {
    /// `None` when no endpoint is configured — the D-0007 lexical-only case.
    pub fn from_config(config: &EmbeddingsConfig) -> Option<Self> {
        let endpoint = config.endpoint.as_deref()?.trim().trim_end_matches('/');
        if endpoint.is_empty() {
            return None;
        }
        Some(Self {
            endpoint: endpoint.to_string(),
            model: config.model_id().to_string(),
            api_key: config.api_key.clone(),
            dimensions: config.dimensions,
            query_prefix: config.query_prefix.clone(),
            document_prefix: config.document_prefix.clone(),
            batch_max_items: config.batch_max_items.max(1),
            batch_max_bytes: config.batch_max_bytes.max(MIN_BATCH_MAX_BYTES),
            concurrency: config.concurrency.max(1),
            // Floor: prefix + header must survive the clip, or every input
            // for a chunk collapses to the same truncated stub.
            max_embed_bytes: config.max_embed_bytes.max(256),
            retry: RetryPolicy::default(),
            request_timeout: REQUEST_TIMEOUT,
        })
    }
}

pub struct EmbedClient {
    http: reqwest::Client,
    url: String,
    settings: EmbedSettings,
}

impl std::fmt::Debug for EmbedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedClient")
            .field("url", &self.url)
            .field("model", &self.settings.model)
            .finish_non_exhaustive()
    }
}

impl EmbedClient {
    pub fn new(settings: EmbedSettings) -> Result<Self, EmbedError> {
        let http = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .connect_timeout(CONNECT_TIMEOUT.min(settings.request_timeout))
            .build()
            .map_err(|err| EmbedError::Permanent(format!("cannot build HTTP client: {err}")))?;
        Ok(Self {
            url: format!("{}/embeddings", settings.endpoint),
            http,
            settings,
        })
    }

    pub fn settings(&self) -> &EmbedSettings {
        &self.settings
    }

    pub fn endpoint(&self) -> &str {
        &self.settings.endpoint
    }

    pub fn model(&self) -> &str {
        &self.settings.model
    }

    /// Embed `texts`, returning one vector per input **in input order**.
    ///
    /// Splits into wire batches internally and sends them **concurrently** —
    /// `try_join_all` keeps results in future order regardless of arrival
    /// order, so the concatenation below is input order by construction. A
    /// batch failure fails the whole call (the caller's unit of work is the
    /// batch it handed in); the sibling requests still in flight are dropped,
    /// which aborts them. Fan-out is bounded by configuration: at most
    /// ⌈`batch_max_items` · `max_embed_bytes` / `batch_max_bytes`⌉ requests.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let clipped: Vec<&str> = texts
            .iter()
            .map(|text| truncate_bytes(text, self.settings.max_embed_bytes))
            .collect();

        let wire = batches(
            &clipped,
            self.settings.batch_max_items,
            self.settings.batch_max_bytes,
        );
        let results = futures_util::future::try_join_all(
            wire.into_iter().map(|batch| self.embed_batch(batch)),
        )
        .await?;
        Ok(results.into_iter().flatten().collect())
    }

    /// Cheapest honest health check: embed one tiny input.
    ///
    /// `/models` was rejected as the probe — it is optional on several local
    /// servers, and a server that lists models can still fail every embedding
    /// call (wrong model loaded, no pooling configured). Answering "can this
    /// endpoint embed?" with anything other than an embedding call is a
    /// guess. Returns the observed vector width.
    pub async fn probe(&self) -> Result<usize, EmbedError> {
        let vectors = self
            .embed_batch(&[truncate_bytes(PROBE_INPUT, self.settings.max_embed_bytes)])
            .await?;
        vectors
            .first()
            .map(Vec::len)
            .ok_or_else(|| EmbedError::Permanent("probe returned no vector".to_string()))
    }

    /// One POST, retried per [`RetryPolicy`].
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut attempt = 1u32;
        loop {
            match self.post(texts).await {
                Ok(vectors) => return Ok(vectors),
                Err(Attempt::Permanent(message)) => return Err(EmbedError::Permanent(message)),
                Err(Attempt::Retryable {
                    message,
                    retry_after,
                    permanent_if_exhausted,
                }) => {
                    if attempt >= self.settings.retry.max_attempts {
                        let message = format!("{message} (after {attempt} attempts)");
                        return Err(match permanent_if_exhausted {
                            true => EmbedError::Permanent(message),
                            false => EmbedError::Transient(message),
                        });
                    }
                    let delay = retry_after
                        .unwrap_or_else(|| backoff(attempt, self.settings.retry.base_delay))
                        .min(self.settings.retry.max_delay);
                    tracing::debug!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %message,
                        "embedding request failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }

    async fn post(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Attempt> {
        let body = EmbeddingsRequest {
            model: &self.settings.model,
            input: texts,
        };
        let mut request = self.http.post(&self.url).json(&body);
        if let Some(key) = &self.settings.api_key {
            request = request.bearer_auth(key);
        }

        let response = request
            .send()
            .await
            .map_err(|err| Attempt::retryable(err.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers());
            let body = response.text().await.unwrap_or_default();
            let wrapped_transport = looks_like_transport(&body);
            let message = format!("HTTP {} {}", status.as_u16(), snippet(&body));
            // 408/429/5xx are the server saying "not now"; every other 4xx is
            // the server saying "not ever" for this exact request — unless its
            // body says otherwise, in which case it gets the retry budget and
            // the status has the last word if that runs out.
            return Err(
                if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
                    Attempt::Retryable {
                        message,
                        retry_after,
                        permanent_if_exhausted: false,
                    }
                } else if wrapped_transport {
                    Attempt::Retryable {
                        message,
                        retry_after,
                        permanent_if_exhausted: true,
                    }
                } else {
                    Attempt::Permanent(message)
                },
            );
        }

        // Read bytes first, then parse: a truncated body is a network problem
        // (retry), a well-formed body of the wrong shape is a contract
        // problem (do not retry).
        let bytes = response
            .bytes()
            .await
            .map_err(|err| Attempt::retryable(format!("reading response body: {err}")))?;
        let parsed: EmbeddingsResponse = serde_json::from_slice(&bytes)
            .map_err(|err| Attempt::Permanent(format!("unreadable response: {err}")))?;

        self.collect(texts.len(), parsed)
    }

    /// Restore input order from the `index` field and validate widths.
    fn collect(
        &self,
        expected: usize,
        parsed: EmbeddingsResponse,
    ) -> Result<Vec<Vec<f32>>, Attempt> {
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; expected];
        for datum in parsed.data {
            let Some(slot) = slots.get_mut(datum.index) else {
                return Err(Attempt::Permanent(format!(
                    "response indexed input {} but only {expected} were sent",
                    datum.index
                )));
            };
            if slot.is_some() {
                return Err(Attempt::Permanent(format!(
                    "response repeated index {}",
                    datum.index
                )));
            }
            *slot = Some(datum.embedding);
        }

        let mut out = Vec::with_capacity(expected);
        for (index, slot) in slots.into_iter().enumerate() {
            let vector = slot.ok_or_else(|| {
                Attempt::Permanent(format!("response omitted the vector for input {index}"))
            })?;
            if vector.is_empty() {
                return Err(Attempt::Permanent(format!(
                    "response gave an empty vector for input {index}"
                )));
            }
            if let Some(expected_dims) = self.settings.dimensions
                && vector.len() != expected_dims as usize
            {
                return Err(Attempt::Permanent(format!(
                    "model returned {} dimensions but config declares {expected_dims}",
                    vector.len()
                )));
            }
            out.push(vector);
        }
        Ok(out)
    }
}

/// Outcome of one HTTP attempt, before the retry policy has an opinion.
enum Attempt {
    Permanent(String),
    Retryable {
        message: String,
        retry_after: Option<Duration>,
        /// Report [`EmbedError::Permanent`] rather than
        /// [`EmbedError::Transient`] once the attempts run out. Set only for a
        /// 4xx that was *given* the benefit of the doubt by
        /// [`TRANSPORT_SIGNATURES`]: retrying is cheap, but calling it
        /// transient forever would let one bad input wedge the pipeline.
        permanent_if_exhausted: bool,
    },
}

impl Attempt {
    fn retryable(message: String) -> Self {
        Attempt::Retryable {
            message,
            retry_after: None,
            permanent_if_exhausted: false,
        }
    }
}

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    index: usize,
    embedding: Vec<f32>,
}

/// Split `texts` into batches honouring both budgets.
///
/// An item that exceeds `max_bytes` on its own still gets its own batch —
/// progress beats obedience, and the caller has already clipped every input to
/// [`EmbedSettings::max_embed_bytes`], which is far below any sane byte budget.
pub fn batches<'a, 'b>(
    texts: &'a [&'b str],
    max_items: usize,
    max_bytes: usize,
) -> Vec<&'a [&'b str]> {
    let max_items = max_items.max(1);
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0usize;

    for (i, text) in texts.iter().enumerate() {
        let full = i > start && (i - start >= max_items || bytes + text.len() > max_bytes);
        if full {
            out.push(&texts[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += text.len();
    }
    if start < texts.len() {
        out.push(&texts[start..]);
    }
    out
}

/// Exponential backoff with full-ish jitter: the nominal delay scaled into
/// `[0.5, 1.0)`. Jitter matters even with one client — the daemon restarts
/// against a server that is still loading, and unjittered retries from a
/// dozen batches land in lockstep.
fn backoff(attempt: u32, base: Duration) -> Duration {
    let nominal = base.saturating_mul(1u32 << attempt.min(16).saturating_sub(1));
    nominal.mul_f64(0.5 + jitter())
}

/// A cheap decorrelated `[0.0, 0.5)`. Deliberately not a `rand` dependency:
/// this is jitter, not cryptography, and the pin list stays shorter.
fn jitter() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 20))
        .unwrap_or(0);
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 29;
    (x % 1000) as f64 / 2000.0
}

/// `Retry-After` in delta-seconds. The HTTP-date form is ignored on purpose:
/// it only appears from caching proxies, which do not sit in front of a
/// loopback embedding server, and a misparsed date could mean an hour's wait.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let seconds: u64 = raw.trim().parse().ok()?;
    Some(Duration::from_secs(seconds))
}

/// Does this error body read as the endpoint's *own* network call failing?
///
/// Only the head is examined: the signatures appear in the error line, and a
/// megabyte of HTML is not worth scanning. Deliberately a substring test — the
/// wording varies by server, language and OS, and the cost of a false positive
/// is three extra attempts, not a wrong verdict (see [`Attempt::Retryable`]).
fn looks_like_transport(body: &str) -> bool {
    const HEAD: usize = 512;
    let head = truncate_bytes(body, HEAD).to_ascii_lowercase();
    TRANSPORT_SIGNATURES
        .iter()
        .any(|signature| head.contains(signature))
}

/// Error bodies go into logs; a model server's 500 can be a megabyte of HTML.
fn snippet(body: &str) -> String {
    const MAX: usize = 200;
    let trimmed = body.trim();
    let clipped = truncate_bytes(trimmed, MAX);
    match clipped.len() < trimmed.len() {
        true => format!("{clipped}…"),
        false => clipped.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(texts: &[&'static str]) -> Vec<&'static str> {
        texts.to_vec()
    }

    #[test]
    fn batching_respects_the_item_count() {
        let texts = strs(&["a", "b", "c", "d", "e"]);
        let split = batches(&texts, 2, BATCH_MAX_BYTES);
        assert_eq!(split.len(), 3);
        assert_eq!(split[0], ["a", "b"]);
        assert_eq!(split[2], ["e"]);
    }

    #[test]
    fn batching_respects_the_byte_budget() {
        let texts = strs(&["aaaa", "bbbb", "cc"]);
        let split = batches(&texts, 100, 8);
        assert_eq!(split.len(), 2, "{split:?}");
        assert_eq!(split[0], ["aaaa", "bbbb"]);
        assert_eq!(split[1], ["cc"]);
    }

    #[test]
    fn an_oversized_item_still_makes_progress() {
        let texts = strs(&["aaaaaaaa", "b"]);
        let split = batches(&texts, 100, 2);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0], ["aaaaaaaa"], "never an empty batch");
        assert!(batches(&[], 10, 10).is_empty());
    }

    #[test]
    fn every_text_appears_exactly_once_in_order() {
        let texts = strs(&["one", "two", "three", "four"]);
        let flat: Vec<&str> = batches(&texts, 3, 7)
            .into_iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        assert_eq!(flat, texts);
    }

    #[test]
    fn retry_after_parses_only_delta_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(2)));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn backoff_grows_and_stays_within_its_jitter_band() {
        for attempt in 1..=4u32 {
            let base = Duration::from_millis(100);
            let delay = backoff(attempt, base);
            let nominal = base * (1 << (attempt - 1));
            assert!(delay >= nominal / 2 && delay <= nominal, "{delay:?}");
        }
    }

    /// The body that started this: Ollama's 400 during Windows ephemeral-port
    /// exhaustion (#6). Ordinary rejections must stay unmatched, or every
    /// permanent 4xx pays the retry budget for nothing.
    #[test]
    fn transport_errors_are_recognised_inside_an_error_body() {
        assert!(looks_like_transport(
            "Post \"http://127.0.0.1:11434/api/embed\": dial tcp 127.0.0.1:11434: \
             connectex: Only one usage of each socket address is normally permitted."
        ));
        assert!(looks_like_transport("BIND: address already in use"));
        assert!(looks_like_transport("upstream request timeout"));

        assert!(!looks_like_transport("model \"nomic\" not found"));
        assert!(!looks_like_transport(
            "{\"error\":{\"message\":\"input exceeds the context window\"}}"
        ));
        assert!(!looks_like_transport(""));
        // Past the head, so it does not count — the signature belongs in the
        // error line, not in a megabyte of trailing HTML.
        assert!(!looks_like_transport(&format!(
            "{} dial tcp",
            "x".repeat(600)
        )));
    }

    #[test]
    fn settings_need_an_endpoint() {
        let mut config = EmbeddingsConfig::default();
        assert!(EmbedSettings::from_config(&config).is_none());
        config.endpoint = Some("   ".into());
        assert!(EmbedSettings::from_config(&config).is_none());

        config.endpoint = Some("http://127.0.0.1:8080/v1/".into());
        let settings = EmbedSettings::from_config(&config).unwrap();
        assert_eq!(settings.endpoint, "http://127.0.0.1:8080/v1");
        assert_eq!(settings.model, crate::config::DEFAULT_MODEL_ID);
        let client = EmbedClient::new(settings).unwrap();
        assert_eq!(client.url, "http://127.0.0.1:8080/v1/embeddings");
    }

    /// A zero here would otherwise mean "never issue a batch", i.e. a backlog
    /// that silently never drains.
    #[test]
    fn sizes_are_clamped_to_something_that_makes_progress() {
        let config = EmbeddingsConfig {
            endpoint: Some("http://127.0.0.1:8080/v1".into()),
            batch_max_items: 0,
            batch_max_bytes: 0,
            concurrency: 0,
            max_embed_bytes: 0,
            ..EmbeddingsConfig::default()
        };
        let settings = EmbedSettings::from_config(&config).unwrap();
        assert_eq!(settings.batch_max_items, 1);
        assert_eq!(settings.batch_max_bytes, MIN_BATCH_MAX_BYTES);
        assert_eq!(settings.concurrency, 1);
        assert_eq!(settings.max_embed_bytes, 256);

        let defaults = EmbeddingsConfig {
            endpoint: Some("http://127.0.0.1:8080/v1".into()),
            ..EmbeddingsConfig::default()
        };
        let settings = EmbedSettings::from_config(&defaults).unwrap();
        assert_eq!(
            settings.concurrency,
            crate::config::DEFAULT_EMBED_CONCURRENCY
        );
        assert_eq!(
            settings.batch_max_bytes,
            crate::config::DEFAULT_BATCH_MAX_BYTES
        );
        assert_eq!(
            settings.max_embed_bytes,
            crate::config::DEFAULT_MAX_EMBED_BYTES
        );
    }
}
