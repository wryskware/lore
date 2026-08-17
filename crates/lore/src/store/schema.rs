//! Schema definition and migrations.
//!
//! Versioning is `PRAGMA user_version`, driven by `rusqlite_migration`. Every
//! migration is append-only: never edit a shipped `M::up` body, add a new one.

use rusqlite_migration::{M, Migrations};

/// Ordered migration list. Index + 1 == `user_version` after application.
pub(crate) fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(V1), M::up(V2), M::up(V3), M::up(V4), M::up(V5)])
}

/// v1 — initial schema.
///
/// Shape notes:
/// - `chunks.id` is an ordinary rowid alias used as the join key for the FTS5
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
/// FTS5 tokenizer choice — `unicode61 remove_diacritics 2 tokenchars '_'`:
/// stock unicode61 treats `_` as a separator, which shatters `content_hash`
/// and `_privateField` into fragments and makes exact identifier search
/// impossible. Making `_` a token character keeps snake_case and `_`-prefixed
/// identifiers whole, while `.`, `:`, `-`, `/`, `#`, `$` stay separators — so
/// `Board.Update` still matches `Update`, `foo::bar` matches `bar`, and
/// `#region` matches `region`. Prefix queries (`content*`) still reach inside
/// a compound token, so nothing is a dead end. `remove_diacritics 2` is the
/// non-buggy variant of unicode61's diacritic folding.
const V1: &str = r#"
CREATE TABLE projects (
    id         INTEGER PRIMARY KEY,
    root       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE files (
    project_id   INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path         TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    indexed_at   INTEGER NOT NULL,
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
    UNIQUE (project_id, chunk_id),
    FOREIGN KEY (project_id, path) REFERENCES files(project_id, path) ON DELETE CASCADE
);

CREATE INDEX chunks_by_file ON chunks(project_id, path, byte_start);
CREATE INDEX chunks_by_status ON chunks(project_id, design_status);

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

/// v2 — declared vs effective authority, source provenance, stable keys.
///
/// Three independent review findings land in one migration because they touch
/// the same two tables and a migration boundary is not free:
///
/// - **S1#2 (authority laundering).** `chunks.authority_tier` keeps its
///   meaning — the tier a document *declares* — and is still what `status`
///   filters read. `effective_tier` is what ranking and `min_authority` read:
///   the declared tier after ledger validation and path/source ceilings (see
///   [`crate::authority`]). `demotion` records *why* they differ, as a stable
///   code rather than prose so the note can be reworded without a migration.
///   `project_decisions` persists the active-decision set parsed out of each
///   project's `**/0_Canon/DECISIONS.md`, which is the one input the effective
///   tier cannot derive from the chunk row alone — and having it stored is
///   what makes the recompute pass a pure store operation (no re-chunk, no
///   re-embed, chunk ids untouched).
/// - **S1#5 (provenance).** `projects.kind` turns the project table into a
///   source registry (`repo` today, `session` at M3, `issue` later);
///   `files.source_ts` is the source-declared timestamp the M3 recency term
///   will read. No CHECK constraint on `kind` on purpose: the vocabulary is
///   expected to grow, and a CHECK would make adding `issue` a table rebuild.
///   Unknown values read back as `repo`.
/// - **S1#3/S1#7 (identity).** `projects.key` is the stable opaque handle that
///   search results round-trip instead of a non-unique display name. SQLite
///   cannot `ADD COLUMN ... UNIQUE`, so uniqueness is a separate index; that
///   also allows the NULLs existing rows start with, which startup
///   reconciliation backfills (`crate::registry`).
///
/// The `effective_tier` backfill here is deliberately only an approximation
/// (declared == effective). The real values need each project's ledger, which
/// lives on disk, so the daemon recomputes on its first pass over every
/// project. Seeding the column with the declared tier means a daemon that
/// somehow never recomputes still ranks exactly as v1 did, rather than
/// flattening every document to a default.
///
/// `chunks_demoted` is partial: demotions are a small minority of rows, and
/// `status` asks "which files in this project are violating?" on every call.
const V2: &str = r#"
ALTER TABLE chunks ADD COLUMN effective_tier INTEGER NOT NULL DEFAULT 1;
ALTER TABLE chunks ADD COLUMN demotion TEXT;
UPDATE chunks SET effective_tier = authority_tier;

CREATE INDEX chunks_by_effective ON chunks(project_id, effective_tier);
CREATE INDEX chunks_demoted ON chunks(project_id, demotion) WHERE demotion IS NOT NULL;

CREATE TABLE project_decisions (
    project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    decision_id TEXT NOT NULL,
    PRIMARY KEY (project_id, decision_id)
) WITHOUT ROWID;

ALTER TABLE projects ADD COLUMN kind TEXT NOT NULL DEFAULT 'repo';
ALTER TABLE projects ADD COLUMN key TEXT;
CREATE UNIQUE INDEX projects_by_key ON projects(key);

ALTER TABLE files ADD COLUMN source_ts INTEGER;
"#;

/// v3 — authority is repository-opt-in (D-0012) and decision records are a
/// corpus with its own health (D-0013).
///
/// The resolved `.lore.toml` lives on the project row rather than being
/// re-read per query, for two reasons. Ranking needs it *per hit* (the weights
/// apply only to `behavior = "rank"` projects) and `lore status` needs it for
/// projects it is not otherwise touching; re-reading a file from disk on either
/// path would put IO inside the store lock. Persisting it also makes the
/// column the natural **profile fingerprint**: the index pass compares what it
/// just read against what is stored, and a difference is exactly the signal
/// that every effective tier in the project is now wrong.
///
/// `authority_profile IS NULL` is the neutral state, which is what every row
/// migrating from v2 starts as — correct rather than merely safe, because a
/// repo that has not been scanned since the upgrade genuinely has no known
/// profile yet, and the first scan writes one.
///
/// `authority_error` is stored, not just logged: a repo whose `.lore.toml` is
/// broken indexes neutrally, and D-0012 requires that be loud in `lore status`
/// even when nothing has re-scanned since.
///
/// `decisions_total` and `decision_violations` are per project rather than a
/// table, because they are a summary that is rewritten whole on every ledger
/// refresh and only ever read whole.
const V3: &str = r#"
ALTER TABLE projects ADD COLUMN authority_profile  TEXT;
ALTER TABLE projects ADD COLUMN authority_behavior TEXT;
ALTER TABLE projects ADD COLUMN authority_error    TEXT;
ALTER TABLE projects ADD COLUMN decisions_total    INTEGER NOT NULL DEFAULT 0;
-- serde_json array of authority::DecisionViolation; NULL == none.
ALTER TABLE projects ADD COLUMN decision_violations TEXT;
"#;

/// v4 — the push epoch survives a restart (D-0015).
///
/// The lease itself is process state: a daemon that restarts holds no leases,
/// and every pusher has to acquire again. The *epoch* cannot be, because it is
/// the thing that makes a stale pusher's commit refusable. If a restart minted
/// epoch 1 again, a pusher still holding a handle stamped with the previous
/// run's epoch 1 would look current, and its snapshot — observed before
/// whatever the restart was for — could publish over a newer one.
///
/// So the counter lives on the project row and only ever increments, in the
/// same statement that reads it (`UPDATE ... RETURNING`), which is what makes
/// two simultaneous acquirers get two distinct epochs with a defined order.
/// Nothing resets it: a project removed and re-added is a new row, and a new
/// row starting at 0 is correct, because no handle can name a project id that
/// did not exist when the handle was minted.
///
/// That last clause is only true once ids stop being recycled, which is what
/// [`V5`] makes structural.
const V4: &str = r#"
ALTER TABLE projects ADD COLUMN push_epoch INTEGER NOT NULL DEFAULT 0;
"#;

/// v5 — a project id is never handed out twice.
///
/// `id INTEGER PRIMARY KEY` is a rowid alias, and SQLite allocates a plain
/// rowid as `max(rowid) + 1`. Delete the highest project and the next
/// registration is handed the id that just died — so anything still holding the
/// old id (a push handle, an in-memory lease/queue/watch entry keyed by
/// `ProjectId`, a client that cached one) silently re-attaches to a *different*
/// project rather than failing. Every holder defending itself against that
/// individually is one forgotten `forget` away from the bug; the store is the
/// only place it can be closed once.
///
/// `AUTOINCREMENT` is exactly that guarantee: SQLite keeps the high-water mark
/// in `sqlite_sequence` and refuses to reuse anything at or below it, so an id
/// is retired the moment it is allocated. The cost is one extra row read and
/// written per insert into a table that gains rows when a human runs
/// `lore add`, which is not a rate worth optimizing.
///
/// SQLite cannot `ALTER TABLE ... ADD AUTOINCREMENT`, so this is the documented
/// table-rebuild dance: new table, copy, drop, rename. Notes on the pieces:
///
/// - **Every column, default and constraint is reproduced verbatim** from v1's
///   `projects` plus the columns v2/v3/v4 appended, in the order
///   `ALTER TABLE ADD COLUMN` left them, so a `SELECT *` reads identically
///   before and after. `root`'s `UNIQUE` comes back with the table; the
///   `projects_by_key` index has to be recreated by hand, because dropping a
///   table drops its indexes.
/// - **Rows are copied with their ids.** Existing project ids are live
///   references (`files.project_id`, `chunks.project_id`,
///   `embeddings.project_id`, `project_decisions.project_id`, and the manifest's
///   view of the world); renumbering them would be a far worse bug than the one
///   this fixes.
/// - **The sequence is seeded above every id this database can still see.**
///   Copying rows already pushes `sqlite_sequence` to `max(projects.id)`, but
///   the referencing tables are the only remaining witness of an id that was
///   allocated and then deleted — a row orphaned by an interrupted removal names
///   an id that must never be issued again. Ids whose last trace is already
///   gone are unknowable; for those `max + 1` is the best available floor, and
///   the reuse window it leaves is closed for every id allocated from here on.
/// - **Foreign keys must be off**, which is `rusqlite_migration`'s documented
///   requirement and what [`crate::store::Store::open`] does. The guard table
///   makes it a refusal instead of a catastrophe: with enforcement on,
///   `DROP TABLE projects` is an implicit `DELETE` that would cascade away every
///   file, chunk and vector in the database. `PRAGMA` is a no-op inside the
///   migration's transaction, so aborting is the only thing left to do.
const V5: &str = r#"
CREATE TABLE migration_v5_guard (
    foreign_keys INTEGER NOT NULL CHECK (foreign_keys = 0)
);
INSERT INTO migration_v5_guard SELECT * FROM pragma_foreign_keys;
DROP TABLE migration_v5_guard;

CREATE TABLE projects_v5 (
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
    decision_violations TEXT,
    push_epoch          INTEGER NOT NULL DEFAULT 0
);

INSERT INTO projects_v5 (
    id, root, name, created_at, kind, key,
    authority_profile, authority_behavior, authority_error,
    decisions_total, decision_violations, push_epoch
)
SELECT
    id, root, name, created_at, kind, key,
    authority_profile, authority_behavior, authority_error,
    decisions_total, decision_violations, push_epoch
FROM projects;

DROP TABLE projects;
ALTER TABLE projects_v5 RENAME TO projects;

CREATE UNIQUE INDEX projects_by_key ON projects(key);

DELETE FROM sqlite_sequence WHERE name = 'projects';
INSERT INTO sqlite_sequence (name, seq)
SELECT 'projects', MAX(high_water) FROM (
    SELECT COALESCE(MAX(id), 0)         AS high_water FROM projects
    UNION ALL SELECT COALESCE(MAX(project_id), 0) FROM files
    UNION ALL SELECT COALESCE(MAX(project_id), 0) FROM chunks
    UNION ALL SELECT COALESCE(MAX(project_id), 0) FROM embeddings
    UNION ALL SELECT COALESCE(MAX(project_id), 0) FROM project_decisions
);
"#;
