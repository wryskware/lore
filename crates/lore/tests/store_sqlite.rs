//! Behavioural tests for the SQLite SearchStore.
//!
//! These exercise the store through its public API only — no SQL, no
//! assumptions about table layout — so they stay valid if the seam is
//! re-implemented on Tantivy+arroy.

use camino::{Utf8Path, Utf8PathBuf};
use lore::store::{
    EmbeddingFingerprint, NewEmbedding, SearchFilter, StatusFilter, Store, StoreError,
};
use lore::types::{Chunk, ChunkKind, DesignStatus, VaultMeta};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Store {
    Store::open(dir.path().join("lore.db")).expect("open store")
}

fn code_chunk(path: &str, symbol: &str, text: &str, language: &str) -> Chunk {
    let path = Utf8PathBuf::from(path);
    let kind = ChunkKind::Code {
        symbol_kind: "function".into(),
        symbol_path: symbol.into(),
    };
    Chunk {
        id: Chunk::derive_id(&path, &kind, text),
        path,
        kind,
        language: Some(language.into()),
        byte_start: 0,
        byte_end: text.len() as u32,
        line_start: 1,
        line_end: 1 + text.lines().count() as u32,
        text: text.into(),
        vault: None,
    }
}

fn section_chunk(path: &str, heading: &str, text: &str, status: Option<DesignStatus>) -> Chunk {
    let path = Utf8PathBuf::from(path);
    let kind = ChunkKind::Section {
        heading_path: vec![heading.into()],
    };
    Chunk {
        id: Chunk::derive_id(&path, &kind, text),
        path,
        kind,
        language: Some("markdown".into()),
        byte_start: 0,
        byte_end: text.len() as u32,
        line_start: 1,
        line_end: 2,
        text: text.into(),
        vault: Some(VaultMeta {
            design_status: status,
            decision_refs: vec!["D-0004".into()],
            body_decision_refs: vec![],
        }),
    }
}

fn p(s: &str) -> &Utf8Path {
    Utf8Path::new(s)
}

// ---------------------------------------------------------------------------
// open / migrate
// ---------------------------------------------------------------------------

#[test]
fn open_migrate_and_reopen_are_idempotent() {
    let dir = TempDir::new().unwrap();
    let project;
    {
        let mut store = open(&dir);
        project = store.register_project(p("C:/repos/lore"), "lore").unwrap();
        assert_eq!(store.bump_generation().unwrap(), 1);
    }
    {
        // Reopening runs the migration machinery again against an
        // already-current schema; it must be a no-op and preserve state.
        let mut store = open(&dir);
        let again = store.register_project(p("C:/repos/lore"), "lore").unwrap();
        assert_eq!(again, project, "register_project is idempotent on root");
        assert_eq!(store.list_projects().unwrap().len(), 1);
        assert_eq!(store.generation().unwrap(), 1);
        assert_eq!(store.bump_generation().unwrap(), 2);
    }
    // And a third open, to prove nothing accumulates.
    let store = open(&dir);
    assert_eq!(store.list_projects().unwrap().len(), 1);
    assert_eq!(store.generation().unwrap(), 2);
}

#[test]
fn register_project_updates_name_but_not_id() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let a = store.register_project(p("C:/repos/x"), "old").unwrap();
    let b = store.register_project(p("C:/repos/x"), "new").unwrap();
    assert_eq!(a, b);
    let projects = store.list_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "new");
}

// ---------------------------------------------------------------------------
// replace_file_chunks
// ---------------------------------------------------------------------------

#[test]
fn replace_file_chunks_evicts_stale_and_preserves_unchanged_embeddings() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let proj = store.register_project(p("C:/repos/x"), "x").unwrap();

    let stable = code_chunk(
        "src/lib.rs",
        "stable_fn",
        "fn stable_fn() { let quokka = 1; }",
        "rust",
    );
    let doomed = code_chunk(
        "src/lib.rs",
        "doomed_fn",
        "fn doomed_fn() { let platypus = 2; }",
        "rust",
    );

    let w = store
        .replace_file_chunks(
            proj,
            p("src/lib.rs"),
            "hash1",
            &[stable.clone(), doomed.clone()],
        )
        .unwrap();
    assert_eq!((w.inserted, w.kept, w.deleted), (2, 0, 0));
    assert_eq!(
        store.file_hash(proj, p("src/lib.rs")).unwrap().as_deref(),
        Some("hash1")
    );

    // Embed both.
    let stored = store
        .upsert_embeddings(&[
            NewEmbedding {
                project: proj,
                chunk_id: stable.id.clone(),
                vector: vec![1.0, 0.0, 0.0],
            },
            NewEmbedding {
                project: proj,
                chunk_id: doomed.id.clone(),
                vector: vec![0.0, 1.0, 0.0],
            },
        ])
        .unwrap();
    assert_eq!(stored, 2);
    assert!(store.chunks_missing_embeddings(0, 10).unwrap().is_empty());

    // Re-index: `stable` is byte-identical (same id), `doomed` is replaced by
    // a new chunk, and `stable` moved down the file (span changed only).
    let mut moved = stable.clone();
    moved.byte_start = 100;
    moved.byte_end = 100 + moved.text.len() as u32;
    assert_eq!(moved.id, stable.id, "span is not part of chunk identity");
    let fresh = code_chunk(
        "src/lib.rs",
        "fresh_fn",
        "fn fresh_fn() { let armadillo = 3; }",
        "rust",
    );

    let w = store
        .replace_file_chunks(proj, p("src/lib.rs"), "hash2", &[moved, fresh.clone()])
        .unwrap();
    assert_eq!((w.inserted, w.kept, w.deleted), (1, 1, 1));
    assert_eq!(
        store.file_hash(proj, p("src/lib.rs")).unwrap().as_deref(),
        Some("hash2")
    );

    // The stale chunk's text is gone from the lexical index.
    let f = SearchFilter::default();
    assert!(
        store.lexical_search("platypus", &f, 10).unwrap().is_empty(),
        "stale chunk text must not remain findable"
    );
    // The surviving and new chunks are findable.
    assert_eq!(store.lexical_search("quokka", &f, 10).unwrap().len(), 1);
    assert_eq!(store.lexical_search("armadillo", &f, 10).unwrap().len(), 1);

    // The unchanged chunk kept its embedding; the new one has none; the
    // deleted one's vector is gone.
    let missing = store.chunks_missing_embeddings(0, 10).unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].chunk.id, fresh.id);

    let st = store.status().unwrap();
    assert_eq!(st.projects[0].files, 1);
    assert_eq!(st.projects[0].chunks, 2);
    assert_eq!(
        st.projects[0].embedded_chunks, 1,
        "doomed chunk's vector cascaded away, stable chunk's survived"
    );

    // And the surviving vector is still the one we stored.
    let hits = store.vector_search(&[1.0, 0.0, 0.0], &f, 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk.id, stable.id);
    assert!((hits[0].score - 1.0).abs() < 1e-5);
    // Span updates on the kept chunk were applied.
    assert_eq!(hits[0].chunk.byte_start, 100);
}

#[test]
fn replace_file_chunks_rejects_chunks_from_another_path() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let proj = store.register_project(p("C:/repos/x"), "x").unwrap();
    let stray = code_chunk("src/other.rs", "f", "fn f() {}", "rust");
    let err = store
        .replace_file_chunks(proj, p("src/lib.rs"), "h", &[stray])
        .unwrap_err();
    assert!(
        matches!(err, StoreError::PathMismatch { .. }),
        "got {err:?}"
    );
}

#[test]
fn get_chunk_and_get_file_chunks_are_ordered() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let proj = store.register_project(p("C:/repos/x"), "x").unwrap();

    let mut first = code_chunk("src/lib.rs", "a", "fn a() {}", "rust");
    first.byte_start = 0;
    let mut second = code_chunk("src/lib.rs", "b", "fn b() {}", "rust");
    second.byte_start = 50;
    let mut third = code_chunk("src/lib.rs", "c", "fn c() {}", "rust");
    third.byte_start = 200;

    // Inserted out of order on purpose.
    store
        .replace_file_chunks(
            proj,
            p("src/lib.rs"),
            "h",
            &[third.clone(), first.clone(), second.clone()],
        )
        .unwrap();

    let chunks = store.get_file_chunks(proj, p("src/lib.rs")).unwrap();
    assert_eq!(
        chunks.iter().map(|c| c.byte_start).collect::<Vec<_>>(),
        vec![0, 50, 200]
    );

    let one = store.get_chunk(proj, &second.id).unwrap().unwrap();
    assert_eq!(one, second);
    assert!(
        store
            .get_chunk(proj, &lore::types::ChunkId("nope".into()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn remove_file_cascades_chunks_fts_and_embeddings() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let proj = store.register_project(p("C:/repos/x"), "x").unwrap();

    let c = code_chunk("src/lib.rs", "f", "fn f() { let capybara = 1; }", "rust");
    store
        .replace_file_chunks(proj, p("src/lib.rs"), "h", std::slice::from_ref(&c))
        .unwrap();
    store
        .upsert_embeddings(&[NewEmbedding {
            project: proj,
            chunk_id: c.id.clone(),
            vector: vec![1.0, 0.0],
        }])
        .unwrap();

    assert!(store.remove_file(proj, p("src/lib.rs")).unwrap());
    assert!(
        !store.remove_file(proj, p("src/lib.rs")).unwrap(),
        "second removal is a no-op"
    );

    let f = SearchFilter::default();
    assert!(store.lexical_search("capybara", &f, 10).unwrap().is_empty());
    assert!(store.vector_search(&[1.0, 0.0], &f, 10).unwrap().is_empty());
    assert!(store.get_chunk(proj, &c.id).unwrap().is_none());
    assert!(
        store
            .get_file_chunks(proj, p("src/lib.rs"))
            .unwrap()
            .is_empty()
    );
    assert!(store.file_hash(proj, p("src/lib.rs")).unwrap().is_none());
    assert!(store.list_files(proj).unwrap().is_empty());

    let st = store.status().unwrap();
    assert_eq!(
        (
            st.projects[0].files,
            st.projects[0].chunks,
            st.projects[0].embedded_chunks
        ),
        (0, 0, 0)
    );
}

// ---------------------------------------------------------------------------
// lexical search
// ---------------------------------------------------------------------------

fn seeded_lexical_store(dir: &TempDir) -> (Store, i64, i64) {
    let mut store = open(dir);
    let a = store.register_project(p("C:/repos/a"), "a").unwrap();
    let b = store.register_project(p("C:/repos/b"), "b").unwrap();

    store
        .replace_file_chunks(a, p("src/board.cs"), "h", &[code_chunk(
            "src/board.cs",
            "Lexomancy.Board.Update",
            "void Update() { // the wombat threshold governs settling\n var content_hash = 1; }",
            "csharp",
        )])
        .unwrap();
    store
        .replace_file_chunks(
            a,
            p("src/other.rs"),
            "h",
            &[code_chunk(
                "src/other.rs",
                "wombat_helper",
                "fn wombat_helper() { /* wombat lives here */ }",
                "rust",
            )],
        )
        .unwrap();
    store
        .replace_file_chunks(
            a,
            p("design/1_Architecture/1.1_Overview.md"),
            "h",
            &[section_chunk(
                "design/1_Architecture/1.1_Overview.md",
                "Storage",
                "The wombat store keeps metadata and vectors in one database.",
                Some(DesignStatus::Decided),
            )],
        )
        .unwrap();
    store
        .replace_file_chunks(
            a,
            p("design/9_Scratch/notes.md"),
            "h",
            &[section_chunk(
                "design/9_Scratch/notes.md",
                "Scratch",
                "Maybe the wombat idea is wrong.",
                Some(DesignStatus::Deprecated),
            )],
        )
        .unwrap();
    store
        .replace_file_chunks(
            a,
            p("README.md"),
            "h",
            &[section_chunk(
                "README.md",
                "Intro",
                "An unclassified wombat mention.",
                None,
            )],
        )
        .unwrap();
    store
        .replace_file_chunks(
            b,
            p("src/lib.rs"),
            "h",
            &[code_chunk(
                "src/lib.rs",
                "wombat_in_b",
                "fn wombat_in_b() { /* wombat lives here */ }",
                "rust",
            )],
        )
        .unwrap();
    (store, a, b)
}

#[test]
fn lexical_search_matches_body_text_not_just_names() {
    let dir = TempDir::new().unwrap();
    let (store, a, _) = seeded_lexical_store(&dir);
    // "threshold" appears only inside a comment in the body of a code chunk.
    let hits = store
        .lexical_search("threshold", &SearchFilter::project(a), 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk.path, "src/board.cs");
    assert!(hits[0].score.is_finite());
}

#[test]
fn lexical_search_keeps_snake_case_identifiers_whole() {
    let dir = TempDir::new().unwrap();
    let (store, a, _) = seeded_lexical_store(&dir);
    let f = SearchFilter::project(a);
    assert_eq!(
        store.lexical_search("content_hash", &f, 10).unwrap().len(),
        1
    );
    // Prefix search still reaches inside the compound token.
    assert_eq!(
        store.lexical_search("content_ha*", &f, 10).unwrap().len(),
        1
    );
    // The documented cost of `tokenchars '_'`: a bare component of a
    // snake_case identifier is NOT a token, so it does not match on its own.
    // `content` here only fails to match because the sole occurrence is
    // inside `content_hash`.
    assert!(store.lexical_search("content", &f, 10).unwrap().is_empty());
    assert_eq!(store.lexical_search("content*", &f, 10).unwrap().len(), 1);
    // A dotted symbol path, by contrast, IS split — `.` stays a separator —
    // so its components are reachable via the indexed anchor.
    assert!(
        !store
            .lexical_search("Lexomancy", &f, 10)
            .unwrap()
            .is_empty()
    );
    assert!(!store.lexical_search("Update", &f, 10).unwrap().is_empty());
}

#[test]
fn lexical_search_filters() {
    let dir = TempDir::new().unwrap();
    let (store, a, b) = seeded_lexical_store(&dir);

    // Unfiltered: every seeded chunk mentions "wombat".
    assert_eq!(
        store
            .lexical_search("wombat", &SearchFilter::default(), 20)
            .unwrap()
            .len(),
        6
    );

    // Project.
    let in_a = store
        .lexical_search("wombat", &SearchFilter::project(a), 20)
        .unwrap();
    assert_eq!(in_a.len(), 5);
    assert!(in_a.iter().all(|h| h.project == a));
    assert_eq!(
        store
            .lexical_search("wombat", &SearchFilter::project(b), 20)
            .unwrap()
            .len(),
        1
    );

    // Language.
    let rust_only = SearchFilter {
        language: Some("rust".into()),
        ..SearchFilter::default()
    };
    let hits = store.lexical_search("wombat", &rust_only, 20).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter()
            .all(|h| h.chunk.language.as_deref() == Some("rust"))
    );

    // Path prefix.
    let design_only = SearchFilter {
        project: Some(a),
        path_prefix: Some("design/".into()),
        ..SearchFilter::default()
    };
    let hits = store.lexical_search("wombat", &design_only, 20).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter()
            .all(|h| h.chunk.path.as_str().starts_with("design/"))
    );

    // Status allowlist: decided only.
    let decided = SearchFilter {
        statuses: Some(vec![StatusFilter::Status(DesignStatus::Decided)]),
        ..SearchFilter::default()
    };
    let hits = store.lexical_search("wombat", &decided, 20).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk.path, "design/1_Architecture/1.1_Overview.md");

    // Status allowlist including unclassified (which is where all code lives).
    let decided_or_unclassified = SearchFilter {
        statuses: Some(vec![
            StatusFilter::Status(DesignStatus::Decided),
            StatusFilter::Unclassified,
        ]),
        ..SearchFilter::default()
    };
    assert_eq!(
        store
            .lexical_search("wombat", &decided_or_unclassified, 20)
            .unwrap()
            .len(),
        5
    );

    // An empty allowlist means nothing qualifies.
    let nothing = SearchFilter {
        statuses: Some(vec![]),
        ..SearchFilter::default()
    };
    assert!(
        store
            .lexical_search("wombat", &nothing, 20)
            .unwrap()
            .is_empty()
    );

    // Authority floor drops deprecated material without enumerating statuses.
    let not_deprecated = SearchFilter {
        min_authority: Some(1),
        ..SearchFilter::default()
    };
    let hits = store.lexical_search("wombat", &not_deprecated, 20).unwrap();
    assert_eq!(hits.len(), 5);
    assert!(
        hits.iter()
            .all(|h| h.chunk.path != "design/9_Scratch/notes.md")
    );

    // Limit is honoured.
    assert_eq!(
        store
            .lexical_search("wombat", &SearchFilter::default(), 2)
            .unwrap()
            .len(),
        2
    );
    assert!(
        store
            .lexical_search("wombat", &SearchFilter::default(), 0)
            .unwrap()
            .is_empty()
    );
}

/// Path prefixes must survive the two ways a real Windows vault spells a
/// directory: non-ASCII characters (SQLite `substr` counts characters, Rust
/// `len()` counts bytes) and a different case (`assets/scripts/` is the same
/// directory as `Assets/Scripts/`). Both search arms share `filter_sql`, so
/// both are asserted.
#[test]
fn path_prefix_filter_handles_non_ascii_and_windows_case() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let a = store.register_project(p("C:/repos/a"), "a").unwrap();

    let accented = code_chunk(
        "données/parser.cs",
        "Parser.Run",
        "void Run() { /* wombat */ }",
        "csharp",
    );
    // Same characters up to the separator: a prefix must not leak into it.
    let sibling = code_chunk(
        "données2/parser.cs",
        "Sibling.Run",
        "void Run() { /* wombat sibling */ }",
        "csharp",
    );
    let mixed_case = code_chunk(
        "Assets/Scripts/Foo.cs",
        "Foo.Run",
        "void Run() { /* wombat asset */ }",
        "csharp",
    );
    for chunk in [&accented, &sibling, &mixed_case] {
        store
            .replace_file_chunks(a, chunk.path.as_path(), "h", std::slice::from_ref(chunk))
            .unwrap();
    }
    store
        .upsert_embeddings(&[
            NewEmbedding {
                project: a,
                chunk_id: accented.id.clone(),
                vector: vec![1.0, 0.0],
            },
            NewEmbedding {
                project: a,
                chunk_id: sibling.id.clone(),
                vector: vec![1.0, 0.0],
            },
            NewEmbedding {
                project: a,
                chunk_id: mixed_case.id.clone(),
                vector: vec![1.0, 0.0],
            },
        ])
        .unwrap();

    let with_prefix = |prefix: &str| SearchFilter {
        project: Some(a),
        path_prefix: Some(prefix.into()),
        ..SearchFilter::default()
    };
    let paths_of = |hits: Vec<lore::store::SearchHit>| {
        let mut out: Vec<String> = hits
            .into_iter()
            .map(|h| h.chunk.path.as_str().to_string())
            .collect();
        out.sort();
        out
    };

    // Non-ASCII directory: the accented file, and only it.
    let accented_only = with_prefix("données/");
    assert_eq!(
        paths_of(store.lexical_search("wombat", &accented_only, 10).unwrap()),
        ["données/parser.cs"]
    );
    assert_eq!(
        paths_of(
            store
                .vector_search(&[1.0, 0.0], &accented_only, 10)
                .unwrap()
        ),
        ["données/parser.cs"]
    );

    // Exact-case ASCII prefix works everywhere.
    let assets = with_prefix("Assets/Scripts/");
    assert_eq!(
        paths_of(store.lexical_search("wombat", &assets, 10).unwrap()),
        ["Assets/Scripts/Foo.cs"]
    );
    assert_eq!(
        paths_of(store.vector_search(&[1.0, 0.0], &assets, 10).unwrap()),
        ["Assets/Scripts/Foo.cs"]
    );

    // Case folding follows `daemon::paths`: ASCII-insensitive on Windows,
    // exact elsewhere, because elsewhere the two really are different files.
    let lowercased = with_prefix("assets/scripts/");
    let lexical = paths_of(store.lexical_search("wombat", &lowercased, 10).unwrap());
    let vector = paths_of(store.vector_search(&[1.0, 0.0], &lowercased, 10).unwrap());
    if cfg!(windows) {
        assert_eq!(lexical, ["Assets/Scripts/Foo.cs"]);
        assert_eq!(vector, ["Assets/Scripts/Foo.cs"]);
    } else {
        assert!(lexical.is_empty(), "{lexical:?}");
        assert!(vector.is_empty(), "{vector:?}");
    }

    // A shorter prefix still stops at what it actually spells.
    assert_eq!(
        paths_of(
            store
                .lexical_search("wombat", &with_prefix("données"), 10)
                .unwrap()
        ),
        ["données/parser.cs", "données2/parser.cs"]
    );
}

#[test]
fn lexical_search_survives_hostile_queries() {
    let dir = TempDir::new().unwrap();
    let (store, _, _) = seeded_lexical_store(&dir);
    let f = SearchFilter::default();
    for hostile in [
        "a AND (",
        "\"unterminated",
        "*",
        "**",
        "NEAR(",
        ")))",
        "^",
        "wombat OR",
        "wombat AND (threshold",
        "col:wombat",
        "-wombat",
        "'; DROP TABLE chunks; --",
        "{}[]<>",
        "\u{1F600}",
        "",
        "   ",
        &"x ".repeat(500),
    ] {
        let got = store.lexical_search(hostile, &f, 5);
        assert!(got.is_ok(), "query {hostile:?} errored: {:?}", got.err());
    }
    // Nothing was destroyed along the way.
    assert_eq!(store.lexical_search("wombat", &f, 20).unwrap().len(), 6);
}

// ---------------------------------------------------------------------------
// vector search
// ---------------------------------------------------------------------------

#[test]
fn vector_search_ranks_by_cosine_and_respects_filters() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let a = store.register_project(p("C:/repos/a"), "a").unwrap();
    let b = store.register_project(p("C:/repos/b"), "b").unwrap();

    let near = code_chunk("src/near.rs", "near", "fn near() {}", "rust");
    let mid = code_chunk("src/mid.rs", "mid", "fn mid() {}", "rust");
    let far = code_chunk("src/far.md", "far", "fn far() {}", "markdown");
    let other = code_chunk("src/other.rs", "other", "fn other() {}", "rust");

    for c in [&near, &mid, &far] {
        store
            .replace_file_chunks(a, c.path.as_path(), "h", std::slice::from_ref(c))
            .unwrap();
    }
    store
        .replace_file_chunks(b, other.path.as_path(), "h", std::slice::from_ref(&other))
        .unwrap();

    store
        .upsert_embeddings(&[
            NewEmbedding {
                project: a,
                chunk_id: near.id.clone(),
                // Deliberately un-normalized: the store normalizes on write,
                // so magnitude must not affect ranking.
                vector: vec![7.0, 0.0, 0.0],
            },
            NewEmbedding {
                project: a,
                chunk_id: mid.id.clone(),
                vector: vec![0.8, 0.6, 0.0],
            },
            NewEmbedding {
                project: a,
                chunk_id: far.id.clone(),
                vector: vec![0.0, 1.0, 0.0],
            },
            NewEmbedding {
                project: b,
                chunk_id: other.id.clone(),
                vector: vec![1.0, 0.0, 0.0],
            },
        ])
        .unwrap();

    let hits = store
        .vector_search(&[1.0, 0.0, 0.0], &SearchFilter::project(a), 10)
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.chunk.id.clone()).collect::<Vec<_>>(),
        vec![near.id.clone(), mid.id.clone(), far.id.clone()]
    );
    assert!(
        (hits[0].score - 1.0).abs() < 1e-5,
        "score {}",
        hits[0].score
    );
    assert!(
        (hits[1].score - 0.8).abs() < 1e-5,
        "score {}",
        hits[1].score
    );
    assert!(hits[2].score.abs() < 1e-5, "score {}", hits[2].score);

    // top-k truncation keeps the best.
    let top1 = store
        .vector_search(&[1.0, 0.0, 0.0], &SearchFilter::project(a), 1)
        .unwrap();
    assert_eq!(top1.len(), 1);
    assert_eq!(top1[0].chunk.id, near.id);

    // Filters apply before scoring.
    let rust_only = SearchFilter {
        project: Some(a),
        language: Some("rust".into()),
        ..SearchFilter::default()
    };
    let hits = store
        .vector_search(&[1.0, 0.0, 0.0], &rust_only, 10)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter()
            .all(|h| h.chunk.language.as_deref() == Some("rust"))
    );

    let in_b = store
        .vector_search(&[1.0, 0.0, 0.0], &SearchFilter::project(b), 10)
        .unwrap();
    assert_eq!(in_b.len(), 1);
    assert_eq!(in_b[0].chunk.id, other.id);

    // An unusable query vector is an error, not a panic or silent empty.
    assert!(matches!(
        store.vector_search(&[0.0, 0.0, 0.0], &SearchFilter::default(), 5),
        Err(StoreError::InvalidQueryVector)
    ));
    // Dimension mismatch is loud (fingerprint discipline is the caller's job).
    assert!(matches!(
        store.vector_search(&[1.0, 0.0], &SearchFilter::project(a), 5),
        Err(StoreError::DimensionMismatch {
            query: 2,
            stored: 3
        })
    ));
}

#[test]
fn upsert_embeddings_skips_vanished_chunks_and_rejects_bad_vectors() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let a = store.register_project(p("C:/repos/a"), "a").unwrap();
    let c = code_chunk("src/lib.rs", "f", "fn f() {}", "rust");
    store
        .replace_file_chunks(a, p("src/lib.rs"), "h", std::slice::from_ref(&c))
        .unwrap();

    let stored = store
        .upsert_embeddings(&[
            NewEmbedding {
                project: a,
                chunk_id: c.id.clone(),
                vector: vec![1.0, 0.0],
            },
            NewEmbedding {
                project: a,
                chunk_id: lore::types::ChunkId("gone-while-embedding".into()),
                vector: vec![0.0, 1.0],
            },
        ])
        .unwrap();
    assert_eq!(
        stored, 1,
        "a chunk deleted mid-flight is skipped, not fatal"
    );

    // Re-embedding the same chunk replaces the vector.
    store
        .upsert_embeddings(&[NewEmbedding {
            project: a,
            chunk_id: c.id.clone(),
            vector: vec![0.0, 1.0],
        }])
        .unwrap();
    let hits = store
        .vector_search(&[0.0, 1.0], &SearchFilter::default(), 5)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!((hits[0].score - 1.0).abs() < 1e-5);

    let err = store
        .upsert_embeddings(&[NewEmbedding {
            project: a,
            chunk_id: c.id.clone(),
            vector: vec![0.0, 0.0],
        }])
        .unwrap_err();
    assert!(
        matches!(err, StoreError::InvalidVector { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// fingerprint
// ---------------------------------------------------------------------------

#[test]
fn embedding_fingerprint_set_get_clear() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    assert!(store.embedding_fingerprint().unwrap().is_none());

    let fp = EmbeddingFingerprint {
        model_id: "jina-code-embeddings-1.5b".into(),
        dimensions: 1536,
        query_prefix: "query: ".into(),
        document_prefix: "passage: ".into(),
        normalization: "l2".into(),
    };
    store.set_embedding_fingerprint(&fp).unwrap();
    assert_eq!(store.embedding_fingerprint().unwrap().as_ref(), Some(&fp));

    // Survives reopen.
    drop(store);
    let mut store = open(&dir);
    assert_eq!(store.embedding_fingerprint().unwrap().as_ref(), Some(&fp));

    // A model swap: caller clears vectors and writes the new fingerprint. The
    // store never does this on its own.
    let a = store.register_project(p("C:/repos/a"), "a").unwrap();
    let c = code_chunk("src/lib.rs", "f", "fn f() {}", "rust");
    store
        .replace_file_chunks(a, p("src/lib.rs"), "h", std::slice::from_ref(&c))
        .unwrap();
    store
        .upsert_embeddings(&[NewEmbedding {
            project: a,
            chunk_id: c.id.clone(),
            vector: vec![1.0, 0.0],
        }])
        .unwrap();
    assert_eq!(store.status().unwrap().projects[0].embedded_chunks, 1);

    assert_eq!(store.clear_all_embeddings().unwrap(), 1);
    assert_eq!(store.status().unwrap().projects[0].embedded_chunks, 0);
    assert_eq!(store.chunks_missing_embeddings(0, 10).unwrap().len(), 1);
    // Chunks themselves are untouched.
    assert!(store.get_chunk(a, &c.id).unwrap().is_some());
    // Fingerprint is left for the caller to overwrite deliberately.
    assert_eq!(store.embedding_fingerprint().unwrap().as_ref(), Some(&fp));

    let fp2 = EmbeddingFingerprint {
        model_id: "other-model".into(),
        dimensions: 768,
        ..fp.clone()
    };
    store.set_embedding_fingerprint(&fp2).unwrap();
    assert_eq!(store.embedding_fingerprint().unwrap(), Some(fp2));
}

// ---------------------------------------------------------------------------
// vault metadata round-trip
// ---------------------------------------------------------------------------

#[test]
fn vault_metadata_round_trips_through_storage() {
    let dir = TempDir::new().unwrap();
    let mut store = open(&dir);
    let a = store.register_project(p("C:/repos/a"), "a").unwrap();
    let mut c = section_chunk(
        "design/x.md",
        "Heading",
        "Body referencing D-0004.",
        Some(DesignStatus::Leaning),
    );
    c.vault.as_mut().unwrap().body_decision_refs = vec!["D-0004".into()];
    c.id = Chunk::derive_id(&c.path, &c.kind, &c.text);

    store
        .replace_file_chunks(a, p("design/x.md"), "h", &[c.clone()])
        .unwrap();
    let got = store.get_chunk(a, &c.id).unwrap().unwrap();
    assert_eq!(got, c, "every Chunk field round-trips unchanged");
}
