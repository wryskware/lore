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

pub mod discovery;

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
    /// Stable opaque project key — the handle `expand` should be given, and
    /// the one thing about a project that never changes. Optional on the wire
    /// so a daemon that predates keys still parses.
    #[serde(default)]
    pub key: String,
    /// Absolute root path as registered.
    pub root: String,
    /// Corpus kind: "repo" today, "session" from M3. Empty from a daemon that
    /// predates provenance.
    #[serde(default)]
    pub kind: String,
    pub files: u64,
    pub chunks: u64,
    /// Chunks with a stored vector under the current fingerprint.
    pub embedded_chunks: u64,
    /// Files whose authority declaration does not hold up — today, documents
    /// declaring `design_status: decided` without citing an active decision.
    /// Lore never edits the declaration; it demotes the document's ranking
    /// authority and reports the count here, because a silent demotion means
    /// an author believes something is canon and Lore quietly disagrees.
    #[serde(default)]
    pub authority_violations: u64,
    /// The first few offending paths, so the report is actionable. Capped by
    /// the daemon; the count above is the complete figure.
    #[serde(default)]
    pub authority_violation_paths: Vec<String>,
    /// Live filesystem-watch coverage. Added after 0.1.0, so it is optional
    /// on the wire: a daemon that predates it simply reports nothing and the
    /// client sees [`WatchState::Unknown`].
    #[serde(default)]
    pub watch: WatchState,
}

/// Whether a project's edits are being indexed live.
///
/// A watch that is not armed is not a hard failure — an explicit reindex
/// still works — but it is a silent one unless it is reported, which is the
/// same reasoning that puts [`EmbeddingStatus`] on this surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchState {
    /// A recursive watch is armed; edits reach the index without a reindex.
    Armed,
    /// The watch is not armed (the initial arm failed, or the platform
    /// invalidated it) and the daemon is retrying with backoff. Until it
    /// succeeds, changes are only seen on an explicit index.
    Retrying,
    /// This daemon does not report watcher coverage.
    #[default]
    Unknown,
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
    /// Stable opaque key (see [`ProjectStatus::key`]).
    #[serde(default)]
    pub key: String,
    pub root: String,
    /// "repo" | "session"; empty from a daemon that predates provenance.
    #[serde(default)]
    pub kind: String,
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
    /// **Declared** vault-status filter: any of "exploration", "leaning",
    /// "decided", "deprecated", "unclassified". Empty/absent = no status
    /// filter. This filters on what documents declare about themselves, which
    /// is a different question from how they rank — see
    /// [`SearchResult::effective_authority`].
    #[serde(default)]
    pub status: Vec<String>,
    /// Corpus kinds to search: any of "repo", "session". Absent = every kind,
    /// which is the only thing v1 can produce. `recall` at M3 is this filter
    /// set to `["session"]`.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
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
    /// Project display name — for humans and log lines only. It is not
    /// guaranteed unique across daemons that predate name enforcement, which
    /// is why [`Self::project_key`] exists.
    pub project: String,
    /// Stable opaque project key. Pass this back as
    /// [`ExpandRequest::project_key`]; it identifies the source exactly, where
    /// the display name only usually does.
    #[serde(default)]
    pub project_key: String,
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
    /// **Declared** vault status — what the document says about itself.
    /// `None` = not a vault doc, or unclassified. Lore never edits this.
    pub design_status: Option<String>,
    /// **Effective** authority — what ranking actually used: "decided",
    /// "leaning", "neutral" or "deprecated". It differs from
    /// [`Self::design_status`] when a declaration failed validation or a path
    /// or source ceiling applied, and `neutral` is reported rather than a
    /// status word because tier 1 covers exploration, unclassified, plain code
    /// and demoted declarations alike.
    #[serde(default)]
    pub effective_authority: String,
    /// Why the effective authority is below the declared one. Present *only*
    /// when the document was demoted, e.g. "decided declared but cites no
    /// active decision" or "9_Scratch path cap".
    #[serde(default)]
    pub authority_note: Option<String>,
    /// Decision IDs cited by the file's frontmatter plus this chunk's body.
    /// Metadata only: citing a decision carries no ranking weight, because
    /// authority flows *from* the ledger to documents it names, never to
    /// whoever quotes a number.
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
    /// Optional now that [`Self::project_key`] exists; resolution by display
    /// name is ambiguous whenever two roots share a name.
    #[serde(default)]
    pub project: String,
    /// Stable opaque project key from [`SearchResult::project_key`]. Takes
    /// precedence over [`Self::project`] when both are given.
    #[serde(default)]
    pub project_key: Option<String>,
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
