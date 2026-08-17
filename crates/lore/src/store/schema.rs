//! The store schema: one flat definition, no migrations.
//!
//! # Why there is no migration list (D-0018 — temporal)
//!
//! D-0018 is a **pre-release posture with an explicit expiry**: until the first
//! tagged release, no schema migrations are authored. The schema is a single
//! definition, and a database that does not carry [`VERSION`] is *discarded and
//! rebuilt* rather than migrated ([`super::Store::open`]). That is affordable
//! because the store is derived data — repos are the source of truth, and the
//! registry, handshake and session Markdown live beside the database rather
//! than in it — so a mismatch costs a re-index and a re-embed, not data.
//!
//! **This posture ends at the first tagged release.** Migration and versioning
//! policy must then be decided anew; nothing here is precedent for what comes
//! after, and any comment citing D-0018 inherits its expiry. Until then, the
//! way to change the schema is to edit [`SCHEMA`] in place and let existing
//! stores rebuild.
//!
//! Everything else in this file — the shape notes, the tokenizer choice, the
//! AUTOINCREMENT guarantee — is permanent reasoning about *what the schema is*,
//! and outlives the posture that removed the migration machinery.

use rusqlite::Connection;

/// The one schema version this build understands.
///
/// Not a floor and not a range: [`super::Store::open`] rebuilds on anything
/// else, in either direction (an older database from a previous build, a newer
/// one written by a binary that has since been rolled back).
pub(crate) const VERSION: i64 = 1;

/// What [`inspect`] found in the file that was just opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Found {
    /// Nothing has ever been written here: [`create`] can run.
    Empty,
    /// Exactly this build's schema.
    Current,
    /// A database this build cannot read — a different `user_version`, or a
    /// `user_version` of 0 over tables that exist (which is what a `create`
    /// interrupted before its commit leaves behind).
    Foreign { user_version: i64 },
}

/// Classify an open connection without writing to it.
pub(crate) fn inspect(conn: &Connection) -> rusqlite::Result<Found> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if user_version == VERSION {
        return Ok(Found::Current);
    }
    let tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if user_version == 0 && tables == 0 {
        Ok(Found::Empty)
    } else {
        Ok(Found::Foreign { user_version })
    }
}

/// Create the schema in an empty database and stamp [`VERSION`].
///
/// One transaction, version stamp included: `PRAGMA user_version` is a header
/// write and commits with everything else, so a database can never be found
/// carrying this build's version over a half-created schema. A crash partway
/// leaves version 0 over tables, which [`inspect`] reports as [`Found::Foreign`]
/// and the next open rebuilds.
pub(crate) fn create(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "BEGIN;{SCHEMA}\nPRAGMA user_version = {VERSION};\nCOMMIT;"
    ))
}

/// The whole schema.
///
/// Shape notes:
/// - `chunks.id` is a rowid alias used as the join key for the FTS5
///   external-content table and for `embeddings`. The *public* identity of a
///   chunk stays `(project_id, chunk_id)`; `id` never leaves this module.
/// - `chunks` upserts on `(project_id, chunk_id)`, so a chunk whose
///   content-addressed id is unchanged keeps its rowid — and therefore keeps
///   its embedding row across re-index. That is the whole point of
///   content-addressed ids (3.1 Chunking_and_Ranking).
/// - `design_status` is its own filterable column (denormalized out of the
///   `vault` JSON blob) plus a precomputed `authority_tier` for ranking.
/// - FTS5 is *external content* (`content='chunks'`): the index stores
///   postings only, the column text lives once in `chunks`. Sync is by
///   trigger, so an FTS row physically cannot be written in a different
///   transaction than the chunk row that produced it.
///
/// **Declared vs effective authority.** `chunks.authority_tier` is the tier a
/// document *declares*, and is what `status` filters read. `effective_tier` is
/// what ranking and `min_authority` read: the declared tier after ledger
/// validation and path/source ceilings (see [`crate::authority`]). `demotion`
/// records *why* they differ, as a stable code rather than prose so the note
/// can be reworded freely. `project_decisions` persists the active-decision set
/// parsed out of each project's `**/0_Canon/DECISIONS.md` — the one input the
/// effective tier cannot derive from the chunk row alone, and having it stored
/// is what makes the recompute pass a pure store operation (no re-chunk, no
/// re-embed, chunk ids untouched).
///
/// **Provenance.** `projects.kind` makes the project table a source registry
/// (`repo` today, `session` at M3, `issue` later); `files.source_ts` is the
/// source-declared timestamp the M3 recency term will read. No CHECK constraint
/// on `kind` on purpose: the vocabulary is expected to grow. Unknown values read
/// back as `repo`.
///
/// **Identity.** `projects.key` is the stable opaque handle that search results
/// round-trip instead of a display name (S1#3/S1#7). It is nullable so that
/// [`crate::store::Store::apply_project_set`] can release every key to NULL
/// mid-reconciliation — SQLite treats NULLs as distinct in a unique index —
/// before claiming the new ones; uniqueness is therefore a separate index
/// rather than a column constraint.
///
/// **The resolved `.lore.toml` lives on the project row** rather than being
/// re-read per query (D-0012). Ranking needs it per hit (the weights apply only
/// to `behavior = "rank"` projects) and `lore status` needs it for projects it
/// is not otherwise touching; re-reading a file from disk on either path would
/// put IO inside the store lock. Persisting it also makes the column the
/// natural **profile fingerprint**: the index pass compares what it just read
/// against what is stored, and a difference is exactly the signal that every
/// effective tier in the project is now wrong. `authority_profile IS NULL` is
/// the neutral state. `authority_error` is stored, not just logged: a repo whose
/// `.lore.toml` is broken indexes neutrally, and D-0012 requires that be loud in
/// `lore status` even when nothing has re-scanned since. `decisions_total` and
/// `decision_violations` are per project rather than a table, because they are a
/// summary rewritten whole on every ledger refresh and only ever read whole.
///
/// **`push_epoch` survives a restart** (D-0015). The lease itself is process
/// state: a daemon that restarts holds no leases. The *epoch* cannot be, because
/// it is the thing that makes a stale pusher's commit refusable — if a restart
/// minted epoch 1 again, a pusher still holding a handle stamped with the
/// previous run's epoch 1 would look current, and its snapshot could publish
/// over a newer one. So the counter lives on the project row and only ever
/// increments, in the same statement that reads it (`UPDATE ... RETURNING`),
/// which is what makes two simultaneous acquirers get two distinct epochs with a
/// defined order. Nothing resets it: a project removed and re-added is a new
/// row, and a new row starting at 0 is correct, because no handle can name a
/// project id that did not exist when the handle was minted — which is only true
/// because ids are never reused, below.
///
/// **`projects.id` is `AUTOINCREMENT`, and that is load-bearing.** A plain
/// `INTEGER PRIMARY KEY` is a rowid alias, and SQLite allocates a plain rowid as
/// `max(rowid) + 1`. Delete the highest project and the next registration is
/// handed the id that just died — so anything still holding the old id (a push
/// handle, an in-memory lease/queue/watch entry keyed by `ProjectId`, a client
/// that cached one) silently re-attaches to a *different* project rather than
/// failing. Every holder defending itself against that individually is one
/// forgotten `forget` away from the bug; the store is the only place it can be
/// closed once. `AUTOINCREMENT` is exactly that guarantee: SQLite keeps the
/// high-water mark in `sqlite_sequence` and refuses to reuse anything at or
/// below it, so an id is retired the moment it is allocated. The cost is one
/// extra row read and written per insert into a table that gains rows when a
/// human runs `lore add`, which is not a rate worth optimizing.
///
/// FTS5 tokenizer choice — `unicode61 remove_diacritics 2 tokenchars '_'`:
/// stock unicode61 treats `_` as a separator, which shatters `content_hash`
/// and `_privateField` into fragments and makes exact identifier search
/// impossible. Making `_` a token character keeps snake_case and `_`-prefixed
/// identifiers whole, while `.`, `:`, `-`, `/`, `#`, `$` stay separators — so
/// `Board.Update` still matches `Update`, `foo::bar` matches `bar`, and
/// `#region` matches `region`. Prefix queries (`content*`) still reach inside
/// a compound token, so nothing is a dead end. `remove_diacritics 2` is the
/// non-buggy variant of unicode61's diacritic folding.
const SCHEMA: &str = r#"
CREATE TABLE projects (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    root                TEXT NOT NULL UNIQUE,
    name                TEXT NOT NULL,
    created_at          INTEGER NOT NULL DEFAULT (unixepoch()),
    kind                TEXT NOT NULL DEFAULT 'repo',
    key                 TEXT,
    authority_profile   TEXT,
    authority_behavior  TEXT,
    authority_error     TEXT,
    decisions_total     INTEGER NOT NULL DEFAULT 0,
    -- serde_json array of authority::DecisionViolation; NULL == none.
    decision_violations TEXT,
    push_epoch          INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX projects_by_key ON projects(key);

CREATE TABLE files (
    project_id   INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path         TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    indexed_at   INTEGER NOT NULL,
    -- Source-declared timestamp (a session's write time), as opposed to when
    -- Lore happened to index it. NULL for repo files.
    source_ts    INTEGER,
    PRIMARY KEY (project_id, path)
) WITHOUT ROWID;

CREATE TABLE chunks (
    id             INTEGER PRIMARY KEY,
    project_id     INTEGER NOT NULL,
    chunk_id       TEXT NOT NULL,
    path           TEXT NOT NULL,
    -- ChunkKind::anchor(): "code:Foo.Bar", "md:A > B", "win:3". Indexed by
    -- FTS so symbol and heading names are searchable on their own.
    anchor         TEXT NOT NULL,
    -- serde_json of types::ChunkKind (tagged enum).
    kind           TEXT NOT NULL,
    language       TEXT,
    byte_start     INTEGER NOT NULL,
    byte_end       INTEGER NOT NULL,
    line_start     INTEGER NOT NULL,
    line_end       INTEGER NOT NULL,
    text           TEXT NOT NULL,
    -- serde_json of types::VaultMeta; NULL for non-vault files.
    vault          TEXT,
    -- Denormalized from vault.design_status; NULL == unclassified.
    design_status  TEXT,
    -- types::authority_tier(design_status), precomputed for ranking/filtering.
    authority_tier INTEGER NOT NULL,
    -- The declared tier after ledger validation and ceilings; what ranking and
    -- min_authority read.
    effective_tier INTEGER NOT NULL DEFAULT 1,
    -- Stable authority::Demotion code; NULL when declared == effective.
    demotion       TEXT,
    UNIQUE (project_id, chunk_id),
    FOREIGN KEY (project_id, path) REFERENCES files(project_id, path) ON DELETE CASCADE
);

CREATE INDEX chunks_by_file ON chunks(project_id, path, byte_start);
CREATE INDEX chunks_by_status ON chunks(project_id, design_status);
CREATE INDEX chunks_by_effective ON chunks(project_id, effective_tier);
-- Partial: demotions are a small minority of rows, and `status` asks "which
-- files in this project are violating?" on every call.
CREATE INDEX chunks_demoted ON chunks(project_id, demotion) WHERE demotion IS NOT NULL;

CREATE TABLE embeddings (
    -- Keyed by chunk rowid: FK cascade removes the vector the moment its
    -- chunk goes away, and surviving chunks keep their vectors for free.
    chunk_rowid INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
    project_id  INTEGER NOT NULL,
    dims        INTEGER NOT NULL,
    -- dims * 4 bytes, little-endian f32, L2-normalized on write.
    vector      BLOB NOT NULL
);

CREATE INDEX embeddings_by_project ON embeddings(project_id);

CREATE TABLE project_decisions (
    project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    decision_id TEXT NOT NULL,
    PRIMARY KEY (project_id, decision_id)
) WITHOUT ROWID;

CREATE VIRTUAL TABLE chunks_fts USING fts5(
    text,
    path,
    anchor,
    content='chunks',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2 tokenchars '_'"
);

CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, text, path, anchor)
    VALUES (new.id, new.text, new.path, new.anchor);
END;

CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text, path, anchor)
    VALUES ('delete', old.id, old.text, old.path, old.anchor);
END;

-- Only the indexed columns can invalidate the FTS row; span/status updates on
-- an otherwise unchanged chunk must not churn the index.
CREATE TRIGGER chunks_au AFTER UPDATE OF text, path, anchor ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text, path, anchor)
    VALUES ('delete', old.id, old.text, old.path, old.anchor);
    INSERT INTO chunks_fts(rowid, text, path, anchor)
    VALUES (new.id, new.text, new.path, new.anchor);
END;

-- Single-row store-wide state: monotonic index generation + the embedding
-- fingerprint (serde_json of store::EmbeddingFingerprint, NULL when unset).
CREATE TABLE meta (
    id                    INTEGER PRIMARY KEY CHECK (id = 1),
    generation            INTEGER NOT NULL,
    embedding_fingerprint TEXT
);
INSERT INTO meta (id, generation, embedding_fingerprint) VALUES (1, 0, NULL);
"#;
