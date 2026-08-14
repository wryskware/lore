//! Shared types for the Lore daemon HTTP API.
//!
//! Every client (CLI subcommands, the `lore-mcp` thin proxy) talks to the
//! daemon exclusively through these types over versioned loopback HTTP
//! (design: D-0007). Nothing here touches index state.
//!
//! Wire-contract conventions:
//! - All routes live under `/v1/`; [`API_VERSION`] bumps on breaking change.
//! - Vault status travels as lowercase strings ("exploration", "leaning",
//!   "decided", "deprecated"); absence means unclassified. The daemon owns
//!   the authoritative enum; the wire stays stringly-typed so thin clients
//!   never need the internal model.
//! - Errors: non-2xx responses carry [`ApiError`] as JSON.

use serde::{Deserialize, Serialize};

/// API version negotiated on every request; bump on breaking changes.
pub const API_VERSION: u32 = 1;

/// Error body for non-2xx responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub message: String,
}

// ---------------------------------------------------------------------------
// GET /v1/status
// ---------------------------------------------------------------------------

/// Daemon health/status snapshot returned by `GET /v1/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub api_version: u32,
    pub daemon_version: String,
    /// Monotonic index generation (bumps once per completed index pass).
    pub generation: u64,
    pub projects: Vec<ProjectStatus>,
    pub embeddings: EmbeddingStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatus {
    pub id: i64,
    pub name: String,
    /// Absolute root path as registered.
    pub root: String,
    pub files: u64,
    pub chunks: u64,
    /// Chunks with a stored vector under the current fingerprint.
    pub embedded_chunks: u64,
}

/// State of the external embedding endpoint (D-0007: absence degrades to
/// lexical-only search and must be visible here, never silent).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EmbeddingStatus {
    /// No endpoint configured; search is lexical-only.
    Unconfigured,
    /// Endpoint configured but last probe failed; search is lexical-only.
    Unreachable { endpoint: String, error: String },
    /// Endpoint healthy.
    Ready { endpoint: String, model: String },
}

// ---------------------------------------------------------------------------
// POST /v1/projects  (register — CLI-only by convention; not exposed via MCP)
// GET  /v1/projects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterProjectRequest {
    /// Absolute path to the project root.
    pub root: String,
    /// Display name; defaults to the root's final path component.
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: i64,
    pub name: String,
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectList {
    pub projects: Vec<ProjectInfo>,
}

// ---------------------------------------------------------------------------
// POST /v1/index  (trigger a rescan/reindex)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexRequest {
    /// Project name or id; `None` reindexes all registered projects.
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexResponse {
    /// Projects queued for (re)indexing.
    pub queued: Vec<ProjectInfo>,
}

// ---------------------------------------------------------------------------
// POST /v1/search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    /// Restrict to one project by name or id.
    pub project: Option<String>,
    /// Project-relative path prefix filter (forward slashes).
    pub path_prefix: Option<String>,
    /// Lowercase language tag filter ("csharp", "markdown", …).
    pub language: Option<String>,
    /// Vault-status filter: any of "exploration", "leaning", "decided",
    /// "deprecated", "unclassified". Empty/absent = no status filter.
    #[serde(default)]
    pub status: Vec<String>,
    /// Max results; daemon clamps to a sane ceiling.
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    /// True when vectors did not participate (no/unhealthy endpoint or
    /// unembedded corpus) — lexical-only degradation, D-0007.
    pub lexical_only: bool,
}

/// One ranked chunk with provenance and authority at a glance (4.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk_id: String,
    pub project: String,
    /// Project-relative path, forward slashes.
    pub path: String,
    /// 1-based inclusive line span.
    pub line_start: u32,
    pub line_end: u32,
    pub language: Option<String>,
    /// For code chunks: dotted symbol path (e.g. `Board.Update`).
    pub symbol_path: Option<String>,
    /// For Markdown chunks: root-to-leaf heading titles.
    pub heading_path: Option<Vec<String>>,
    /// Vault status label; `None` = not a vault doc or unclassified.
    pub design_status: Option<String>,
    /// Decision IDs cited by the file's frontmatter plus this chunk's body.
    pub decision_refs: Vec<String>,
    /// Fused relevance score (higher is better; comparable within one
    /// response only).
    pub score: f64,
    /// Chunk text, possibly truncated to keep `search` token-lean; use
    /// `expand` for full context.
    pub excerpt: String,
    pub excerpt_truncated: bool,
}

// ---------------------------------------------------------------------------
// POST /v1/expand
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandRequest {
    /// Project name or id (as returned in [`SearchResult::project`]).
    pub project: String,
    pub chunk_id: String,
    /// Extra context lines around the chunk (daemon default/clamp applies).
    pub context_lines: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandResponse {
    pub path: String,
    /// 1-based inclusive line span of the returned text within the file.
    pub line_start: u32,
    pub line_end: u32,
    /// The requested chunk plus surrounding context.
    pub text: String,
    /// Full-file line count, so the caller knows whether more exists.
    pub file_lines: u32,
}
