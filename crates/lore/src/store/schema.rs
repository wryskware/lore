//! The store schema: one flat definition, no migrations.
//!
//! # The one exception: the FTS layer rebuilds itself (see [`FTS_VERSION`])
//!
//! The lexical index is *derived from a table in the same file*, so it is the
//! one part of the schema that can be replaced without discarding anything.
//! [`ensure_fts_current`] does exactly that and nothing else. It is not a
//! migration framework and must not grow into one: it can only ever drop and
//! re-derive [`FTS_SCHEMA`], and any change that touches a base table still
//! goes through [`VERSION`] and the discard below.
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
//! If a pre-release schema change ever *does* need data to survive — an
//! expensive embed corpus, a store that took days to build — the escape hatch
//! is manual, not framework: back up `lore.db` and operate on the copy
//! surgically (`sqlite3` and a transcript of what you did), or decide at that
//! moment that migration support has earned its way in. Do not resurrect a
//! migration framework casually for one awkward change.
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

/// The layout of the derived lexical index, versioned *separately* from
/// [`VERSION`] — and that separation is the whole point.
///
/// Bumping [`VERSION`] throws the database away, which re-chunks and, far more
/// expensively, **re-embeds** every registered project: minutes to hours of
/// local GPU for a change that touches only postings. The FTS5 index is the one
/// structure in the file that is derived from another structure in the same
/// file, so it can be dropped and re-derived from `chunks` in place. Chunk
/// rowids never move, `embeddings` is keyed by chunk rowid, and neither table is
/// written — so the vectors survive an index rebuild untouched.
///
/// Bump this (not [`VERSION`]) for any change to [`FTS_SCHEMA`]: a column, the
/// tokenizer, the trigger payloads. `0` is the pre-subword layout that shipped
/// without this counter at all, which is why the column defaults to it.
///
/// - `1` — added the `subwords` column (see [`super::subword`]).
pub(crate) const FTS_VERSION: i64 = 1;

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
        "BEGIN;{SCHEMA}{FTS_SCHEMA}
         INSERT INTO meta (id, generation, embedding_fingerprint, fts_version)
         VALUES (1, 0, NULL, {FTS_VERSION});
         PRAGMA user_version = {VERSION};
         COMMIT;"
    ))
}

/// Bring an existing database's lexical index up to [`FTS_VERSION`], rebuilding
/// it from `chunks` if it is behind. Reports whether it did.
///
/// Runs on every open of a current database and writes nothing when the index
/// is already current, so it costs one `SELECT` on the ordinary path.
///
/// **What survives:** everything except the FTS5 postings. `chunks` is only
/// read, so chunk rowids do not move; `embeddings` is keyed by chunk rowid and
/// is not touched at all, which is the property that makes this affordable
/// enough to exist (a [`VERSION`] bump would re-embed the whole corpus to fix a
/// tokenization bug).
///
/// One transaction: a crash partway leaves the old index and the old
/// `fts_version`, and the next open tries again. The `rebuild` command
/// re-reads every chunk through the content view, so it is the single
/// expensive statement here — linear in corpus size, and the reason this is
/// gated on a version rather than run unconditionally.
///
/// The `ALTER TABLE` is the price of introducing the counter into databases
/// written before it existed. Those carry `fts_version = 0` by construction,
/// which is exactly the "needs rebuilding" state they are in.
pub(crate) fn ensure_fts_current(conn: &Connection) -> rusqlite::Result<bool> {
    let has_column = conn
        .prepare("SELECT 1 FROM pragma_table_info('meta') WHERE name = 'fts_version'")?
        .exists([])?;
    if !has_column {
        conn.execute_batch("ALTER TABLE meta ADD COLUMN fts_version INTEGER NOT NULL DEFAULT 0")?;
    }

    let found: i64 = conn.query_row("SELECT fts_version FROM meta WHERE id = 1", [], |r| {
        r.get(0)
    })?;
    if found == FTS_VERSION {
        return Ok(false);
    }

    conn.execute_batch(&format!(
        "BEGIN;
         DROP TRIGGER IF EXISTS chunks_ai;
         DROP TRIGGER IF EXISTS chunks_ad;
         DROP TRIGGER IF EXISTS chunks_au;
         DROP TABLE IF EXISTS chunks_fts;
         DROP VIEW IF EXISTS chunks_fts_content;
         {FTS_SCHEMA}
         INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild');
         UPDATE meta SET fts_version = {FTS_VERSION} WHERE id = 1;
         COMMIT;"
    ))?;
    Ok(true)
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
/// The base tables. The derived lexical index is [`FTS_SCHEMA`], appended by
/// [`create`] — separate because [`ensure_fts_current`] re-emits that half on
/// its own.
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

-- Single-row store-wide state: monotonic index generation, the embedding
-- fingerprint (serde_json of store::EmbeddingFingerprint, NULL when unset),
-- and the layout version of the derived lexical index (schema::FTS_VERSION).
CREATE TABLE meta (
    id                    INTEGER PRIMARY KEY CHECK (id = 1),
    generation            INTEGER NOT NULL,
    embedding_fingerprint TEXT,
    fts_version           INTEGER NOT NULL DEFAULT 0
);
"#;

/// The derived lexical index: the content view, the FTS5 table over it, and
/// the triggers that keep the two in sync.
///
/// Emitted both by [`create`] (fresh database) and by [`ensure_fts_current`]
/// (existing database whose index predates [`FTS_VERSION`]), so there is one
/// definition and the rebuild cannot drift from the thing it rebuilds.
///
/// **Tokenizer — `unicode61 remove_diacritics 2 tokenchars '_'`:** stock
/// unicode61 treats `_` as a separator, which shatters `content_hash` and
/// `_privateField` into fragments and makes exact identifier search impossible.
/// Making `_` a token character keeps snake_case and `_`-prefixed identifiers
/// whole, while `.`, `:`, `-`, `/`, `#`, `$` stay separators — so `Board.Update`
/// still matches `Update`, `foo::bar` matches `bar`, and `#region` matches
/// `region`. Prefix queries (`content*`) still reach inside a compound token, so
/// nothing is a dead end. `remove_diacritics 2` is the non-buggy variant of
/// unicode61's diacritic folding.
///
/// **`subwords` is what makes that choice survivable.** Keeping identifiers
/// whole is right for exact search and blind for everything else: nothing a
/// human types as "dispatch fanout" can reach a token spelled
/// `_dispatch_fanout`, and FTS5 has no infix search to fall back on. The column
/// carries the subword expansion of the other three ([`super::subword`]) — and
/// *only* the expansion, so a chunk of prose contributes an empty column and its
/// ranking cannot move. Anchor and path are folded in beside text because they
/// are where identifiers are densest: `code:ConcurrentOrchestration` and
/// `.../_concurrent.py` are the strongest evidence a chunk is the one being
/// asked about, and both were previously reachable only by spelling them.
///
/// **The content table is a view, not `chunks`.** `subwords` is a pure function
/// of the other three columns, and storing it would mean a second copy of the
/// corpus in `chunks` plus an invariant ("recompute it whenever text changes")
/// that a future writer can forget. A view cannot go stale. The cost is that
/// `chunks_fts` can only be opened by a connection that has registered
/// `lore_subwords` — [`super::Store::connect`] is the only place a connection to
/// this file is made, so that is total inside Lore, but `sqlite3` on the command
/// line will fail on `rebuild`, `integrity-check`, or selecting an FTS column
/// (querying `chunks_fts_data` or MATCHing for rowids still works).
const FTS_SCHEMA: &str = r#"
CREATE VIEW chunks_fts_content AS
SELECT id,
       text,
       path,
       anchor,
       lore_subwords(text, anchor) AS subwords
FROM chunks;

CREATE VIRTUAL TABLE chunks_fts USING fts5(
    text,
    path,
    anchor,
    subwords,
    content='chunks_fts_content',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2 tokenchars '_'"
);

CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, text, path, anchor, subwords)
    VALUES (new.id, new.text, new.path, new.anchor,
            lore_subwords(new.text, new.anchor));
END;

CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text, path, anchor, subwords)
    VALUES ('delete', old.id, old.text, old.path, old.anchor,
            lore_subwords(old.text, old.anchor));
END;

-- Only the indexed columns can invalidate the FTS row; span/status updates on
-- an otherwise unchanged chunk must not churn the index. `subwords` is derived
-- from these same three, so it needs no entry of its own in the UPDATE OF list.
CREATE TRIGGER chunks_au AFTER UPDATE OF text, path, anchor ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text, path, anchor, subwords)
    VALUES ('delete', old.id, old.text, old.path, old.anchor,
            lore_subwords(old.text, old.anchor));
    INSERT INTO chunks_fts(rowid, text, path, anchor, subwords)
    VALUES (new.id, new.text, new.path, new.anchor,
            lore_subwords(new.text, new.anchor));
END;
"#;
