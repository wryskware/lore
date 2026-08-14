//! Schema definition and migrations.
//!
//! Versioning is `PRAGMA user_version`, driven by `rusqlite_migration`. Every
//! migration is append-only: never edit a shipped `M::up` body, add a new one.

use rusqlite_migration::{M, Migrations};

/// Ordered migration list. Index + 1 == `user_version` after application.
pub(crate) fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(V1)])
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
