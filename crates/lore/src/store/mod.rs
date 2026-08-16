//! SQLite SearchStore — metadata + FTS5 lexical + vectors in one database,
//! one transaction domain (Architecture 1.1 storage model; SearchStore seam
//! for a later Tantivy+arroy engine).
//!
//! # Seam discipline
//!
//! Nothing in the public surface of this module mentions SQLite: no
//! `rusqlite` types in arguments, returns, or the error enum's public shape.
//! A future Tantivy+arroy engine implements the same methods over the same
//! plain types ([`crate::types::Chunk`], [`SearchFilter`], [`SearchHit`], …).
//!
//! # Ownership and threading
//!
//! [`Store`] is a **synchronous, single-owner** library type. It is `Send` but
//! not `Sync` (it holds a `rusqlite::Connection`), which is exactly the
//! constraint canon wants: one authoritative owner of index state (D-0007).
//! The daemon should hold it behind a mutex (or in a dedicated blocking task)
//! and never hand out clones of the connection. Mutating calls take
//! `&mut self`, so the borrow checker enforces write serialization at compile
//! time.
//!
//! # Transaction domain
//!
//! Metadata, the FTS5 index and the vectors live in one database file, so
//! `replace_file_chunks` is a single atomic transaction across all three.
//! FTS5 is an *external-content* table synchronized by triggers (see
//! [`schema`]), meaning an index row cannot possibly be written outside the
//! transaction that wrote its chunk.

mod query;
mod schema;
/// Crate-visible so the embed worker can screen vectors with the *same*
/// predicate the write path uses (see [`vector::is_usable`]).
pub(crate) mod vector;

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::authority::{Authority, DecisionViolation, Decisions, Declared, Demotion};
use crate::repo_config::{Behavior, Profile, RepoAuthority};
use crate::types::{
    Chunk, ChunkId, ChunkKind, DesignStatus, SourceKind, VaultMeta, authority_tier,
};

use query::{filter_sql, or_fts_query, sanitize_fts_terms};
use vector::{Scored, TopK};

/// Opaque project handle. Stable for the life of the database.
pub type ProjectId = i64;

pub type Result<T> = std::result::Result<T, StoreError>;

/// How long to wait for a competing writer before giving up. The daemon is
/// the only writer, but CLI/read tooling may attach to the same file.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Chunk columns, in the order [`row_to_chunk`] and [`row_to_authority`]
/// expect. `c` is the chunks table alias in every query that uses this.
const CHUNK_COLS: &str = "c.project_id, c.chunk_id, c.path, c.kind, c.language, \
     c.byte_start, c.byte_end, c.line_start, c.line_end, c.text, c.vault, \
     c.effective_tier, c.demotion";

/// Number of columns [`CHUNK_COLS`] selects, so callers that append their own
/// (a score, a rowid) index past it instead of hard-coding a number that
/// silently rots when a column is added.
const CHUNK_COL_COUNT: usize = 13;

/// How many offending file paths [`Store::status`] lists per project. Enough
/// to fix them, few enough that a status call stays a status call.
pub const MAX_VIOLATION_PATHS: usize = 5;

/// BM25 field weights: `text`, `path`, `anchor`.
///
/// A hit on a symbol name or heading (`anchor`) is worth more than a hit in
/// the body, and a hit in the path is worth least — but bodies *are* indexed
/// and *do* score, deliberately: names-only BM25 is the CodeGraph mistake
/// called out in 3.1. Weights are tuning, not canon.
const BM25_WEIGHTS: &str = "1.0, 0.5, 2.0";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("schema migration failed: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("chunk metadata (de)serialization failed: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("chunk {chunk_id} claims path `{chunk_path}` but was written for file `{file_path}`")]
    PathMismatch {
        chunk_id: String,
        chunk_path: String,
        file_path: String,
    },
    #[error("embedding vector for chunk {chunk_id} is unusable (empty, zero-length or non-finite)")]
    InvalidVector { chunk_id: String },
    #[error("query vector is unusable (empty, zero-length or non-finite)")]
    InvalidQueryVector,
    #[error("embedding dimension mismatch: query has {query} dims, stored vector has {stored}")]
    DimensionMismatch { query: usize, stored: usize },
    #[error("stored row is corrupt: {0}")]
    Corrupt(String),
}

/// A registered source: a repo root today, a session corpus at M3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Internal row id. Non-authoritative and never on the wire — [`Self::key`]
    /// is the stable public handle (S1#3).
    pub id: ProjectId,
    pub root: Utf8PathBuf,
    pub name: String,
    /// Stable opaque key, assigned once at registration (see
    /// [`crate::registry`]). Never recomputed, unchanged by a rename.
    pub key: String,
    pub kind: SourceKind,
    /// The repo's `.lore.toml` authority configuration as of the last index
    /// pass (D-0012). Neutral for a source that has never been scanned, for
    /// one with no config, and for one whose config is broken — the last of
    /// which carries its error here so `status` can shout about it.
    pub authority: RepoAuthority,
}

/// One source as the caller wants it to exist after
/// [`Store::apply_project_set`] — the desired end state of a row, not a delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSpec {
    /// The existing row this describes, or `None` to insert a new one.
    ///
    /// Resolved by the caller: deciding that a manifest root names an existing
    /// project is a path comparison ([`crate::registry`] does it
    /// case-insensitively on Windows), and the store does not own path
    /// semantics. Supplying the id also lets a root be *respelled* — same
    /// directory, different separators or case — without the store mistaking it
    /// for a second project.
    pub id: Option<ProjectId>,
    pub root: Utf8PathBuf,
    pub name: String,
    /// The key this row must end up with. Unlike [`Store::upsert_project`]
    /// there is no "keep whatever is there" mode: the manifest is
    /// authoritative, so the caller always knows the answer.
    pub key: String,
    pub kind: SourceKind,
}

/// A file's indexing state, for change detection and orphan pruning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub path: Utf8PathBuf,
    pub content_hash: String,
    /// Unix seconds at last successful index.
    pub indexed_at: i64,
    /// Source-declared timestamp (a session's write time), as opposed to when
    /// Lore happened to index it. `None` for every repo file in v1; the M3
    /// recency term reads it.
    pub source_ts: Option<i64>,
}

/// What [`Store::replace_file_chunks`] actually did — useful for logs and for
/// asserting that content-addressed ids are earning their keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileWrite {
    /// Chunk ids that did not exist before.
    pub inserted: usize,
    /// Chunk ids that already existed (their embeddings survive).
    pub kept: usize,
    /// Chunk ids that were present and are no longer produced.
    pub deleted: usize,
    /// The effective authority stamped on every chunk of this file. Returned
    /// (rather than merely written) so the indexer can log a violation with
    /// the project and path in scope — the store knows the verdict, only the
    /// caller knows the context (D-0007: degradation is never silent).
    pub authority: Authority,
}

/// `design_status` filter atom. `Unclassified` matches chunks with no
/// `design_status` (non-vault files and vault files missing frontmatter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    Unclassified,
    Status(DesignStatus),
}

/// Pre-scoring restriction, applied in SQL before any ranking happens.
///
/// `None` on a field means "no restriction". `statuses: Some(vec![])` means
/// "nothing is acceptable" and yields no results.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchFilter {
    pub project: Option<ProjectId>,
    /// Project-relative path prefix, e.g. `design/` or `Assets/Scripts`.
    /// Matched literally (not a `LIKE` pattern).
    pub path_prefix: Option<String>,
    /// Exact language tag as stored on the chunk ("csharp", "markdown", …).
    pub language: Option<String>,
    /// Allowed **declared** `design_status` values. Declared on purpose: a
    /// caller asking for `decided` material is asking what documents say about
    /// themselves, which is a different question from how they rank.
    pub statuses: Option<Vec<StatusFilter>>,
    /// Minimum **effective** authority tier (see [`crate::authority`]) — e.g.
    /// `Some(1)` drops `deprecated` and `9_Scratch` material without
    /// enumerating every other status.
    pub min_authority: Option<u8>,
    /// Allowed source kinds. `None` = every kind, which is the only thing v1
    /// can produce; `recall` at M3 is this filter set to `[Session]`, with no
    /// path convention leaking through the store API.
    pub source_kinds: Option<Vec<SourceKind>>,
}

impl SearchFilter {
    /// Convenience for the common "one project, no other restriction" case.
    pub fn project(project: ProjectId) -> Self {
        Self {
            project: Some(project),
            ..Self::default()
        }
    }
}

/// A ranked result. `score` is always **higher is better**, whichever engine
/// produced it (BM25 is negated; vector scores are cosine in `[-1, 1]`).
/// Scores from different engines are not comparable — fusion is the caller's
/// job (RRF, per 3.1).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub project: ProjectId,
    pub chunk: Chunk,
    /// Effective authority as stored — the ranking multiplier's input, and the
    /// source of the result's `effective_authority` / `authority_note`.
    pub authority: Authority,
    pub score: f32,
}

/// A chunk awaiting embedding, with everything the embed pipeline needs to
/// build its prefixed embedding text (language, path, symbol/heading anchor).
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedCandidate {
    pub project: ProjectId,
    /// Storage-order key, monotonic per insertion. It is the paging cursor for
    /// [`Store::chunks_missing_embeddings`], not a stable chunk identity —
    /// `chunk.id` is that.
    pub rowid: i64,
    pub chunk: Chunk,
}

/// One vector to store. `vector` is normalized by the store on write.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEmbedding {
    pub project: ProjectId,
    pub chunk_id: ChunkId,
    pub vector: Vec<f32>,
}

/// What [`Store::find_chunk_by_prefix`] made of a chunk id or id prefix.
///
/// Ambiguity is its own answer rather than a `None` or an arbitrary first row:
/// the caller has to be able to tell "no such chunk" (re-run search) from
/// "several such chunks" (type more characters), and only one of those is the
/// caller's fault.
#[derive(Debug, Clone, PartialEq)]
pub enum ChunkLookup {
    /// Exactly one chunk in this project starts with the prefix.
    Found(Box<Chunk>),
    /// None does.
    Unknown,
    /// Several do; full ids, capped by the caller's `max_candidates`.
    Ambiguous(Vec<String>),
}

/// Exclusive upper bound of the range of strings starting with `prefix`:
/// `prefix` with its last byte incremented.
///
/// `None` for an empty prefix, and for the (unreachable for hex input)
/// all-`0xFF` case where no such bound exists.
fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    while let Some(last) = bytes.pop() {
        if last < u8::MAX {
            bytes.push(last + 1);
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

/// Identity of the embedding space every stored vector belongs to.
///
/// Stored opaquely (serialized whole). A mismatch means the vectors in the
/// database are not comparable to newly produced ones; resolving that is the
/// **caller's** job ([`Store::clear_all_embeddings`] then re-embed). The store
/// never silently mixes vector spaces (3.1, `model_id_tag` pattern).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    pub model_id: String,
    /// Vector width this space produces: the configured `dimensions` when the
    /// config declares one, otherwise the width the endpoint was first
    /// observed to answer with. `0` only in a record written before the
    /// observed width was folded in — see [`Store::embedding_widths`], which
    /// is how such a record is adopted rather than re-embedded.
    pub dimensions: u32,
    pub query_prefix: String,
    pub document_prefix: String,
    /// Free-form normalization tag, e.g. "l2".
    pub normalization: String,
    /// Per-input byte budget the clip was applied at. Defaults to the
    /// historical 8 KiB when absent, so a fingerprint stored before this
    /// field existed compares equal under an unchanged config instead of
    /// forcing a pointless full re-embed.
    #[serde(default = "default_max_embed_bytes")]
    pub max_embed_bytes: u32,
}

fn default_max_embed_bytes() -> u32 {
    8 * 1024
}

/// Per-project index counts plus the store-wide generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub generation: u64,
    pub projects: Vec<ProjectStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStatus {
    pub project: ProjectId,
    pub root: Utf8PathBuf,
    pub name: String,
    pub key: String,
    pub kind: SourceKind,
    pub files: u64,
    pub chunks: u64,
    pub embedded_chunks: u64,
    /// Files whose authority declaration does not hold up — today, files
    /// declaring `decided` without citing an active decision. Reported for the
    /// same reason watcher and embedding degradation are: a silent demotion is
    /// a document the author believes is canon and Lore quietly does not.
    pub authority_violations: u64,
    /// The first [`MAX_VIOLATION_PATHS`] offending paths, so the report is
    /// actionable without being a file dump.
    pub authority_violation_paths: Vec<Utf8PathBuf>,
    /// The repo's `.lore.toml` verdict (D-0012), including a config error to
    /// shout about.
    pub authority: RepoAuthority,
    /// Decisions that are accepted and unretired.
    pub decisions_active: u64,
    /// Decisions that exist at all, whatever their status. Excluded records
    /// are not counted here; they are in [`Self::decision_violations`].
    pub decisions_total: u64,
    /// Identity defects in the decision corpus itself (D-0013): a record whose
    /// heading and filename disagree, or a duplicated id.
    pub decision_violations: Vec<DecisionViolation>,
}

/// The SQLite-backed SearchStore.
pub struct Store {
    conn: Connection,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Open (creating if absent) and migrate to the latest schema.
    ///
    /// Idempotent: reopening an up-to-date database applies no migrations.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(db_path)?;

        conn.busy_timeout(BUSY_TIMEOUT)?;
        // WAL: readers (CLI/status) never block the daemon's writer.
        // journal_mode returns a row, so it cannot go through execute_batch.
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        // FK enforcement must be off while migrating (rusqlite_migration's
        // documented requirement); it is turned on immediately after.
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = OFF;")?;

        schema::migrations().to_latest(&mut conn)?;

        // recursive_triggers so that rows removed by an ON DELETE CASCADE
        // still fire the FTS sync triggers. Every code path here also deletes
        // chunks explicitly, so this is belt-and-braces against a future
        // cascade (e.g. project deletion) silently orphaning FTS rows.
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA recursive_triggers = ON;")?;

        Ok(Self { conn })
    }

    // ---- projects ---------------------------------------------------------

    /// Register a repo root, or update the display name of an existing one.
    /// Idempotent on `root`; the caller is responsible for handing in an
    /// already-canonicalized path.
    ///
    /// A new row gets a freshly allocated [`Project::key`], so the table is
    /// never left in a keyless state even when nothing consulted the manifest
    /// (unit tests, a direct store user).
    pub fn register_project(&mut self, root: &Utf8Path, name: &str) -> Result<ProjectId> {
        self.upsert_project(root, name, None, SourceKind::Repo)
    }

    /// Register or update a source with full control over its identity.
    ///
    /// `key: None` keeps the existing key (a rename must not change it) and
    /// allocates one only when the row is new or predates keys. Passing a key
    /// explicitly is how [`crate::registry`] makes the manifest authoritative.
    pub fn upsert_project(
        &mut self,
        root: &Utf8Path,
        name: &str,
        key: Option<&str>,
        kind: SourceKind,
    ) -> Result<ProjectId> {
        let tx = self.conn.transaction()?;
        let existing: Option<(ProjectId, Option<String>)> = tx
            .query_row(
                "SELECT id, key FROM projects WHERE root = ?",
                params![root.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let key = match key {
            Some(key) => key.to_string(),
            None => match existing.as_ref().and_then(|(_, key)| key.clone()) {
                Some(key) => key,
                None => {
                    let mut taken: HashSet<String> = HashSet::new();
                    let mut stmt = tx.prepare("SELECT key FROM projects WHERE key IS NOT NULL")?;
                    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                    for row in rows {
                        taken.insert(row?);
                    }
                    crate::registry::allocate_key(name, &taken)
                }
            },
        };

        let id = match existing {
            Some((id, _)) => {
                tx.execute(
                    "UPDATE projects SET name = ?, key = ?, kind = ? WHERE id = ?",
                    params![name, key, kind.as_str(), id],
                )?;
                id
            }
            None => tx.query_row(
                "INSERT INTO projects (root, name, key, kind) VALUES (?, ?, ?, ?) RETURNING id",
                params![root.as_str(), name, key, kind.as_str()],
                |r| r.get(0),
            )?,
        };
        tx.commit()?;
        Ok(id)
    }

    /// Forget a source entirely: its chunks, FTS rows, vectors, file records
    /// and parsed decision set go with it. Returns whether it was known.
    ///
    /// One project on its own; [`Self::apply_project_set`] does the same
    /// deletion as part of a larger reconciliation.
    pub fn remove_project(&mut self, project: ProjectId) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let removed = forget_project(&tx, project)?;
        tx.commit()?;
        Ok(removed)
    }

    /// Apply a whole registry reconciliation as **one** transaction: every
    /// `desired` row is written and every `remove` row is forgotten, or nothing
    /// is.
    ///
    /// # Why this is not a loop over [`Self::upsert_project`]
    ///
    /// `projects.key` is unique, and the manifest is a hand-edited file, so a
    /// user is free to hand in a state that is perfectly legal *as a whole* but
    /// unreachable one row at a time — most obviously two entries that
    /// **exchange** keys. Row-at-a-time upserts hit `UNIQUE constraint failed:
    /// projects.key` partway through, and because each one committed
    /// separately, the registry was left half-applied and failed identically on
    /// every subsequent start instead of converging.
    ///
    /// So the apply is ordered so a legal end state cannot transit through an
    /// illegal one:
    ///
    /// 1. **removals first** — a key or root being handed to a surviving
    ///    project may still be held by a project this edit deletes;
    /// 2. **release** — every surviving row named by `desired` has its key set
    ///    to `NULL`, which the unique index permits any number of times (SQLite
    ///    treats NULLs as distinct), so no old key can collide with a new one;
    /// 3. **claim** — only then are the final identities written.
    ///
    /// The caller supplies `id` per spec and the `remove` list because matching
    /// a manifest root to an existing row is the *registry's* comparison to
    /// make (it is case-insensitive on Windows); the store matches nothing and
    /// guesses nothing.
    ///
    /// Uniqueness *within* `desired` is likewise the caller's to guarantee —
    /// [`crate::registry`] repairs a manifest that violates it before calling.
    /// A `desired` set that still collides with itself fails, and the
    /// transaction rolls the whole reconciliation back rather than leaving a
    /// partial one behind.
    pub fn apply_project_set(
        &mut self,
        desired: &[ProjectSpec],
        remove: &[ProjectId],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        for &project in remove {
            forget_project(&tx, project)?;
        }

        {
            let mut release = tx.prepare("UPDATE projects SET key = NULL WHERE id = ?")?;
            for spec in desired {
                if let Some(id) = spec.id {
                    release.execute(params![id])?;
                }
            }
        }

        {
            let mut update = tx.prepare(
                "UPDATE projects SET root = ?, name = ?, key = ?, kind = ? WHERE id = ?",
            )?;
            let mut insert =
                tx.prepare("INSERT INTO projects (root, name, key, kind) VALUES (?, ?, ?, ?)")?;
            for spec in desired {
                match spec.id {
                    Some(id) => {
                        update.execute(params![
                            spec.root.as_str(),
                            spec.name,
                            spec.key,
                            spec.kind.as_str(),
                            id
                        ])?;
                    }
                    None => {
                        insert.execute(params![
                            spec.root.as_str(),
                            spec.name,
                            spec.key,
                            spec.kind.as_str()
                        ])?;
                    }
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, root, name, key, kind,
                    authority_profile, authority_behavior, authority_error
             FROM projects ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Project {
                id: r.get(0)?,
                root: Utf8PathBuf::from(r.get::<_, String>(1)?),
                name: r.get(2)?,
                key: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                kind: read_kind(r.get::<_, Option<String>>(4)?.as_deref()),
                authority: read_authority((r.get(5)?, r.get(6)?, r.get(7)?)),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- authority --------------------------------------------------------

    /// Record the repo's resolved `.lore.toml` (D-0012). Returns whether it
    /// changed — which is the **profile fingerprint** the index pass compares
    /// on every refresh, and therefore the signal that every effective tier in
    /// the project may now be wrong.
    ///
    /// The error string is part of the comparison on purpose: a `.lore.toml`
    /// edited from one broken state to a differently broken one has still
    /// changed what the user needs to be told.
    pub fn set_project_authority(
        &mut self,
        project: ProjectId,
        authority: &RepoAuthority,
    ) -> Result<bool> {
        let current: Option<(Option<String>, Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT authority_profile, authority_behavior, authority_error
                 FROM projects WHERE id = ?",
                params![project],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((profile, behavior, error)) = current else {
            return Ok(false);
        };
        if &read_authority((profile, behavior, error)) == authority {
            return Ok(false);
        }
        self.conn.execute(
            "UPDATE projects
                SET authority_profile = ?, authority_behavior = ?, authority_error = ?
              WHERE id = ?",
            params![
                authority.profile.map(Profile::as_str),
                authority.profile.map(|_| authority.behavior.as_str()),
                authority.error,
                project,
            ],
        )?;
        Ok(true)
    }

    /// The stored `.lore.toml` verdict for one project.
    pub fn project_authority(&self, project: ProjectId) -> Result<RepoAuthority> {
        let row: Option<(Option<String>, Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT authority_profile, authority_behavior, authority_error
                 FROM projects WHERE id = ?",
                params![project],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            Some(stored) => read_authority(stored),
            None => RepoAuthority::default(),
        })
    }

    /// The project's active decision set, as last parsed from its ledger.
    pub fn active_decisions(&self, project: ProjectId) -> Result<BTreeSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT decision_id FROM project_decisions WHERE project_id = ?")?;
        let rows = stmt.query_map(params![project], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Replace the project's active decision set. Returns whether it changed —
    /// which is exactly the signal that every effective tier in the project
    /// may now be wrong and a recompute is owed.
    ///
    /// Convenience over [`Self::set_decisions`] for callers that only have the
    /// active set: the reported total follows it and no corpus violations are
    /// recorded.
    pub fn set_active_decisions(
        &mut self,
        project: ProjectId,
        decisions: &BTreeSet<String>,
    ) -> Result<bool> {
        self.set_decisions(
            project,
            &Decisions {
                active: decisions.clone(),
                total: decisions.len(),
                violations: Vec::new(),
            },
        )
    }

    /// Replace the project's whole decision corpus summary: the active set,
    /// how many decisions exist at all, and the identity defects found while
    /// parsing them (D-0013).
    ///
    /// Returns whether the **active set** changed. The total and the violation
    /// list are reporting; only the active set can move an effective tier, so
    /// only it triggers a recompute.
    pub fn set_decisions(&mut self, project: ProjectId, decisions: &Decisions) -> Result<bool> {
        let current = self.active_decisions(project)?;
        let changed = current != decisions.active;
        let tx = self.conn.transaction()?;
        if changed {
            tx.execute(
                "DELETE FROM project_decisions WHERE project_id = ?",
                params![project],
            )?;
            let mut ins = tx
                .prepare("INSERT INTO project_decisions (project_id, decision_id) VALUES (?, ?)")?;
            for decision in &decisions.active {
                ins.execute(params![project, decision])?;
            }
        }
        let violations = (!decisions.violations.is_empty())
            .then(|| serde_json::to_string(&decisions.violations))
            .transpose()?;
        tx.execute(
            "UPDATE projects SET decisions_total = ?, decision_violations = ? WHERE id = ?",
            params![decisions.total as i64, violations, project],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    /// Decision-corpus defects recorded by the last refresh.
    pub fn decision_violations(&self, project: ProjectId) -> Result<Vec<DecisionViolation>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT decision_violations FROM projects WHERE id = ?",
                params![project],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        match raw {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Vec::new()),
        }
    }

    /// Re-derive every chunk's effective tier from persisted state.
    ///
    /// This is the whole payoff of computing authority *from* stored columns:
    /// a ledger edit, or a policy change, costs one pass over the metadata —
    /// no re-read, no re-chunk, no re-embed, and every [`ChunkId`] and the
    /// `CHUNK_FORMAT_VERSION` stay exactly where they were. The FTS sync
    /// trigger only fires on `text`/`path`/`anchor`, so the index does not
    /// churn either.
    ///
    /// Work is per *file*, not per chunk: declared status, frontmatter refs
    /// and path are file-level, so one verdict is stamped on the whole file.
    /// Rows whose verdict is unchanged are not written at all, which makes a
    /// steady-state startup backfill a read-only scan.
    pub fn recompute_effective_authority(&mut self, project: ProjectId) -> Result<Recompute> {
        let context = self.authority_context(project)?;
        let paths: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT path FROM chunks WHERE project_id = ?")?;
            let rows = stmt.query_map(params![project], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };

        let mut summary = Recompute::default();
        let tx = self.conn.transaction()?;
        {
            let mut declared =
                tx.prepare("SELECT vault FROM chunks WHERE project_id = ? AND path = ? LIMIT 1")?;
            let mut update = tx.prepare(
                "UPDATE chunks SET effective_tier = ?, demotion = ?
                 WHERE project_id = ? AND path = ?
                   AND (effective_tier <> ? OR demotion IS NOT ?)",
            )?;
            for path in &paths {
                summary.files += 1;
                let row: Option<Option<String>> = declared
                    .query_row(params![project, path], |r| r.get::<_, Option<String>>(0))
                    .optional()?;
                let Some(vault_json) = row else { continue };
                let vault: Option<VaultMeta> = vault_json
                    .map(|json| serde_json::from_str::<VaultMeta>(&json))
                    .transpose()?;
                let authority = context.verdict(Utf8Path::new(path), vault.as_ref());
                let changed = update.execute(params![
                    authority.tier,
                    authority.demotion.map(Demotion::code),
                    project,
                    path,
                    authority.tier,
                    authority.demotion.map(Demotion::code),
                ])?;
                if changed > 0 {
                    summary.files_changed += 1;
                    summary.chunks_changed += changed;
                }
                if authority.is_violation() {
                    summary.violations += 1;
                }
            }
        }
        tx.commit()?;
        Ok(summary)
    }

    /// Everything the effective-tier policy needs that is not on the chunk
    /// row: the project's active decisions and its source kind.
    fn authority_context(&self, project: ProjectId) -> Result<AuthorityContext> {
        let row: Option<(Option<String>, StoredAuthority)> = self
            .conn
            .query_row(
                "SELECT kind, authority_profile, authority_behavior, authority_error
                 FROM projects WHERE id = ?",
                params![project],
                |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))),
            )
            .optional()?;
        let (kind, stored) = row.unwrap_or_default();
        Ok(AuthorityContext {
            active: self.active_decisions(project)?,
            kind: read_kind(kind.as_deref()),
            profile: read_authority(stored).active(),
        })
    }

    // ---- files ------------------------------------------------------------

    /// Content hash recorded at the last successful index of `path`, if any.
    pub fn file_hash(&self, project: ProjectId, path: &Utf8Path) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT content_hash FROM files WHERE project_id = ? AND path = ?",
                params![project, path.as_str()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Every indexed file in a project — for pruning files that vanished from
    /// disk between index passes.
    pub fn list_files(&self, project: ProjectId) -> Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, content_hash, indexed_at, source_ts FROM files
             WHERE project_id = ? ORDER BY path",
        )?;
        let rows = stmt.query_map(params![project], |r| {
            Ok(FileRecord {
                path: Utf8PathBuf::from(r.get::<_, String>(0)?),
                content_hash: r.get(1)?,
                indexed_at: r.get(2)?,
                source_ts: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Record the source-declared timestamp of a file (a session's write
    /// time). Separate from [`Self::replace_file_chunks`] because the source
    /// knows it and the chunker never will; unused in v1, where every file
    /// comes from a repo and `indexed_at` is the only honest timestamp.
    /// Returns whether the file was known.
    pub fn set_file_source_ts(
        &mut self,
        project: ProjectId,
        path: &Utf8Path,
        source_ts: Option<i64>,
    ) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE files SET source_ts = ? WHERE project_id = ? AND path = ?",
            params![source_ts, project, path.as_str()],
        )?;
        Ok(updated > 0)
    }

    /// Replace a file's chunk set in one transaction.
    ///
    /// Chunks whose content-addressed id is unchanged are **updated in place**
    /// (same rowid), so their embedding rows and FTS postings survive; only
    /// ids that disappeared are deleted, taking their vectors with them.
    ///
    /// The upsert deliberately does not rewrite `path`, `anchor` or `text`:
    /// those three are the inputs to [`ChunkId::derive`], so an id collision
    /// proves they are identical. Not touching them also means the FTS sync
    /// trigger (`AFTER UPDATE OF text, path, anchor`) does not fire for
    /// unchanged chunks — no index churn on re-index of a stable file.
    pub fn replace_file_chunks(
        &mut self,
        project: ProjectId,
        path: &Utf8Path,
        content_hash: &str,
        chunks: &[Chunk],
    ) -> Result<FileWrite> {
        for c in chunks {
            if c.path != path {
                return Err(StoreError::PathMismatch {
                    chunk_id: c.id.0.clone(),
                    chunk_path: c.path.to_string(),
                    file_path: path.to_string(),
                });
            }
        }

        // One verdict for the whole file: declared status, frontmatter refs
        // and path are all file-level, and every chunk carries a clone of the
        // same `VaultMeta` frontmatter (only `body_decision_refs` differ, and
        // those deliberately carry no weight any more).
        let context = self.authority_context(project)?;
        let authority = context.verdict(path, chunks.first().and_then(|c| c.vault.as_ref()));

        let tx = self.conn.transaction()?;

        tx.execute(
            "INSERT INTO files (project_id, path, content_hash, indexed_at)
             VALUES (?, ?, ?, unixepoch())
             ON CONFLICT(project_id, path) DO UPDATE
               SET content_hash = excluded.content_hash, indexed_at = excluded.indexed_at",
            params![project, path.as_str(), content_hash],
        )?;

        let existing: HashSet<String> = {
            let mut stmt =
                tx.prepare("SELECT chunk_id FROM chunks WHERE project_id = ? AND path = ?")?;
            let rows =
                stmt.query_map(params![project, path.as_str()], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let incoming: HashSet<&str> = chunks.iter().map(|c| c.id.0.as_str()).collect();

        let mut stats = FileWrite::default();
        {
            let mut del = tx.prepare("DELETE FROM chunks WHERE project_id = ? AND chunk_id = ?")?;
            for stale in existing.iter().filter(|id| !incoming.contains(id.as_str())) {
                del.execute(params![project, stale])?;
                stats.deleted += 1;
            }
        }

        {
            let mut ins = tx.prepare(
                "INSERT INTO chunks (
                     project_id, chunk_id, path, anchor, kind, language,
                     byte_start, byte_end, line_start, line_end, text, vault,
                     design_status, authority_tier, effective_tier, demotion)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(project_id, chunk_id) DO UPDATE SET
                     kind = excluded.kind,
                     language = excluded.language,
                     byte_start = excluded.byte_start,
                     byte_end = excluded.byte_end,
                     line_start = excluded.line_start,
                     line_end = excluded.line_end,
                     vault = excluded.vault,
                     design_status = excluded.design_status,
                     authority_tier = excluded.authority_tier,
                     effective_tier = excluded.effective_tier,
                     demotion = excluded.demotion",
            )?;
            for c in chunks {
                let status = c.vault.as_ref().and_then(|v| v.design_status);
                ins.execute(params![
                    project,
                    c.id.0,
                    c.path.as_str(),
                    c.kind.anchor(),
                    serde_json::to_string(&c.kind)?,
                    c.language,
                    c.byte_start,
                    c.byte_end,
                    c.line_start,
                    c.line_end,
                    c.text,
                    c.vault.as_ref().map(serde_json::to_string).transpose()?,
                    status.map(status_str),
                    authority_tier(status),
                    authority.tier,
                    authority.demotion.map(Demotion::code),
                ])?;
                if existing.contains(c.id.0.as_str()) {
                    stats.kept += 1;
                } else {
                    stats.inserted += 1;
                }
            }
        }

        tx.commit()?;
        stats.authority = authority;
        Ok(stats)
    }

    /// Forget a file entirely: chunks, FTS rows and embeddings go with it.
    /// Returns whether the file was known.
    /// `Ok(None)` when the file was not indexed here; `Ok(Some(n))` when it
    /// was, carrying the chunks that went with it.
    ///
    /// The count is returned rather than discarded because a pass that removes
    /// files is deleting chunks, and a summary reporting `removed = 19,
    /// chunks_deleted = 0` describes a different pass than the one that ran.
    pub fn remove_file(&mut self, project: ProjectId, path: &Utf8Path) -> Result<Option<usize>> {
        let tx = self.conn.transaction()?;
        // Explicit chunk delete (rather than relying on the files FK cascade)
        // so the FTS triggers fire on a plain DELETE statement.
        let chunks = tx.execute(
            "DELETE FROM chunks WHERE project_id = ? AND path = ?",
            params![project, path.as_str()],
        )?;
        let removed = tx.execute(
            "DELETE FROM files WHERE project_id = ? AND path = ?",
            params![project, path.as_str()],
        )?;
        tx.commit()?;
        Ok((removed > 0).then_some(chunks))
    }

    // ---- generation / status ---------------------------------------------

    /// Increment and return the index generation. Call once at the end of an
    /// index pass; surfaced by `status` so clients can tell whether the index
    /// moved under them.
    pub fn bump_generation(&mut self) -> Result<u64> {
        let g: i64 = self.conn.query_row(
            "UPDATE meta SET generation = generation + 1 WHERE id = 1 RETURNING generation",
            [],
            |r| r.get(0),
        )?;
        Ok(g as u64)
    }

    pub fn generation(&self) -> Result<u64> {
        let g: i64 = self
            .conn
            .query_row("SELECT generation FROM meta WHERE id = 1", [], |r| r.get(0))?;
        Ok(g as u64)
    }

    pub fn status(&self) -> Result<Status> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.root, p.name, p.key, p.kind,
                    (SELECT COUNT(*) FROM files f WHERE f.project_id = p.id),
                    (SELECT COUNT(*) FROM chunks c WHERE c.project_id = p.id),
                    (SELECT COUNT(*) FROM embeddings e WHERE e.project_id = p.id),
                    (SELECT COUNT(DISTINCT c.path) FROM chunks c
                       WHERE c.project_id = p.id AND c.demotion = ?),
                    (SELECT COUNT(*) FROM project_decisions d WHERE d.project_id = p.id),
                    p.decisions_total, p.decision_violations,
                    p.authority_profile, p.authority_behavior, p.authority_error
             FROM projects p ORDER BY p.id",
        )?;
        let violation = Demotion::UncitedDecided.code();
        let rows = stmt.query_map(params![violation], |r| {
            Ok(ProjectStatus {
                project: r.get(0)?,
                root: Utf8PathBuf::from(r.get::<_, String>(1)?),
                name: r.get(2)?,
                key: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                kind: read_kind(r.get::<_, Option<String>>(4)?.as_deref()),
                files: r.get::<_, i64>(5)? as u64,
                chunks: r.get::<_, i64>(6)? as u64,
                embedded_chunks: r.get::<_, i64>(7)? as u64,
                authority_violations: r.get::<_, i64>(8)? as u64,
                authority_violation_paths: Vec::new(),
                decisions_active: r.get::<_, i64>(9)? as u64,
                decisions_total: r.get::<_, i64>(10)? as u64,
                // A corrupt blob degrades to "no violations reported" rather
                // than failing `status`; the counts around it are still true.
                decision_violations: r
                    .get::<_, Option<String>>(11)?
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or_default(),
                authority: read_authority((r.get(12)?, r.get(13)?, r.get(14)?)),
            })
        })?;
        let mut projects: Vec<ProjectStatus> = rows.collect::<std::result::Result<_, _>>()?;

        // Paths are a second, cheap query rather than a correlated subquery:
        // only projects that actually have violations pay for it, and the
        // partial `chunks_demoted` index answers it directly.
        let mut offenders = self.conn.prepare(
            "SELECT DISTINCT path FROM chunks
             WHERE project_id = ? AND demotion = ? ORDER BY path LIMIT ?",
        )?;
        for project in &mut projects {
            if project.authority_violations == 0 {
                continue;
            }
            let rows = offenders.query_map(
                params![project.project, violation, MAX_VIOLATION_PATHS as i64],
                |r| r.get::<_, String>(0),
            )?;
            project.authority_violation_paths = rows
                .map(|path| path.map(Utf8PathBuf::from))
                .collect::<std::result::Result<_, _>>()?;
        }

        Ok(Status {
            generation: self.generation()?,
            projects,
        })
    }

    // ---- retrieval --------------------------------------------------------

    /// BM25 lexical search over chunk text, path and symbol/heading anchor.
    ///
    /// `query` is arbitrary user text: it is sanitized into a safe FTS5 MATCH
    /// expression (see [`query::sanitize_fts_terms`]), so malformed input
    /// returns `Ok` with sensible-or-empty results rather than an error or a
    /// panic. Returned scores are `-bm25(...)`, i.e. higher is better.
    ///
    /// Multi-term queries are tried as a conjunction first and retried as a
    /// disjunction only when the conjunction matches nothing. Precise queries
    /// therefore keep their exact behaviour and cost one statement; prose
    /// questions that would otherwise return nothing (see
    /// [`query::or_fts_query`]) pay a second statement to get a ranked list
    /// instead of an empty one. Scores are not comparable between the two
    /// passes, but nothing needs them to be: the caller fuses by rank.
    pub fn lexical_search(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let terms = sanitize_fts_terms(query);
        let match_expr = terms.join(" ");
        if match_expr.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let hits = self.lexical_match(&match_expr, filter, limit)?;
        if !hits.is_empty() {
            return Ok(hits);
        }
        let relaxed = or_fts_query(&terms);
        if relaxed.is_empty() {
            return Ok(hits);
        }
        self.lexical_match(&relaxed, filter, limit)
    }

    /// Run one already-built MATCH expression. Split out of
    /// [`Self::lexical_search`] so the conjunction and its disjunctive retry
    /// cannot drift apart in filters, ordering, or row decoding.
    fn lexical_match(
        &self,
        match_expr: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let f = filter_sql(filter);
        let sql = format!(
            "SELECT {CHUNK_COLS}, -bm25(chunks_fts, {BM25_WEIGHTS}) AS score
             FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid
             WHERE chunks_fts MATCH ?{}
             ORDER BY score DESC, c.id ASC
             LIMIT ?",
            f.sql
        );

        let mut params: Vec<Value> = vec![Value::Text(match_expr.to_string())];
        params.extend(f.params);
        params.push(Value::Integer(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(params))?;
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            hits.push(SearchHit {
                project: row.get(0)?,
                chunk: row_to_chunk(row)?,
                authority: row_to_authority(row)?,
                score: row.get::<_, f64>(CHUNK_COL_COUNT)? as f32,
            });
        }
        Ok(hits)
    }

    /// Brute-force cosine search over stored vectors.
    ///
    /// Filters are applied in SQL so unscanned rows are never decoded; rows
    /// stream through a bounded top-k heap (O(n log k) time, O(k) memory), and
    /// only the surviving k rows are hydrated into full chunks. At the ~1e5
    /// chunk corpora this design targets that is a few milliseconds of pure
    /// arithmetic — an ANN index is the Tantivy/arroy upgrade's problem.
    ///
    /// Both sides are L2-normalized, so the dot product *is* cosine.
    pub fn vector_search(
        &self,
        query_vec: &[f32],
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let q = vector::normalize(query_vec).ok_or(StoreError::InvalidQueryVector)?;

        let f = filter_sql(filter);
        let sql = format!(
            "SELECT c.id, e.vector
             FROM embeddings e JOIN chunks c ON c.id = e.chunk_rowid
             WHERE 1{}",
            f.sql
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(f.params))?;
        let mut top = TopK::new(limit);
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let blob = row.get_ref(1)?.as_blob().map_err(|_| {
                StoreError::Corrupt(format!("embedding for chunk rowid {rowid} is not a blob"))
            })?;
            let score = vector::dot_blob(&q, blob).ok_or(StoreError::DimensionMismatch {
                query: q.len(),
                stored: blob.len() / 4,
            })?;
            top.push(Scored { score, rowid });
        }

        let mut hits = Vec::new();
        let mut fetch = self
            .conn
            .prepare(&format!("SELECT {CHUNK_COLS} FROM chunks c WHERE c.id = ?"))?;
        for Scored { score, rowid } in top.into_sorted() {
            let mut got = fetch.query(params![rowid])?;
            if let Some(row) = got.next()? {
                hits.push(SearchHit {
                    project: row.get(0)?,
                    chunk: row_to_chunk(row)?,
                    authority: row_to_authority(row)?,
                    score,
                });
            }
        }
        Ok(hits)
    }

    pub fn get_chunk(&self, project: ProjectId, chunk_id: &ChunkId) -> Result<Option<Chunk>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CHUNK_COLS} FROM chunks c WHERE c.project_id = ? AND c.chunk_id = ?"
        ))?;
        let mut rows = stmt.query(params![project, chunk_id.0])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_chunk(row)?)),
            None => Ok(None),
        }
    }

    /// Resolve a chunk id **or a prefix of one** within a project.
    ///
    /// A half-open range scan (`chunk_id >= prefix AND chunk_id < prefix⁺`)
    /// rather than `LIKE`/`GLOB`, so the query is a bounded seek on the
    /// existing `UNIQUE (project_id, chunk_id)` index whatever SQLite's
    /// pattern-optimizer feels like doing that day. A full-length id is the
    /// same scan over a range that can hold exactly one row.
    ///
    /// `max_candidates` caps how many ids an ambiguous prefix reports; the
    /// caller phrases the answer as "several", not "exactly these", because a
    /// full listing is not what the caller needs to type a longer prefix.
    pub fn find_chunk_by_prefix(
        &self,
        project: ProjectId,
        prefix: &str,
        max_candidates: usize,
    ) -> Result<ChunkLookup> {
        let Some(upper) = prefix_upper_bound(prefix) else {
            return Ok(ChunkLookup::Unknown);
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CHUNK_COLS}, c.chunk_id FROM chunks c
             WHERE c.project_id = ? AND c.chunk_id >= ? AND c.chunk_id < ?
             ORDER BY c.chunk_id
             LIMIT ?"
        ))?;
        // One more than the cap is fetched only to know that the first row is
        // not alone; the extra row is never reported.
        let limit = max_candidates.max(2) as i64;
        let mut rows = stmt.query(params![project, prefix, upper, limit])?;

        let Some(row) = rows.next()? else {
            return Ok(ChunkLookup::Unknown);
        };
        let first = row_to_chunk(row)?;
        let mut ids = vec![first.id.0.clone()];
        while let Some(row) = rows.next()? {
            ids.push(row.get(CHUNK_COL_COUNT)?);
        }
        if ids.len() == 1 {
            Ok(ChunkLookup::Found(Box::new(first)))
        } else {
            ids.truncate(max_candidates);
            Ok(ChunkLookup::Ambiguous(ids))
        }
    }

    /// Every chunk of a file in document order — the backing query for the
    /// `expand` tool.
    pub fn get_file_chunks(&self, project: ProjectId, path: &Utf8Path) -> Result<Vec<Chunk>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CHUNK_COLS} FROM chunks c
             WHERE c.project_id = ? AND c.path = ?
             ORDER BY c.byte_start, c.id"
        ))?;
        let mut rows = stmt.query(params![project, path.as_str()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_chunk(row)?);
        }
        Ok(out)
    }

    // ---- embeddings -------------------------------------------------------

    /// One page of chunks with no vector, oldest rowid first (so a large
    /// backlog drains in insertion order and repeated calls make progress).
    ///
    /// `after` is an exclusive [`EmbedCandidate::rowid`] cursor. The caller
    /// pages with it to walk *past* candidates it has decided to skip; without
    /// a cursor a page full of skipped rows is indistinguishable from an empty
    /// backlog, and everything behind it starves.
    ///
    /// A short page means the end of the missing set was reached.
    pub fn chunks_missing_embeddings(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<EmbedCandidate>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CHUNK_COLS}, c.id FROM chunks c
             LEFT JOIN embeddings e ON e.chunk_rowid = c.id
             WHERE e.chunk_rowid IS NULL AND c.id > ?
             ORDER BY c.id
             LIMIT ?"
        ))?;
        let mut rows = stmt.query(params![after, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(EmbedCandidate {
                project: row.get(0)?,
                rowid: row.get(CHUNK_COL_COUNT)?,
                chunk: row_to_chunk(row)?,
            });
        }
        Ok(out)
    }

    /// Store vectors, L2-normalizing each on the way in (cosine == dot
    /// thereafter). One transaction for the batch.
    ///
    /// Entries whose chunk no longer exists are **skipped, not errors**: a
    /// chunk can legitimately vanish between batching and the embedding
    /// endpoint replying. The return value is the number actually stored.
    pub fn upsert_embeddings(&mut self, items: &[NewEmbedding]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut stored = 0usize;
        {
            let mut lookup =
                tx.prepare("SELECT id FROM chunks WHERE project_id = ? AND chunk_id = ?")?;
            let mut ins = tx.prepare(
                "INSERT INTO embeddings (chunk_rowid, project_id, dims, vector)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(chunk_rowid) DO UPDATE
                   SET dims = excluded.dims, vector = excluded.vector",
            )?;
            for item in items {
                let unit =
                    vector::normalize(&item.vector).ok_or_else(|| StoreError::InvalidVector {
                        chunk_id: item.chunk_id.0.clone(),
                    })?;
                let rowid: Option<i64> = lookup
                    .query_row(params![item.project, item.chunk_id.0], |r| r.get(0))
                    .optional()?;
                let Some(rowid) = rowid else { continue };
                ins.execute(params![
                    rowid,
                    item.project,
                    unit.len() as i64,
                    vector::encode(&unit)
                ])?;
                stored += 1;
            }
        }
        tx.commit()?;
        Ok(stored)
    }

    pub fn embedding_fingerprint(&self) -> Result<Option<EmbeddingFingerprint>> {
        let raw: Option<String> = self.conn.query_row(
            "SELECT embedding_fingerprint FROM meta WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        match raw {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    pub fn set_embedding_fingerprint(&mut self, fp: &EmbeddingFingerprint) -> Result<()> {
        self.conn.execute(
            "UPDATE meta SET embedding_fingerprint = ? WHERE id = 1",
            params![serde_json::to_string(fp)?],
        )?;
        Ok(())
    }

    /// The distinct vector widths actually present, at most a handful.
    ///
    /// Empty means there are no vectors. More than one entry means the store
    /// already holds mixed spaces, which is the corruption a fingerprint
    /// exists to prevent — so a caller deciding whether an unwidthed
    /// fingerprint may simply be adopted must treat that as "no".
    ///
    /// Deliberately not indexed and deliberately a scan: it is called only
    /// when a fingerprint has *changed*, which is once per model change, not
    /// once per pass.
    pub fn embedding_widths(&self) -> Result<Vec<usize>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT dims FROM embeddings ORDER BY dims LIMIT 8")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows
            .collect::<std::result::Result<Vec<i64>, _>>()?
            .into_iter()
            .map(|dims| dims as usize)
            .collect())
    }

    /// Drop every stored vector. Leaves the fingerprint alone — the caller
    /// decides what the new embedding space is and sets it explicitly.
    /// Returns the number of vectors discarded.
    pub fn clear_all_embeddings(&mut self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM embeddings", [])?)
    }
}

/// What [`Store::recompute_effective_authority`] did. `chunks_changed` being
/// zero on a steady-state pass is the assertion that recompute is cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Recompute {
    pub files: usize,
    pub files_changed: usize,
    pub chunks_changed: usize,
    /// Files declaring `decided` without citing an active decision.
    pub violations: usize,
}

/// The per-project inputs to the effective-tier policy that do not live on a
/// chunk row. Read once per store call and reused for every file in it.
struct AuthorityContext {
    active: BTreeSet<String>,
    kind: SourceKind,
    /// The profile in force, or `None` for a neutral repo (D-0012).
    profile: Option<Profile>,
}

impl AuthorityContext {
    /// One file's verdict. `vault` is the file's frontmatter metadata (`None`
    /// for code and other non-vault files, which declare nothing and are
    /// therefore neutral unless a path or source ceiling applies).
    fn verdict(&self, path: &Utf8Path, vault: Option<&VaultMeta>) -> Authority {
        crate::authority::effective(
            path,
            Declared {
                status: vault.and_then(|v| v.design_status),
                decision_refs: vault.map_or(&[], |v| v.decision_refs.as_slice()),
            },
            &self.active,
            self.kind,
            self.profile,
        )
    }
}

/// The three `projects` columns that spell out a repo's `.lore.toml` verdict:
/// `(authority_profile, authority_behavior, authority_error)`.
type StoredAuthority = (Option<String>, Option<String>, Option<String>);

/// Rebuild a [`RepoAuthority`] from its three stored columns.
///
/// Unrecognized spellings degrade to neutral rather than failing, on the same
/// reasoning as [`read_kind`]: a database written by a newer daemon that knows
/// a profile this build does not must not make the store unopenable — and
/// neutral is the *safe* direction, since the alternative is applying a policy
/// this build does not actually implement.
fn read_authority((stored, behavior, error): StoredAuthority) -> RepoAuthority {
    let profile = stored.as_deref().and_then(Profile::parse);
    let error = match (&stored, profile) {
        // Written by a daemon that knows a profile this one does not. Neutral
        // *and* loud: silently ranking as if nothing were configured is the
        // exact failure D-0012 names.
        (Some(name), None) => Some(error.unwrap_or_else(|| {
            format!("stored authority profile `{name}` is not known to this build of lore")
        })),
        _ => error,
    };
    RepoAuthority {
        profile,
        behavior: behavior
            .as_deref()
            .and_then(Behavior::parse)
            .unwrap_or_default(),
        error,
    }
}

/// Drop a source and everything derived from it. Returns whether the row was
/// there.
///
/// Chunks are deleted explicitly rather than through the project FK cascade so
/// the FTS sync triggers fire on a plain `DELETE`, which is the same discipline
/// [`Store::remove_file`] follows.
///
/// Takes the transaction rather than opening one, so it composes into a larger
/// atomic unit ([`Store::apply_project_set`]) instead of committing on its own.
fn forget_project(tx: &rusqlite::Transaction<'_>, project: ProjectId) -> Result<bool> {
    tx.execute("DELETE FROM chunks WHERE project_id = ?", params![project])?;
    tx.execute("DELETE FROM files WHERE project_id = ?", params![project])?;
    tx.execute(
        "DELETE FROM project_decisions WHERE project_id = ?",
        params![project],
    )?;
    let removed = tx.execute("DELETE FROM projects WHERE id = ?", params![project])?;
    Ok(removed > 0)
}

/// An unrecognized `projects.kind` reads back as `repo` rather than failing.
/// The vocabulary is expected to grow (`issue`), and a daemon downgrade must
/// degrade to "treat it as an ordinary source", never to an unopenable store.
fn read_kind(kind: Option<&str>) -> SourceKind {
    kind.and_then(SourceKind::parse).unwrap_or_default()
}

/// Canonical on-disk spelling of a [`DesignStatus`]. Exhaustive match so a new
/// variant in `types` is a compile error here rather than a silent NULL.
pub(crate) fn status_str(status: DesignStatus) -> &'static str {
    match status {
        DesignStatus::Exploration => "exploration",
        DesignStatus::Leaning => "leaning",
        DesignStatus::Decided => "decided",
        DesignStatus::Deprecated => "deprecated",
    }
}

/// Map a row selecting [`CHUNK_COLS`] (from column 0) to a [`Chunk`].
fn row_to_chunk(row: &Row<'_>) -> Result<Chunk> {
    let kind_json: String = row.get(3)?;
    let kind: ChunkKind = serde_json::from_str(&kind_json)?;
    let vault: Option<VaultMeta> = row
        .get::<_, Option<String>>(10)?
        .map(|j| serde_json::from_str::<VaultMeta>(&j))
        .transpose()?;
    Ok(Chunk {
        id: ChunkId(row.get(1)?),
        path: Utf8PathBuf::from(row.get::<_, String>(2)?),
        kind,
        language: row.get(4)?,
        byte_start: row.get(5)?,
        byte_end: row.get(6)?,
        line_start: row.get(7)?,
        line_end: row.get(8)?,
        text: row.get(9)?,
        vault,
    })
}

/// Map the effective-authority tail of a [`CHUNK_COLS`] row.
///
/// An unknown demotion code is dropped rather than rejected: the tier is the
/// load-bearing value, the note is explanation, and a store written by a newer
/// daemon must not make search fail.
fn row_to_authority(row: &Row<'_>) -> Result<Authority> {
    Ok(Authority {
        tier: row.get(11)?,
        demotion: row
            .get::<_, Option<String>>(12)?
            .as_deref()
            .and_then(Demotion::from_code),
    })
}

/// White-box checks that need the connection itself — the storage invariants
/// the public API cannot observe. Behavioural coverage lives in
/// `tests/store_sqlite.rs`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::UNCITED_DECIDED_TIER;
    use crate::types::ChunkKind;

    fn chunk(path: &str, symbol: &str, text: &str) -> Chunk {
        let path = Utf8PathBuf::from(path);
        let kind = ChunkKind::Code {
            window: None,
            symbol_kind: "function".into(),
            symbol_path: symbol.into(),
        };
        Chunk {
            id: Chunk::derive_id(&path, &kind, text),
            path,
            kind,
            language: Some("rust".into()),
            byte_start: 0,
            byte_end: text.len() as u32,
            line_start: 1,
            line_end: 2,
            text: text.into(),
            vault: None,
        }
    }

    /// A fingerprint stored before `max_embed_bytes` existed must read as the
    /// historical 8 KiB, or every upgraded vault re-embeds for nothing.
    #[test]
    fn fingerprint_without_a_byte_budget_reads_as_the_historical_default() {
        let old = r#"{"model_id":"m","dimensions":768,"query_prefix":"q: ",
                      "document_prefix":"d: ","normalization":"l2"}"#;
        let fp: EmbeddingFingerprint = serde_json::from_str(old).unwrap();
        assert_eq!(fp.max_embed_bytes, 8 * 1024);
    }

    #[test]
    fn open_sets_expected_pragmas_and_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("lore.db")).unwrap();
        let journal: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        let fk: i64 = store
            .conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 3, "every shipped migration applied");
    }

    /// The external-content FTS5 table can drift from its content table if a
    /// sync trigger is wrong; `integrity-check` is the only thing that
    /// actually proves it did not. Run it after a full churn cycle
    /// (insert / update-in-place / delete / file removal).
    #[test]
    fn fts_index_stays_consistent_through_churn() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("lore.db")).unwrap();
        let proj = store
            .register_project(Utf8Path::new("C:/repos/x"), "x")
            .unwrap();
        let path = Utf8Path::new("src/lib.rs");

        let a = chunk("src/lib.rs", "a", "fn a() { alpha }");
        let b = chunk("src/lib.rs", "b", "fn b() { beta }");
        store
            .replace_file_chunks(proj, path, "h1", &[a.clone(), b])
            .unwrap();

        // `a` survives with a changed span (update-in-place), `b` is evicted,
        // `c` is new.
        let mut moved = a.clone();
        moved.byte_start = 500;
        moved.line_start = 40;
        let c = chunk("src/lib.rs", "c", "fn c() { gamma }");
        store
            .replace_file_chunks(proj, path, "h2", &[moved, c])
            .unwrap();

        let other = chunk("src/other.rs", "d", "fn d() { delta }");
        store
            .replace_file_chunks(proj, Utf8Path::new("src/other.rs"), "h", &[other])
            .unwrap();
        store
            .remove_file(proj, Utf8Path::new("src/other.rs"))
            .unwrap();

        store
            .conn
            .execute_batch("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')")
            .expect("FTS index must match its content table exactly");

        // Row counts corroborate: 2 chunks, and the FTS table agrees.
        let chunks: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        let indexed: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!((chunks, indexed), (2, 2));
    }

    /// A Markdown chunk that declares something about itself.
    fn vault_chunk(path: &str, heading: &str, text: &str, vault: VaultMeta) -> Chunk {
        let path = Utf8PathBuf::from(path);
        let kind = ChunkKind::Section {
            heading_path: vec![heading.to_string()],
            window: None,
        };
        Chunk {
            id: Chunk::derive_id(&path, &kind, text),
            path,
            kind,
            language: Some("markdown".into()),
            byte_start: 0,
            byte_end: text.len() as u32,
            line_start: 1,
            line_end: 3,
            text: text.into(),
            vault: Some(vault),
        }
    }

    /// Authority semantics are repository-opt-in (D-0012), so a store test
    /// about the effective-tier machinery has to say that the repository opted
    /// in — exactly as a committed `.lore.toml` would.
    fn opt_into_lore_v1(store: &mut Store, project: ProjectId) {
        store
            .set_project_authority(
                project,
                &RepoAuthority {
                    profile: Some(Profile::LoreV1),
                    behavior: Behavior::Rank,
                    error: None,
                },
            )
            .unwrap();
    }

    fn declares(status: DesignStatus, refs: &[&str]) -> VaultMeta {
        VaultMeta {
            design_status: Some(status),
            decision_refs: refs.iter().map(|r| (*r).to_string()).collect(),
            body_decision_refs: Vec::new(),
        }
    }

    /// Everything the FTS5 external-content index physically stores, as one
    /// comparable value. A `delete`+re-insert through the sync triggers appends
    /// new segment blocks, so this moves whenever the index churns — which is
    /// the cost `recompute_effective_authority` claims not to pay.
    fn fts_footprint(store: &Store) -> (i64, i64, i64) {
        store
            .conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(block)), 0), COALESCE(SUM(id), 0)
                 FROM chunks_fts_data",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    /// `(rowid, chunk_id, effective_tier, demotion)` for every chunk, in
    /// storage order. The rowid is the join key for embeddings *and* for the
    /// FTS index, so it is the thing that must not move.
    fn chunk_rows(store: &Store) -> Vec<(i64, String, i64, Option<String>)> {
        let mut stmt = store
            .conn
            .prepare("SELECT id, chunk_id, effective_tier, demotion FROM chunks ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    }

    fn embedding_rows(store: &Store) -> Vec<(i64, Vec<u8>)> {
        let mut stmt = store
            .conn
            .prepare("SELECT chunk_rowid, vector FROM embeddings ORDER BY chunk_rowid")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)));
        rows.unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    /// The claim that makes effective authority affordable: a ledger edit is a
    /// metadata pass. Not "cheap" in a hand-wavy sense — it must leave chunk
    /// rowids, content-addressed ids, stored vectors *and* the FTS index
    /// physically untouched, because those are what a re-index would destroy.
    ///
    /// Tested white-box on purpose: from outside the store, a recompute that
    /// deleted and re-inserted every row would look identical to one that
    /// updated them in place, right up until the embeddings were gone.
    #[test]
    fn a_recompute_rewrites_tiers_without_disturbing_rowids_vectors_or_the_fts_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("lore.db")).unwrap();
        let proj = store
            .register_project(Utf8Path::new("C:/repos/x"), "x")
            .unwrap();
        opt_into_lore_v1(&mut store, proj);

        let path = Utf8Path::new("design/1.1.md");
        let chunks = [
            vault_chunk(
                "design/1.1.md",
                "Overview",
                "Alpha section about ranking.",
                declares(DesignStatus::Decided, &["D-0001"]),
            ),
            vault_chunk(
                "design/1.1.md",
                "Detail",
                "Beta section about ranking.",
                declares(DesignStatus::Decided, &["D-0001"]),
            ),
        ];
        store
            .replace_file_chunks(proj, path, "h1", &chunks)
            .unwrap();

        // No ledger has been parsed yet, so the declaration is refused.
        let before_rows = chunk_rows(&store);
        assert_eq!(before_rows.len(), 2);
        assert!(before_rows.iter().all(|(_, _, tier, code)| *tier
            == i64::from(UNCITED_DECIDED_TIER)
            && code.as_deref() == Some("uncited_decided")));

        let embeddings: Vec<NewEmbedding> = chunks
            .iter()
            .enumerate()
            .map(|(n, chunk)| NewEmbedding {
                project: proj,
                chunk_id: chunk.id.clone(),
                vector: vec![0.25 + n as f32, 0.5, 0.75, 1.0],
            })
            .collect();
        assert_eq!(store.upsert_embeddings(&embeddings).unwrap(), 2);

        let before_embeddings = embedding_rows(&store);
        let before_fts = fts_footprint(&store);

        // The ledger now says D-0001 is active. Every tier in the project is
        // wrong and has to be re-derived.
        let active: BTreeSet<String> = ["D-0001".to_string()].into_iter().collect();
        assert!(store.set_active_decisions(proj, &active).unwrap());
        let summary = store.recompute_effective_authority(proj).unwrap();
        assert_eq!(
            (summary.files, summary.files_changed, summary.violations),
            (1, 1, 0),
            "{summary:?}"
        );
        assert_eq!(summary.chunks_changed, 2, "both chunks of the file");

        let after_rows = chunk_rows(&store);
        assert!(
            after_rows
                .iter()
                .all(|(_, _, tier, code)| *tier == 3 && code.is_none()),
            "{after_rows:?}"
        );
        assert_eq!(
            before_rows
                .iter()
                .map(|(rowid, id, _, _)| (*rowid, id.clone()))
                .collect::<Vec<_>>(),
            after_rows
                .iter()
                .map(|(rowid, id, _, _)| (*rowid, id.clone()))
                .collect::<Vec<_>>(),
            "rowids and content-addressed ids are the identity embeddings hang off"
        );
        assert_eq!(
            embedding_rows(&store),
            before_embeddings,
            "a metadata pass must not touch a single stored vector"
        );
        assert_eq!(
            fts_footprint(&store),
            before_fts,
            "the AFTER UPDATE trigger is scoped to text/path/anchor; a tier \
             rewrite must not churn the index"
        );
        store
            .conn
            .execute_batch("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')")
            .expect("FTS index still matches its content table");

        // A steady-state recompute writes nothing at all.
        let repeat = store.recompute_effective_authority(proj).unwrap();
        assert_eq!((repeat.files_changed, repeat.chunks_changed), (0, 0));
        assert_eq!(fts_footprint(&store), before_fts);

        // Guard against a vacuous assertion: the footprint *does* move when an
        // indexed column changes, so its stability above means something.
        store
            .conn
            .execute("UPDATE chunks SET text = text || ' addendum'", [])
            .unwrap();
        assert_ne!(
            fts_footprint(&store),
            before_fts,
            "the detector must be sensitive to real index churn"
        );
    }

    /// The other half of "no re-index": a recompute that lowers a tier is just
    /// as non-destructive as one that raises it, and a project whose ledger
    /// vanished must not lose its embeddings on the way down.
    #[test]
    fn a_demoting_recompute_is_as_non_destructive_as_a_promoting_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("lore.db")).unwrap();
        let proj = store
            .register_project(Utf8Path::new("C:/repos/x"), "x")
            .unwrap();
        opt_into_lore_v1(&mut store, proj);
        let active: BTreeSet<String> = ["D-0001".to_string()].into_iter().collect();
        store.set_active_decisions(proj, &active).unwrap();

        let path = Utf8Path::new("design/1.1.md");
        let chunk = vault_chunk(
            "design/1.1.md",
            "Overview",
            "Alpha section.",
            declares(DesignStatus::Decided, &["D-0001"]),
        );
        store
            .replace_file_chunks(proj, path, "h1", std::slice::from_ref(&chunk))
            .unwrap();
        store
            .upsert_embeddings(&[NewEmbedding {
                project: proj,
                chunk_id: chunk.id.clone(),
                vector: vec![1.0, 0.0, 0.0, 0.0],
            }])
            .unwrap();
        assert_eq!(chunk_rows(&store)[0].2, 3);

        let before_rows = chunk_rows(&store);
        let before_embeddings = embedding_rows(&store);
        let before_fts = fts_footprint(&store);

        // The ledger is deleted: nothing is active any more.
        assert!(store.set_active_decisions(proj, &BTreeSet::new()).unwrap());
        let summary = store.recompute_effective_authority(proj).unwrap();
        assert_eq!(summary.violations, 1, "{summary:?}");

        let after = chunk_rows(&store);
        assert_eq!(after[0].2, i64::from(UNCITED_DECIDED_TIER));
        assert_eq!(after[0].3.as_deref(), Some("uncited_decided"));
        assert_eq!(after[0].0, before_rows[0].0, "same rowid");
        assert_eq!(after[0].1, before_rows[0].1, "same chunk id");
        assert_eq!(embedding_rows(&store), before_embeddings);
        assert_eq!(fts_footprint(&store), before_fts);
    }

    /// The upgrade path the review demanded be survivable (S1#3/S1#7): a
    /// database written before V2 has no keys and no uniqueness on `name`, so
    /// two projects can legitimately share a display name. Migration plus
    /// startup reconciliation must give them distinct keys and must not refuse
    /// to start over a rule that only applies to *new* registrations.
    #[test]
    fn a_pre_v2_database_with_duplicate_display_names_migrates_and_gets_distinct_keys() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let db = data_dir.join("lore.db");

        // A V1 database, exactly as a pre-provenance daemon left it.
        {
            let mut conn = Connection::open(&db).unwrap();
            schema::migrations().to_version(&mut conn, 1).unwrap();
            for root in ["C:/repos/a/shared", "D:/work/shared"] {
                conn.execute(
                    "INSERT INTO projects (root, name) VALUES (?, 'shared')",
                    params![root],
                )
                .unwrap();
            }
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(version, 1, "the fixture really is pre-V2");
        }

        // Opening migrates. Keys start NULL, which `list_projects` reports as
        // empty rather than failing.
        let mut store = Store::open(&db).unwrap();
        assert!(
            store
                .list_projects()
                .unwrap()
                .iter()
                .all(|p| p.key.is_empty() && p.kind == SourceKind::Repo)
        );

        // Startup reconciliation is what assigns them.
        let outcome = crate::registry::reconcile(&mut store, &data_dir).expect("must not fail");
        assert!(
            matches!(
                outcome,
                crate::registry::Reconciliation::Bootstrapped {
                    projects: 2,
                    keys_assigned: 2
                }
            ),
            "{outcome:?}"
        );

        let projects = store.list_projects().unwrap();
        assert_eq!(projects[0].key, "shared");
        assert_ne!(projects[0].key, projects[1].key, "{projects:?}");
        assert!(projects[1].key.starts_with("shared-"), "{projects:?}");

        // Documented residual, asserted rather than assumed: the pre-existing
        // duplicate *display names* are left alone. Name enforcement guards new
        // registrations; it does not rewrite a user's history. So name
        // resolution stays ambiguous for these two rows — which is exactly why
        // `project_key` exists and why search results carry it.
        assert_eq!(projects[0].name, projects[1].name);
        assert_eq!(
            crate::daemon::resolve_project(&projects, "shared").map(|p| p.id),
            Some(projects[0].id),
            "by name, the first row silently wins"
        );
        for project in &projects {
            assert_eq!(
                crate::daemon::resolve_project_key(&projects, &project.key).map(|p| p.id),
                Some(project.id),
                "by key, each one is reachable"
            );
        }
    }
}
