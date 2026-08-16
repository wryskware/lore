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

/// Shortest chunk-id prefix [`ExpandRequest::chunk_id`] will resolve.
///
/// Eight hex characters is 32 bits. Within the one project an `expand` is
/// scoped to — tens of thousands of chunks, not the whole machine — that is a
/// collision probability small enough that a prefix is a handle rather than a
/// gamble, and an ambiguous one is answered with the candidates rather than a
/// guess. It is also short enough to type from a terminal, which is the only
/// reason to accept anything below what `search` actually prints.
pub const MIN_CHUNK_ID_PREFIX: usize = 8;

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
    /// Chunks the embed worker has given up on **this process lifetime**: the
    /// endpoint refused them twice, so they are held back rather than retried
    /// forever. Not a permanent verdict — each is offered again when its
    /// poison window expires — and not persisted, because it describes what
    /// this daemon did, not what the index is.
    ///
    /// Reported because a corpus quietly missing some of its vectors is
    /// exactly the silent degradation D-0007 refuses to allow. Zero (and
    /// absent from the JSON) is the normal case, and from a daemon that
    /// predates the field.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub embed_abandoned: u64,
    /// Request-latency percentiles per endpoint over a rolling window.
    /// Empty/absent from a daemon that predates latency metrics (additive on
    /// the wire in both directions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latency: Vec<EndpointLatency>,
}

/// `skip_serializing_if` predicate for additive counters whose interesting
/// value is "not zero".
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Nearest-rank latency percentiles for one endpoint (`search`,
/// `search_embed` — the embed-query wait inside search — or `expand`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointLatency {
    pub endpoint: String,
    /// Lifetime request count; percentiles cover the most recent window.
    pub samples: u64,
    pub p50_ms: u64,
    pub p90_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
}

/// One project's index state.
///
/// `Default` is derived because this struct is deliberately *additive* — every
/// new field carries `#[serde(default)]` — and constructing one field by field
/// in a client or a test makes every addition a mechanical edit of code that
/// does not care about the new field. `..Default::default()` is the shape that
/// keeps those sites honest about what they are actually asserting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Authority profile this repository opted into via its committed
    /// `.lore.toml` (D-0012), e.g. "lore-v1". Absent = a neutral repository:
    /// no `design_status`/`decision_refs` parsing, no path ceilings, no
    /// authority metadata and no authority weights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_profile: Option<String>,
    /// How far the declared profile reaches: "off", "annotate" or "rank".
    /// Present exactly when [`Self::authority_profile`] is; "off" means the
    /// profile is declared but suspended, which is reported rather than hidden
    /// so it is distinguishable from never having configured anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_behavior: Option<String>,
    /// The repository's `.lore.toml` could not be used — unknown profile,
    /// unknown key, malformed TOML. The repository still indexes, neutrally;
    /// D-0012 requires that be loud rather than silently a different authority
    /// model, which is what this field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_config_error: Option<String>,
    /// Decisions that are accepted and not retired.
    #[serde(default)]
    pub decisions_active: u64,
    /// Decisions that exist at all, whatever their status. Records excluded
    /// for an identity defect are not counted here — they are in
    /// [`Self::decision_violations`].
    #[serde(default)]
    pub decisions_total: u64,
    /// Defects in the decision corpus itself (D-0013): a per-file record whose
    /// heading disagrees with its filename, or two records claiming one id.
    /// One human-readable line each, path included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_violations: Vec<String>,
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
// DELETE /v1/projects/{name-or-key}  (deregister — CLI-only, not exposed via MCP)
// ---------------------------------------------------------------------------

/// What deregistering a project actually destroyed. The counts are read
/// *before* the delete, so the caller can be told the size of what it just
/// discarded rather than a row of zeroes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveProjectResponse {
    pub project: ProjectInfo,
    /// File records forgotten with the project.
    #[serde(default)]
    pub files: u64,
    /// Chunks (and their FTS rows and vectors) forgotten with the project.
    #[serde(default)]
    pub chunks: u64,
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
// POST /v1/shutdown  (clean stop — CLI-only, not exposed via MCP)
// ---------------------------------------------------------------------------

/// Acknowledgement that a clean shutdown has begun. Answered *before* the
/// daemon is gone — it is still serving this very request — so a caller that
/// needs "actually stopped" waits for the handshake file to disappear, which
/// the daemon removes as its last act.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownResponse {
    /// The process that is stopping, so a client can tell a daemon that obeyed
    /// from a successor that replaced it while it was waiting.
    pub pid: u32,
}

// ---------------------------------------------------------------------------
// POST /v1/search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    /// Scope the query to one project by name or id. **Required**: a request
    /// naming neither this nor [`Self::project_key`] is rejected, because
    /// every query is scoped to exactly one project.
    pub project: Option<String>,
    /// Stable opaque project key, as returned by
    /// [`SearchResult::project_key`], [`ProjectInfo::key`] and
    /// `GET /v1/resolve`. Takes precedence over [`Self::project`] when both
    /// are given — the same rule [`ExpandRequest::project_key`] follows, and
    /// for the same reason: the key identifies a source exactly where a
    /// display name only usually does.
    #[serde(default)]
    pub project_key: Option<String>,
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
    /// Full content-addressed chunk id. The wire always carries it whole;
    /// shortening for display is a renderer's decision, and `expand` accepts
    /// any prefix of at least [`MIN_CHUNK_ID_PREFIX`] characters so a
    /// shortened one still round-trips.
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
    ///
    /// **Absent** when the result's repository has no authority profile in
    /// force (D-0012). That is not "neutral": neutral is a verdict, and a
    /// repository that never opted in has not been judged at all. Also absent
    /// from a daemon that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_authority: Option<String>,
    /// Why the effective authority is below the declared one. Present *only*
    /// when the document was demoted, e.g. "decided declared but cites no
    /// active decision" or "99_Scratch path cap".
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// The chunk to read, as a full id **or any prefix of it** of at least
    /// [`MIN_CHUNK_ID_PREFIX`] hex characters — renderers print a shortened id
    /// to keep the search surface token-lean, and it is meant to be passed
    /// straight back. A prefix matching more than one chunk in the scoped
    /// project is a 400 listing the candidates, never a guess.
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
