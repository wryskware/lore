//! `config.toml` — the daemon's optional configuration file.
//!
//! Lives at `<data-dir>/config.toml` (see [`crate::daemon::paths`]). Every
//! key is optional and the file itself is optional: an absent file is exactly
//! equivalent to an empty one, so a fresh install needs no configuration at
//! all and degrades to lexical-only search (D-0007).
//!
//! The shape is fixed by design/3_Retrieval/3.1 §"Embedding provider config".
//! Unknown keys are **rejected** rather than ignored: a silently-misspelled
//! `endpoint` would present as "embeddings mysteriously never turn on", which
//! is the exact failure mode D-0007 says must never be silent.

use anyhow::{Context, Result};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};

/// File name within the data directory.
pub const CONFIG_FILE: &str = "config.toml";

/// Batch size used by the embedding pipeline when the file says nothing.
pub const DEFAULT_BATCH_MAX_ITEMS: usize = 64;

/// Wire-batch byte budget when the file says nothing — the historical 64 KiB
/// constant from [`crate::embed::client::BATCH_MAX_BYTES`], sized for a
/// modest local server. A GPU-backed server draining a large backlog wants
/// this several times larger, so fewer round trips carry the same corpus.
pub const DEFAULT_BATCH_MAX_BYTES: usize = crate::embed::client::BATCH_MAX_BYTES;

/// Batches the embed worker keeps in flight at once when the file says
/// nothing. Local servers serve several requests at a time and queue the
/// rest; a serial worker leaves the server idle for the whole round-trip of
/// every batch, and each in-flight batch also spends time off the wire
/// (response parsing, the store upsert), so keeping more batches in flight
/// than the server has slots is what keeps its queue from running dry.
pub const DEFAULT_EMBED_CONCURRENCY: usize = 8;

/// Embed-text byte budget when the file says nothing — the historical 8 KiB
/// ceiling from [`crate::embed::text::MAX_EMBED_TEXT_BYTES`]. Keeping the
/// default equal to the old constant means upgrading lore does not change any
/// vector, so no existing vault re-embeds.
pub const DEFAULT_MAX_EMBED_BYTES: usize = crate::embed::text::MAX_EMBED_TEXT_BYTES;

/// Model id sent (and fingerprinted) when the file names no model.
///
/// `llama-server` ignores the field entirely, so requiring it would block the
/// simplest valid configuration — `endpoint` and nothing else. The literal is
/// still part of the fingerprint, so switching to a named model later is a
/// visible change that forces a re-embed rather than silent vector mixing.
pub const DEFAULT_MODEL_ID: &str = "default";

/// Quiet period before a burst of watcher events becomes an index pass, when
/// the file says nothing.
///
/// Twenty seconds, not the sub-second value a "live" watcher suggests: bench
/// round 1 showed agent retrieval is front-loaded at task start, so searching
/// for code *while* editing it is pathological and sub-10s freshness buys
/// nothing (D-0015). What it costs is real — every micro-batch is a pass, a
/// generation bump and an embed wake. `lore index` remains the immediate path
/// for anyone who wants the index now.
///
/// An operational default, tunable without a new decision.
pub const DEFAULT_WATCH_DEBOUNCE_SECS: u64 = 20;

/// Floor on how often a *receiving* daemon accepts a manifest for one project
/// (D-0015: "a receiving daemon enforces a hard minimum push interval and
/// rejects clients configured hotter").
///
/// Ten seconds, half the watcher debounce above: a pusher pacing itself at the
/// intended ~20s cadence never comes near the floor even with jitter or a
/// manual `lore index` in between, while a client configured for the sub-10s
/// freshness D-0015 says buys nothing is refused by name rather than quietly
/// costing the daemon a pass, a generation bump and an embed wake per push.
///
/// It is a floor, not a schedule: nothing here makes a slow pusher push, and
/// the refusal names the number so a misconfigured client can be fixed.
///
/// An operational default, tunable without a new decision.
pub const DEFAULT_PUSH_MIN_INTERVAL_SECS: u64 = 10;

/// How long a push lease survives silence before the daemon reaps it and
/// deletes its staging area.
///
/// Thirty seconds. With takeover as the conflict policy (D-0015) the TTL never
/// blocks a successor — an acquirer displaces the holder immediately, whatever
/// the clock says — so this number only decides how long a dead pusher's
/// staged files sit on disk and how long `status` keeps naming an epoch nobody
/// holds. Long enough that an ordinary pause between uploads is not a lost
/// lease; short enough that a crashed pusher is forgotten in seconds rather
/// than at cleanup.
///
/// An operational default, tunable without a new decision.
pub const DEFAULT_PUSH_LEASE_TTL_SECS: u64 = 30;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub embeddings: EmbeddingsConfig,
    pub watcher: WatcherConfig,
    pub push: PushConfig,
}

/// Filesystem-watch behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WatcherConfig {
    /// Seconds of quiet before a burst of events is delivered as one batch.
    /// Long enough to absorb an editor's write-temp-then-rename dance, a Unity
    /// asset import, or a whole `cargo build`.
    pub debounce_secs: u64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_secs: DEFAULT_WATCH_DEBOUNCE_SECS,
        }
    }
}

impl WatcherConfig {
    /// The debounce actually handed to the watcher. Clamped to a second at the
    /// bottom — a zero-length quiet period is not "immediate", it is a
    /// debouncer that fires once per event, which is the storm behaviour the
    /// pump exists to prevent. `lore index` is the immediate path.
    pub fn debounce(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.debounce_secs.max(1))
    }
}

/// The receiving side of the push protocol (D-0015).
///
/// Both keys are the *receiver's* policy, which is why they live here and the
/// mass-delete override does not: the override is a human's per-invocation
/// decision about one push and D-0015 forbids it as a config key, while these
/// two describe what this daemon will put up with from any client at all.
///
/// Nothing here is an authorization concept. The daemon still binds loopback
/// only, has no users and no roles; a lease is a consistency primitive and
/// these are its cadences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PushConfig {
    /// Seconds that must pass between accepted manifests for one project.
    pub min_interval_secs: u64,
    /// Seconds of silence before a push lease is reaped and its staging area
    /// deleted.
    pub lease_ttl_secs: u64,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            min_interval_secs: DEFAULT_PUSH_MIN_INTERVAL_SECS,
            lease_ttl_secs: DEFAULT_PUSH_LEASE_TTL_SECS,
        }
    }
}

impl PushConfig {
    /// The floor as handed to the lease manager. Zero is honoured: a
    /// deployment whose pusher *is* the cadence (a test, a one-shot forwarder)
    /// can switch the floor off, which is different from the debounce's clamp
    /// because a floor of zero is a coherent policy while a debounce of zero is
    /// a debouncer that fires once per event.
    pub fn min_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.min_interval_secs)
    }

    /// Clamped to a second at the bottom: a zero TTL would reap every lease
    /// before its holder could use it.
    pub fn lease_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.lease_ttl_secs.max(1))
    }

    /// The renewal cadence advertised to pushers — a third of the TTL, so two
    /// heartbeats can be lost before a lease is at risk.
    pub fn heartbeat(&self) -> std::time::Duration {
        std::time::Duration::from_secs((self.lease_ttl_secs / 3).max(1))
    }
}

/// External OpenAI-compatible embedding endpoint (D-0003: local-only; the
/// daemon never manages the server process in v0.1, D-0007).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbeddingsConfig {
    /// Base URL, e.g. `http://127.0.0.1:8080/v1`. Absent ⇒ lexical-only.
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<u32>,
    /// Model-card instruction prefixes; part of the persisted fingerprint.
    pub query_prefix: String,
    pub document_prefix: String,
    pub batch_max_items: usize,
    /// Byte budget for one HTTP request's total input text. A worker batch
    /// larger than this is split into several wire batches, sent
    /// concurrently. Like `concurrency`, not part of the fingerprint: it
    /// changes how vectors arrive, never what they are.
    pub batch_max_bytes: usize,
    /// Batches the worker may have in flight at once; `1` is the serial
    /// pipeline. Deliberately *not* part of the embedding fingerprint: it
    /// changes when vectors arrive, never what they are.
    pub concurrency: usize,
    /// Byte budget for one embed input (prefix + header + chunk text; the
    /// tail of the text is clipped to fit). Servers that reject rather than
    /// truncate oversized inputs (llama-server's 400) need this to sit under
    /// the model's per-slot token window at a *conservative* bytes-per-token
    /// ratio — dense JSON tokenizes near 2 bytes/token, not prose's 4. Part
    /// of the fingerprint: clipping changes what a vector is.
    pub max_embed_bytes: usize,
    /// Sent as `Authorization: Bearer …`. Local servers usually ignore it,
    /// but some (llama-server started with `--api-key`) demand *something*.
    /// Absent ⇒ no header at all.
    pub api_key: Option<String>,
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            model: None,
            dimensions: None,
            query_prefix: String::new(),
            document_prefix: String::new(),
            batch_max_items: DEFAULT_BATCH_MAX_ITEMS,
            batch_max_bytes: DEFAULT_BATCH_MAX_BYTES,
            concurrency: DEFAULT_EMBED_CONCURRENCY,
            max_embed_bytes: DEFAULT_MAX_EMBED_BYTES,
            api_key: None,
        }
    }
}

impl EmbeddingsConfig {
    /// The model id actually used on the wire and in the fingerprint.
    pub fn model_id(&self) -> &str {
        match self.model.as_deref() {
            Some(model) if !model.trim().is_empty() => model,
            _ => DEFAULT_MODEL_ID,
        }
    }
}

impl Config {
    /// Read `<data_dir>/config.toml`. A missing file yields defaults; a
    /// malformed one is a hard error (starting with silently-wrong config is
    /// worse than not starting).
    pub fn load(data_dir: &Utf8Path) -> Result<Self> {
        let path = data_dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("parsing {path}")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("reading {path}")),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// The embedding status implied by configuration *alone*, before anything
    /// has talked to the endpoint.
    ///
    /// A configured endpoint starts as [`lore_core::EmbeddingStatus::Unreachable`]
    /// rather than `Ready`: until the first probe succeeds, vectors genuinely
    /// do not participate in ranking, and D-0007 requires that degradation be
    /// visible rather than assumed away. [`crate::embed::Embedder`] takes this
    /// as its initial state and replaces it with live probe results.
    pub fn embedding_status(&self) -> lore_core::EmbeddingStatus {
        match &self.embeddings.endpoint {
            None => lore_core::EmbeddingStatus::Unconfigured,
            Some(endpoint) => lore_core::EmbeddingStatus::Unreachable {
                endpoint: endpoint.clone(),
                error: "endpoint not probed yet; search is lexical-only until it answers"
                    .to_string(),
            },
        }
    }
}
