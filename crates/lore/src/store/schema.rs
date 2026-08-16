//! Schema definition and migrations.
//!
//! Versioning is `PRAGMA user_version`, driven by `rusqlite_migration`. Every
//! migration is append-only: never edit a shipped `M::up` body, add a new one.

use rusqlite_migration::{M, Migrations};

/// Ordered migration list. Index + 1 == `user_version` after application.
pub(crate) fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(V1), M::up(V2), M::up(V3)])
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
