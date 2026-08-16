//! A stub OpenAI-compatible embeddings server, plus the fixture glue the
//! embedding tests share.
//!
//! Why a real socket rather than a mocked `reqwest::Client`: the behaviours
//! under test *are* HTTP behaviours — status classification, `Retry-After`,
//! response-order restoration, batch sizing. A mock that returns canned
//! `Result`s would test the code's own assumptions about HTTP, which is
//! exactly the thing worth doubting.
//!
//! Vectors are deterministic and *meaningful*: a tiny bag-of-synonym-groups
//! model (see [`stub_vector`]) so a test can construct a document that is
//! semantically close to a query while sharing no searchable words with it.
//! That is the only way to prove the vector arm surfaced something BM25 could
//! not have.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Any input containing this marker is rejected with a non-retryable 400 —
/// the stand-in for a chunk the real endpoint refuses.
pub const POISON: &str = "ZZPOISONZZ";

/// Width of every stub vector. Small enough to read in a failure message.
pub const DIMS: usize = 16;

/// Synonym groups. Every word in a group lands on the same dimension, which
/// is what lets a query and a document be "about the same thing" without
/// sharing a token.
const GROUPS: [&[&str]; 3] = [
    // dim 0 — how results are ordered
    &[
        "ordering",
        "ordered",
        "rank",
        "ranked",
        "ranking",
        "rrf",
        "reciprocal",
        "fusion",
        "fuses",
        "relevance",
    ],
    // dim 1 — the wire format
    &["json", "response", "payload", "serialize", "field"],
    // dim 2 — the filesystem watcher
    &["watcher", "watches", "debounce", "debounces", "filesystem"],
];

/// First dimension used for the hash-derived "everything else" signal.
const NOISE_BASE: usize = GROUPS.len();

/// Weight of an off-vocabulary word. Well below a group hit, so topical
/// similarity dominates but unrelated documents still differ from each other.
const NOISE_WEIGHT: f32 = 0.25;

/// A canned reply, consumed once, ahead of the normal behaviour.
#[derive(Debug, Clone)]
pub enum Reply {
    /// Fail with this status and no `Retry-After`.
    Status(u16),
    /// Fail with this status and `Retry-After: <secs>`.
    StatusRetryAfter(u16, u64),
    /// Succeed with exactly these vectors, one per input — the way to hand
    /// the worker a vector no honest model would produce.
    Vectors(Vec<Vec<f32>>),
}

#[derive(Default)]
pub struct StubState {
    /// Every POST, including retried ones.
    pub attempts: AtomicUsize,
    /// The `input` array of every POST, in arrival order.
    pub batches: Mutex<Vec<Vec<String>>>,
    /// Canned replies, popped from the front.
    pub script: Mutex<VecDeque<Reply>>,
    /// Return `data` in reverse order (with correct `index` values) so a
    /// client that trusts arrival order is caught.
    pub shuffle: bool,
    /// Milliseconds to stall before answering, so a test can act while the
    /// worker is provably mid-drain rather than racing it.
    pub delay_ms: AtomicUsize,
    /// Requests being served right now, and the most ever at once. Measured
    /// server-side because that is the only place the question "did the client
    /// actually overlap its batches?" has an honest answer.
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl StubState {
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// High-water mark of concurrent requests, over the stub's whole life.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// Count this request in for as long as the guard lives — including the
    /// early-return paths, which is why it is a guard and not a pair of calls.
    fn enter(self: &Arc<Self>) -> Busy {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        Busy(self.clone())
    }

    pub fn set_delay(&self, delay: std::time::Duration) {
        self.delay_ms
            .store(delay.as_millis() as usize, Ordering::SeqCst);
    }

    pub fn batches(&self) -> Vec<Vec<String>> {
        self.batches.lock().unwrap().clone()
    }

    /// Every `input` seen, flattened.
    pub fn inputs(&self) -> Vec<String> {
        self.batches().into_iter().flatten().collect()
    }

    pub fn script(&self, replies: impl IntoIterator<Item = Reply>) {
        *self.script.lock().unwrap() = replies.into_iter().collect();
    }
}

struct Busy(Arc<StubState>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Stub {
    /// Base URL including the `/v1` path segment, no trailing slash.
    pub base: String,
    pub state: Arc<StubState>,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl Stub {
    pub async fn start() -> Self {
        Self::with_state(StubState::default()).await
    }

    pub async fn shuffled() -> Self {
        Self::with_state(StubState {
            shuffle: true,
            ..StubState::default()
        })
        .await
    }

    pub async fn with_state(state: StubState) -> Self {
        let state = Arc::new(state);
        let router = Router::new()
            .route("/v1/embeddings", post(embeddings))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind stub");
        let port = listener.local_addr().expect("stub addr").port();
        let cancel = CancellationToken::new();

        let task = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(async move { cancel.cancelled().await })
                    .await;
            })
        };

        Self {
            base: format!("http://127.0.0.1:{port}/v1"),
            state,
            cancel,
            task: Some(task),
        }
    }

    /// Stop serving and *wait* for the socket to close, so a following
    /// request reliably gets a connection error rather than a race.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn embeddings(State(state): State<Arc<StubState>>, body: String) -> Response {
    let _busy = state.enter();
    state.attempts.fetch_add(1, Ordering::SeqCst);

    let request: Value = match serde_json::from_str(&body) {
        Ok(request) => request,
        Err(err) => return (StatusCode::BAD_REQUEST, format!("bad json: {err}")).into_response(),
    };
    let inputs: Vec<String> = request["input"]
        .as_array()
        .expect("input must be an array")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("input entries are strings")
                .to_string()
        })
        .collect();
    state.batches.lock().unwrap().push(inputs.clone());

    let delay = state.delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
    }

    let scripted = state.script.lock().unwrap().pop_front();
    if let Some(reply) = scripted {
        return match reply {
            Reply::Status(code) => (status(code), "scripted failure").into_response(),
            Reply::StatusRetryAfter(code, secs) => (
                status(code),
                [("retry-after", secs.to_string())],
                "scripted failure",
            )
                .into_response(),
            Reply::Vectors(vectors) => {
                assert_eq!(vectors.len(), inputs.len(), "scripted vectors per input");
                let data: Vec<Value> = vectors
                    .into_iter()
                    .enumerate()
                    .map(|(index, vector)| json!({ "index": index, "embedding": vector }))
                    .collect();
                axum::Json(json!({ "object": "list", "model": "stub", "data": data }))
                    .into_response()
            }
        };
    }

    if inputs.iter().any(|input| input.contains(POISON)) {
        return (StatusCode::BAD_REQUEST, "refusing this input").into_response();
    }

    let mut data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(index, text)| json!({ "index": index, "embedding": stub_vector(text) }))
        .collect();
    if state.shuffle {
        data.reverse();
    }
    axum::Json(json!({ "object": "list", "model": "stub", "data": data })).into_response()
}

fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).expect("valid status")
}

/// A tiny deterministic embedding: synonym-group counts in the first
/// [`NOISE_BASE`] dimensions, hashed word signal in the rest.
pub fn stub_vector(text: &str) -> Vec<f32> {
    let mut vector = vec![0f32; DIMS];
    for word in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        match GROUPS.iter().position(|group| group.contains(&word)) {
            Some(dim) => vector[dim] += 1.0,
            None => {
                let slot = NOISE_BASE + (hash(word) as usize % (DIMS - NOISE_BASE));
                vector[slot] += NOISE_WEIGHT;
            }
        }
    }
    // The store rejects a zero vector, and so would a real model's caller.
    if vector.iter().all(|value| *value == 0.0) {
        vector[0] = 1.0;
    }
    vector
}

/// FNV-1a. Stable across runs and platforms, unlike `DefaultHasher`.
fn hash(word: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in word.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Poll `check` until it holds, or fail after `label`'s deadline.
///
/// Bounded and short: every wait in these tests is for work already in
/// flight, never for a timer, so a passing run finishes in milliseconds and a
/// broken one fails fast instead of hanging the suite.
pub async fn until(label: &str, check: impl Fn() -> bool) {
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    const STEP: std::time::Duration = std::time::Duration::from_millis(10);
    let started = std::time::Instant::now();
    while started.elapsed() < DEADLINE {
        if check() {
            return;
        }
        tokio::time::sleep(STEP).await;
    }
    panic!("timed out waiting for: {label}");
}

/// Settings pointed at a stub, with retries fast enough to be invisible.
pub fn settings(base: &str) -> lore::embed::EmbedSettings {
    lore::embed::EmbedSettings {
        endpoint: base.to_string(),
        model: "stub-model".to_string(),
        api_key: None,
        dimensions: None,
        query_prefix: String::new(),
        document_prefix: String::new(),
        batch_max_items: 8,
        // Serial by default so a test that counts requests, or that walks the
        // backlog batch by batch, sees exactly the order it asks for. The
        // tests that are *about* overlap raise it themselves.
        concurrency: 1,
        retry: lore::embed::RetryPolicy {
            max_attempts: 4,
            base_delay: std::time::Duration::from_millis(5),
            max_delay: std::time::Duration::from_millis(50),
        },
        request_timeout: std::time::Duration::from_secs(5),
    }
}
