//! Candidate acquisition and collapse, through `search::execute` against a
//! real store (S3#1, S3#4, S4#4, S4 top-10 #4/#6).
//!
//! These are deliberately *not* `fuse` tests. Everything the two reviews found
//! lives between `execute` and the store: how deep each arm is fetched, how
//! many rounds that takes, and what collapse does to a page once the rows are
//! in hand. Feeding `fuse` a materialized list assumes away exactly the step
//! under test.
//!
//! Every corpus therefore asserts its own preconditions against
//! `Store::lexical_search` / `Store::vector_search` before asserting the
//! ranking. If a corpus stops encoding "rank 51" or "the first fifty rows are
//! all one window family", the test says so instead of passing for the wrong
//! reason.

use camino::{Utf8Path, Utf8PathBuf};
use lore::chunk::{FileChunks, chunk_file};
use lore::daemon::search::{
    self, DEFAULT_LIMIT, LEXICAL_CANDIDATES, MAX_LIMIT, RRF_K, VECTOR_CANDIDATES,
};
use lore::store::{NewEmbedding, ProjectId, SearchFilter, Store};
use lore::types::{Chunk, ChunkId, ChunkKind, WindowFamily};
use lore_core::{SearchRequest, SearchResponse};
use tempfile::TempDir;

/// The one word every "on target" chunk contains and nothing else does.
const QUERY: &str = "quorum";

/// First-round depth. Both arms are asked for the same number, so one constant
/// describes the round; the assertions below break loudly if that stops being
/// true.
const FIRST_ROUND: usize = if LEXICAL_CANDIDATES > VECTOR_CANDIDATES {
    LEXICAL_CANDIDATES
} else {
    VECTOR_CANDIDATES
};

struct Corpus {
    _dir: TempDir,
    store: Store,
    project: ProjectId,
}

impl Corpus {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let mut store = Store::open(dir.path().join("lore.db")).expect("open store");
        let project = store
            .register_project(Utf8Path::new("C:/repos/demo"), "demo")
            .expect("register project");
        Self {
            _dir: dir,
            store,
            project,
        }
    }

    /// One file's worth of chunks. Callers group by path themselves because
    /// `replace_file_chunks` is per file and the grouping is part of what the
    /// BM25 arm sees.
    fn write(&mut self, path: &str, chunks: &[Chunk]) {
        self.store
            .replace_file_chunks(self.project, Utf8Path::new(path), "hash", chunks)
            .expect("replace file chunks");
    }

    fn embed(&mut self, chunk: &Chunk, cosine: f32) {
        self.store
            .upsert_embeddings(&[NewEmbedding {
                project: self.project,
                chunk_id: chunk.id.clone(),
                vector: unit_at(cosine),
            }])
            .expect("upsert embedding");
    }

    fn search(&mut self, limit: u32, query_vector: Option<&[f32]>) -> SearchResponse {
        let request = SearchRequest {
            query: QUERY.to_string(),
            limit: Some(limit),
            ..SearchRequest::default()
        };
        search::execute(&mut self.store, &request, query_vector).expect("search succeeds")
    }
}

/// A vector whose cosine against [`QUERY_VECTOR`] is exactly `cosine`. Both
/// sides are L2-normalized by the store, so the dot product *is* the cosine
/// and rank order in the vector arm is chosen, not hoped for.
fn unit_at(cosine: f32) -> Vec<f32> {
    vec![cosine, (1.0 - cosine * cosine).sqrt(), 0.0, 0.0]
}

const QUERY_VECTOR: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

fn section(path: &str, heading: &str, text: &str) -> Chunk {
    let path = Utf8PathBuf::from(path);
    let kind = ChunkKind::Section {
        heading_path: vec![heading.to_string()],
        window: None,
    };
    chunk_with(path, kind, text, Some("markdown"))
}

fn chunk_with(path: Utf8PathBuf, kind: ChunkKind, text: &str, language: Option<&str>) -> Chunk {
    Chunk {
        id: Chunk::derive_id(&path, &kind, text),
        path,
        kind,
        language: language.map(str::to_string),
        byte_start: 0,
        byte_end: text.len() as u32,
        line_start: 1,
        line_end: 1 + text.lines().count() as u32,
        text: text.to_string(),
        // No frontmatter anywhere: every chunk weighs AUTHORITY_NEUTRAL, so
        // the assertions below are pure RRF arithmetic.
        vault: None,
    }
}

/// Strongly on-query text: the term eight times in a short document, which is
/// what puts these rows at the top of a BM25 list.
fn dense(marker: usize) -> String {
    format!("{} lexical fixture row {marker}\n", [QUERY; 8].join(" "))
}

/// On-query but weak: the term once, buried in a long document. BM25 punishes
/// both the low term frequency and the length, so these rows sort below every
/// [`dense`] one.
fn sparse(marker: usize) -> String {
    let filler: Vec<String> = (0..200).map(|i| format!("filler{i}")).collect();
    format!(
        "{QUERY} appears once here in row {marker}. {}\n",
        filler.join(" ")
    )
}

/// Contains no query term at all, so BM25 cannot see it and the vector arm is
/// the only way in.
fn opaque(marker: usize) -> String {
    format!("semantically adjacent prose about consensus and agreement, row {marker}\n")
}

fn paths(response: &SearchResponse) -> Vec<&str> {
    response.results.iter().map(|r| r.path.as_str()).collect()
}

// ---------------------------------------------------------------------------
// S3#4 scenario A — cross-arm agreement below the first round wins the page
// ---------------------------------------------------------------------------

/// The reported failure, stated as arithmetic: a chunk both arms rank 51st
/// scores `2/(60+51) = 0.018018`, and the best either arm can do alone is
/// `1/(60+1) = 0.016393`. It is fused rank 1, and a fixed 50-per-arm pull can
/// never see it.
///
/// The corpus is built so the two arms are disjoint above rank 51: the fifty
/// lexical rows carry no vector, the fifty vector rows carry no query term.
/// The shared chunk is last in both.
#[test]
fn rank_51_cross_arm_agreement_can_win() {
    let mut corpus = Corpus::new();

    // Fifty lexical-only rows: dense on the query, no embedding.
    for i in 0..FIRST_ROUND {
        let path = format!("lex/l{i:03}.md");
        let chunk = section(&path, "Lexical", &dense(i));
        corpus.write(&path, std::slice::from_ref(&chunk));
    }
    // Fifty vector-only rows: invisible to BM25, cosine 0.99 downwards so
    // their order in the vector arm is fixed and all of them beat the shared
    // chunk.
    for i in 0..FIRST_ROUND {
        let path = format!("vec/v{i:03}.md");
        let chunk = section(&path, "Vector", &opaque(i));
        corpus.write(&path, std::slice::from_ref(&chunk));
        corpus.embed(&chunk, 0.99 - 0.001 * i as f32);
    }
    // The shared chunk: worst of the lexical matches and worst of the vectors.
    let shared = section("shared/agreement.md", "Agreement", &sparse(999));
    corpus.write("shared/agreement.md", std::slice::from_ref(&shared));
    corpus.embed(&shared, 0.5);

    // --- preconditions, asserted against the store, not assumed -------------
    let filter = SearchFilter::default();
    let lexical = corpus
        .store
        .lexical_search(QUERY, &filter, FIRST_ROUND + 10)
        .expect("lexical arm");
    assert_eq!(
        lexical.len(),
        FIRST_ROUND + 1,
        "exactly one lexical match beyond the first round, or there is no rank 51"
    );
    assert_eq!(
        lexical[FIRST_ROUND].chunk.id,
        shared.id,
        "the shared chunk must sit at lexical rank {}",
        FIRST_ROUND + 1
    );
    let vector = corpus
        .store
        .vector_search(&QUERY_VECTOR, &filter, FIRST_ROUND + 10)
        .expect("vector arm");
    assert_eq!(vector.len(), FIRST_ROUND + 1);
    assert_eq!(
        vector[FIRST_ROUND].chunk.id,
        shared.id,
        "the shared chunk must sit at vector rank {}",
        FIRST_ROUND + 1
    );
    // The two arms genuinely disagree above it: nothing is in both top-50s.
    let top_lex: Vec<&ChunkId> = lexical[..FIRST_ROUND].iter().map(|h| &h.chunk.id).collect();
    assert!(
        vector[..FIRST_ROUND]
            .iter()
            .all(|h| !top_lex.contains(&&h.chunk.id)),
        "the first rounds of the two arms must be disjoint"
    );
    // And a single fixed-depth round would miss it outright — which is what
    // makes a second acquisition round load-bearing rather than incidental.
    assert_eq!(
        corpus
            .store
            .lexical_search(QUERY, &filter, FIRST_ROUND)
            .expect("lexical arm")
            .len(),
        FIRST_ROUND,
        "the lexical arm returns a full first round, so it is still open"
    );

    // --- the ranking --------------------------------------------------------
    let response = corpus.search(DEFAULT_LIMIT, Some(&QUERY_VECTOR));
    assert!(!response.lexical_only, "both arms ran");
    assert_eq!(response.results.len(), DEFAULT_LIMIT as usize);

    let rank_51 = RRF_K + (FIRST_ROUND + 1) as f64;
    let expected_shared = 2.0 / rank_51;
    let expected_singleton = 1.0 / (RRF_K + 1.0);
    assert!(
        expected_shared > expected_singleton,
        "the premise itself: {expected_shared} must beat {expected_singleton}"
    );

    let winner = &response.results[0];
    assert_eq!(
        winner.chunk_id,
        shared.id.0,
        "agreement at rank {} is fused rank 1; got {:?}",
        FIRST_ROUND + 1,
        paths(&response)
    );
    assert!(
        (winner.score - expected_shared).abs() < 1e-6,
        "expected {expected_shared}, got {}",
        winner.score
    );
    // Behind it, the two arms' rank-1 singletons, tied and split by chunk id.
    assert!((response.results[1].score - expected_singleton).abs() < 1e-6);
    assert!((response.results[2].score - expected_singleton).abs() < 1e-6);
    assert!(response.results[1].chunk_id < response.results[2].chunk_id);
}

// ---------------------------------------------------------------------------
// S3#4 scenario B — collapse must not strand a page
// ---------------------------------------------------------------------------

/// One oversized symbol's windows fill the whole first round. Collapse keeps
/// one of them, and a fixed pull would hand back a page of one while twenty-odd
/// distinct chunks match below the family. Acquisition has to go back for them.
#[test]
fn window_collapse_refills_the_page() {
    let mut corpus = Corpus::new();
    let limit = DEFAULT_LIMIT as usize;

    // Sixty windows of one generated family, all dense on the query, so they
    // own the top of the BM25 list.
    let big = Utf8PathBuf::from("Assets/Scripts/Board.cs");
    let windows: Vec<Chunk> = (0..60)
        .map(|i| {
            chunk_with(
                big.clone(),
                ChunkKind::Code {
                    symbol_kind: "method_declaration".into(),
                    symbol_path: format!("Board.Update#w{i}"),
                    window: Some(WindowFamily {
                        family: 0,
                        index: i,
                    }),
                },
                &dense(i as usize),
                Some("csharp"),
            )
        })
        .collect();
    corpus.write(big.as_str(), &windows);

    // Twenty-five distinct places that also match, each weaker than every
    // window: eligible results sitting below the family.
    let others: Vec<String> = (0..25).map(|i| format!("docs/other{i:02}.md")).collect();
    for (i, path) in others.iter().enumerate() {
        let chunk = section(path, "Other", &sparse(i));
        corpus.write(path, std::slice::from_ref(&chunk));
    }

    // --- preconditions ------------------------------------------------------
    let filter = SearchFilter::default();
    let first = corpus
        .store
        .lexical_search(QUERY, &filter, FIRST_ROUND)
        .expect("lexical arm");
    assert_eq!(
        first.len(),
        FIRST_ROUND,
        "the arm is full, so it stays open"
    );
    assert!(
        first.iter().all(|h| h.chunk.path == big),
        "the first round must be nothing but the window family, or this test \
         is not about refill"
    );
    assert_eq!(
        corpus
            .store
            .lexical_search(QUERY, &filter, 500)
            .expect("lexical arm")
            .len(),
        60 + 25,
        "everything below the family is reachable, just not in round one"
    );

    // --- the page -----------------------------------------------------------
    let response = corpus.search(DEFAULT_LIMIT, None);
    assert_eq!(
        response.results.len(),
        limit,
        "asked for {limit} with {} distinct places matching; got {:?}",
        1 + others.len(),
        paths(&response)
    );
    // The family speaks once, and its representative leads the page because
    // every window outscored every other chunk in the arm.
    let from_big: Vec<&str> = paths(&response)
        .into_iter()
        .filter(|p| *p == big.as_str())
        .collect();
    assert_eq!(from_big.len(), 1, "one result per window family");
    assert_eq!(response.results[0].path, big.as_str());
    // The other nineteen are nineteen different files.
    let mut rest: Vec<&str> = paths(&response)[1..].to_vec();
    rest.sort_unstable();
    rest.dedup();
    assert_eq!(rest.len(), limit - 1, "the refill is distinct content");
    assert!(rest.iter().all(|p| others.iter().any(|o| o == p)));
}

// ---------------------------------------------------------------------------
// S3#1 — equal anchors are ordinary content, through storage
// ---------------------------------------------------------------------------

/// Two oversized `Parse` overloads in one real C# file, chunked by the real
/// chunker, persisted, and searched. This is the end-to-end version of the
/// flagship failure: every string the two families carry is identical —
/// `Demo.Parser.Parse#w0`, `Demo.Parser.Parse#w1`, same file — and only the family
/// ordinal stamped at chunk time keeps them apart.
#[test]
fn independently_windowed_overloads_survive_collapse_end_to_end() {
    let path = Utf8Path::new("Assets/Scripts/Parser.cs");
    let source = overloaded_parser();
    let chunks = match chunk_file(path, source.as_bytes()) {
        FileChunks::Chunked(chunks) => chunks,
        FileChunks::Skipped(reason) => panic!("unexpected skip: {reason:?}"),
    };

    // --- precondition: the chunker really produced two families -------------
    let families: Vec<(String, WindowFamily)> = chunks
        .iter()
        .filter_map(|c| match &c.kind {
            ChunkKind::Code {
                symbol_path,
                window: Some(window),
                ..
            } if symbol_path.starts_with("Demo.Parser.Parse") => {
                Some((symbol_path.clone(), *window))
            }
            _ => None,
        })
        .collect();
    assert!(
        families.len() >= 4,
        "expected both overloads to be split into several windows, got {families:?}"
    );
    let mut ordinals: Vec<u32> = families.iter().map(|(_, w)| w.family).collect();
    ordinals.sort_unstable();
    ordinals.dedup();
    assert_eq!(
        ordinals.len(),
        2,
        "two independently windowed overloads are two families, got {families:?}"
    );
    // The strings genuinely collide across the families: both spell `#w0`.
    let first_windows: Vec<&str> = families
        .iter()
        .filter(|(_, w)| w.index == 0)
        .map(|(anchor, _)| anchor.as_str())
        .collect();
    assert_eq!(
        first_windows,
        ["Demo.Parser.Parse#w0", "Demo.Parser.Parse#w0"],
        "the anchor carries no family identity, which is the whole point"
    );

    let mut corpus = Corpus::new();
    corpus.write(path.as_str(), &chunks);

    let response = corpus.search(MAX_LIMIT, None);
    let parses: Vec<&lore_core::SearchResult> = response
        .results
        .iter()
        .filter(|r| {
            r.symbol_path
                .as_deref()
                .is_some_and(|s| s.starts_with("Demo.Parser.Parse"))
        })
        .collect();
    assert_eq!(
        parses.len(),
        2,
        "one result per overload — never one, never every window: {:?}",
        response
            .results
            .iter()
            .map(|r| r.symbol_path.as_deref())
            .collect::<Vec<_>>()
    );
    // And the two survivors are windows of *different* families, not two
    // windows of the same one.
    let mut kept: Vec<u32> = parses
        .iter()
        .map(|r| {
            corpus
                .store
                .get_chunk(corpus.project, &ChunkId(r.chunk_id.clone()))
                .expect("stored chunk")
                .expect("chunk exists")
                .kind
                .window_family()
                .expect("a window of a family")
                .family
        })
        .collect();
    kept.sort_unstable();
    assert_eq!(kept, ordinals, "one representative per family");
}

/// A C# class with two `Parse` overloads, each body well past
/// `MAX_CHUNK_BYTES`, and each body textually distinct so the `#d` collision
/// pass never fires.
fn overloaded_parser() -> String {
    let body = |tag: &str| -> String {
        (0..120)
            .map(|i| {
                format!("            total += {tag}Step{i}(input); // {QUERY} accumulates here\n")
            })
            .collect::<Vec<_>>()
            .join("")
    };
    format!(
        "namespace Demo\n{{\n    public class Parser\n    {{\n\
         \x20       public int Parse(string input)\n        {{\n            var total = 0;\n{}\
         \x20           return total;\n        }}\n\n\
         \x20       public int Parse(System.IO.Stream input)\n        {{\n            var total = 1;\n{}\
         \x20           return total;\n        }}\n    }}\n}}\n",
        body("Text"),
        body("Stream"),
    )
}

/// Rows written before `CHUNK_FORMAT_VERSION` 4 carry no `window` key at all.
/// They must read back as "not a window" and never collapse: a duplicate shown
/// is recoverable, a result hidden is not.
#[test]
fn legacy_rows_without_window_metadata_never_collapse() {
    // `window: None` is serialized by omission, so this row is byte-identical
    // to what version 3 wrote. Assert that rather than trusting it.
    let legacy_kind = ChunkKind::Code {
        symbol_kind: "method_declaration".into(),
        symbol_path: "Parser.Parse#w0".into(),
        window: None,
    };
    let json = serde_json::to_string(&legacy_kind).expect("serialize kind");
    assert!(
        !json.contains("window"),
        "a v3 row has no window key; this fixture is not one: {json}"
    );

    let path = Utf8PathBuf::from("Assets/Scripts/Legacy.cs");
    let mut corpus = Corpus::new();
    // Two rows that a v3 index would have collapsed into one: same anchor
    // shape, same file, different content.
    let rows: Vec<Chunk> = (0..2)
        .map(|i| {
            chunk_with(
                path.clone(),
                ChunkKind::Code {
                    symbol_kind: "method_declaration".into(),
                    symbol_path: format!("Parser.Parse#w{i}"),
                    window: None,
                },
                &dense(i),
                Some("csharp"),
            )
        })
        .collect();
    corpus.write(path.as_str(), &rows);

    let response = corpus.search(DEFAULT_LIMIT, None);
    let mut got: Vec<&str> = response
        .results
        .iter()
        .map(|r| r.chunk_id.as_str())
        .collect();
    got.sort_unstable();
    let mut want: Vec<&str> = rows.iter().map(|c| c.id.0.as_str()).collect();
    want.sort_unstable();
    assert_eq!(got, want, "legacy rows are shown, not folded");
}
