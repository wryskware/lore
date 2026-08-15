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

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::types::{Chunk, ChunkId, ChunkKind, DesignStatus, VaultMeta, authority_tier};

use query::{filter_sql, sanitize_fts_query};
use vector::{Scored, TopK};

/// Opaque project handle. Stable for the life of the database.
pub type ProjectId = i64;

pub type Result<T> = std::result::Result<T, StoreError>;

/// How long to wait for a competing writer before giving up. The daemon is
/// the only writer, but CLI/read tooling may attach to the same file.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Chunk columns, in the order [`row_to_chunk`] expects. `c` is the chunks
/// table alias in every query that uses this.
const CHUNK_COLS: &str = "c.project_id, c.chunk_id, c.path, c.kind, c.language, \
     c.byte_start, c.byte_end, c.line_start, c.line_end, c.text, c.vault";

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

/// A registered project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub root: Utf8PathBuf,
    pub name: String,
}

/// A file's indexing state, for change detection and orphan pruning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub path: Utf8PathBuf,
    pub content_hash: String,
    /// Unix seconds at last successful index.
    pub indexed_at: i64,
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
    /// Allowed `design_status` values.
    pub statuses: Option<Vec<StatusFilter>>,
    /// Minimum [`crate::types::authority_tier`] — e.g. `Some(1)` drops
    /// `deprecated` material without enumerating every other status.
    pub min_authority: Option<u8>,
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

/// Identity of the embedding space every stored vector belongs to.
///
/// Stored opaquely (serialized whole). A mismatch means the vectors in the
/// database are not comparable to newly produced ones; resolving that is the
/// **caller's** job ([`Store::clear_all_embeddings`] then re-embed). The store
/// never silently mixes vector spaces (3.1, `model_id_tag` pattern).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    pub model_id: String,
    pub dimensions: u32,
    pub query_prefix: String,
    pub document_prefix: String,
    /// Free-form normalization tag, e.g. "l2".
    pub normalization: String,
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
    pub files: u64,
    pub chunks: u64,
    pub embedded_chunks: u64,
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

    /// Register a project root, or update the display name of an existing
    /// one. Idempotent on `root`; the caller is responsible for handing in an
    /// already-canonicalized path.
    pub fn register_project(&mut self, root: &Utf8Path, name: &str) -> Result<ProjectId> {
        let id = self.conn.query_row(
            "INSERT INTO projects (root, name) VALUES (?, ?)
             ON CONFLICT(root) DO UPDATE SET name = excluded.name
             RETURNING id",
            params![root.as_str(), name],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, root, name FROM projects ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Project {
                id: r.get(0)?,
                root: Utf8PathBuf::from(r.get::<_, String>(1)?),
                name: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
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
            "SELECT path, content_hash, indexed_at FROM files WHERE project_id = ? ORDER BY path",
        )?;
        let rows = stmt.query_map(params![project], |r| {
            Ok(FileRecord {
                path: Utf8PathBuf::from(r.get::<_, String>(0)?),
                content_hash: r.get(1)?,
                indexed_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
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
                     design_status, authority_tier)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(project_id, chunk_id) DO UPDATE SET
                     kind = excluded.kind,
                     language = excluded.language,
                     byte_start = excluded.byte_start,
                     byte_end = excluded.byte_end,
                     line_start = excluded.line_start,
                     line_end = excluded.line_end,
                     vault = excluded.vault,
                     design_status = excluded.design_status,
                     authority_tier = excluded.authority_tier",
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
                ])?;
                if existing.contains(c.id.0.as_str()) {
                    stats.kept += 1;
                } else {
                    stats.inserted += 1;
                }
            }
        }

        tx.commit()?;
        Ok(stats)
    }

    /// Forget a file entirely: chunks, FTS rows and embeddings go with it.
    /// Returns whether the file was known.
    pub fn remove_file(&mut self, project: ProjectId, path: &Utf8Path) -> Result<bool> {
        let tx = self.conn.transaction()?;
        // Explicit chunk delete (rather than relying on the files FK cascade)
        // so the FTS triggers fire on a plain DELETE statement.
        tx.execute(
            "DELETE FROM chunks WHERE project_id = ? AND path = ?",
            params![project, path.as_str()],
        )?;
        let removed = tx.execute(
            "DELETE FROM files WHERE project_id = ? AND path = ?",
            params![project, path.as_str()],
        )?;
        tx.commit()?;
        Ok(removed > 0)
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
            "SELECT p.id, p.root, p.name,
                    (SELECT COUNT(*) FROM files f WHERE f.project_id = p.id),
                    (SELECT COUNT(*) FROM chunks c WHERE c.project_id = p.id),
                    (SELECT COUNT(*) FROM embeddings e WHERE e.project_id = p.id)
             FROM projects p ORDER BY p.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectStatus {
                project: r.get(0)?,
                root: Utf8PathBuf::from(r.get::<_, String>(1)?),
                name: r.get(2)?,
                files: r.get::<_, i64>(3)? as u64,
                chunks: r.get::<_, i64>(4)? as u64,
                embedded_chunks: r.get::<_, i64>(5)? as u64,
            })
        })?;
        let projects = rows.collect::<std::result::Result<_, _>>()?;
        Ok(Status {
            generation: self.generation()?,
            projects,
        })
    }

    // ---- retrieval --------------------------------------------------------

    /// BM25 lexical search over chunk text, path and symbol/heading anchor.
    ///
    /// `query` is arbitrary user text: it is sanitized into a safe FTS5 MATCH
    /// expression (see [`query::sanitize_fts_query`]), so malformed input
    /// returns `Ok` with sensible-or-empty results rather than an error or a
    /// panic. Returned scores are `-bm25(...)`, i.e. higher is better.
    pub fn lexical_search(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let match_expr = sanitize_fts_query(query);
        if match_expr.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let f = filter_sql(filter);
        let sql = format!(
            "SELECT {CHUNK_COLS}, -bm25(chunks_fts, {BM25_WEIGHTS}) AS score
             FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid
             WHERE chunks_fts MATCH ?{}
             ORDER BY score DESC, c.id ASC
             LIMIT ?",
            f.sql
        );

        let mut params: Vec<Value> = vec![Value::Text(match_expr)];
        params.extend(f.params);
        params.push(Value::Integer(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(params))?;
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            hits.push(SearchHit {
                project: row.get(0)?,
                chunk: row_to_chunk(row)?,
                score: row.get::<_, f64>(11)? as f32,
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
                rowid: row.get(11)?,
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

    /// Drop every stored vector. Leaves the fingerprint alone — the caller
    /// decides what the new embedding space is and sets it explicitly.
    /// Returns the number of vectors discarded.
    pub fn clear_all_embeddings(&mut self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM embeddings", [])?)
    }
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

/// White-box checks that need the connection itself — the storage invariants
/// the public API cannot observe. Behavioural coverage lives in
/// `tests/store_sqlite.rs`.
#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(version, 1, "one migration applied");
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
}
