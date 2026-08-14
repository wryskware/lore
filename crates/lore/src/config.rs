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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub embeddings: EmbeddingsConfig,
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

    /// What `GET /v1/status` should report about embeddings.
    ///
    /// This work package ships no embedding client, so a *configured*
    /// endpoint is reported as [`lore_core::EmbeddingStatus::Unreachable`]
    /// with an explicit reason rather than `Ready`: vectors genuinely do not
    /// participate in ranking yet, and D-0007 requires that degradation be
    /// visible. The embeddings package replaces this with a real probe.
    pub fn embedding_status(&self) -> lore_core::EmbeddingStatus {
        match &self.embeddings.endpoint {
            None => lore_core::EmbeddingStatus::Unconfigured,
            Some(endpoint) => lore_core::EmbeddingStatus::Unreachable {
                endpoint: endpoint.clone(),
                error: "embedding client not implemented yet; search is lexical-only".to_string(),
            },
        }
    }
}
