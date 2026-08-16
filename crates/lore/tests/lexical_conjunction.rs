//! `Store::lexical_search` conjunction relaxation (issue #4).
//!
//! FTS5 reads juxtaposed terms as AND, so a prose question only matched a
//! chunk containing *every* word — which is usually no chunk at all. The
//! lexical arm then contributed nothing to fusion, silently, on exactly the
//! query shape the MCP `search` description tells agents to use.
//!
//! What is asserted here is the *contract*, not the mechanism: a precise
//! multi-term query still resolves conjunctively and is not diluted by
//! partial matches, and a query no single chunk satisfies comes back ranked
//! instead of empty.

use camino::{Utf8Path, Utf8PathBuf};
use lore::store::{ProjectId, SearchFilter, Store};
use lore::types::{Chunk, ChunkKind};
use tempfile::TempDir;

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

    fn write(&mut self, path: &str, text: &str) {
        let path_buf = Utf8PathBuf::from(path);
        let kind = ChunkKind::Section {
            heading_path: vec!["Doc".to_string()],
            window: None,
        };
        let chunk = Chunk {
            id: Chunk::derive_id(&path_buf, &kind, text),
            path: path_buf.clone(),
            kind,
            language: Some("markdown".to_string()),
            byte_start: 0,
            byte_end: text.len() as u32,
            line_start: 1,
            line_end: 1 + text.lines().count() as u32,
            text: text.to_string(),
            vault: None,
        };
        self.store
            .replace_file_chunks(self.project, Utf8Path::new(path), "hash", &[chunk])
            .expect("replace file chunks");
    }

    fn search(&mut self, query: &str) -> Vec<String> {
        self.store
            .lexical_search(query, &SearchFilter::default(), 50)
            .expect("lexical search succeeds")
            .into_iter()
            .map(|hit| hit.chunk.path.to_string())
            .collect()
    }
}

/// The regression itself: every term exists in the corpus, no chunk holds all
/// of them, and the arm used to return nothing at all.
#[test]
fn a_prose_query_no_single_chunk_satisfies_still_returns_ranked_hits() {
    let mut corpus = Corpus::new();
    corpus.write("a.md", "the daemon owns a single instance of the index");
    corpus.write(
        "b.md",
        "handshake and heartbeat liveness on the loopback port",
    );
    corpus.write("c.md", "takeover happens when the previous owner is stale");
    corpus.write("unrelated.md", "chunking policy for markdown headings");

    let hits = corpus.search("single instance handshake heartbeat takeover");

    assert!(
        !hits.is_empty(),
        "no chunk contains all five terms, so the conjunction matches nothing; \
         the arm must relax rather than contribute an empty list to fusion"
    );
    for path in ["a.md", "b.md", "c.md"] {
        assert!(
            hits.contains(&path.to_string()),
            "{path} carries some of the query terms and should appear; got {hits:?}"
        );
    }
    assert!(
        !hits.contains(&"unrelated.md".to_string()),
        "a chunk sharing no query term must not be pulled in; got {hits:?}"
    );
}

/// Relaxation is a fallback, not the new default: when the conjunction *does*
/// match, partial matches must not be mixed in beside it.
#[test]
fn a_query_the_conjunction_satisfies_is_not_diluted_by_partial_matches() {
    let mut corpus = Corpus::new();
    corpus.write("both.md", "the handshake carries a heartbeat timestamp");
    corpus.write("one.md", "heartbeat only, no mention of the other word");
    corpus.write("other.md", "handshake only, nothing about liveness");

    let hits = corpus.search("handshake heartbeat");

    assert_eq!(
        hits,
        vec!["both.md".to_string()],
        "the conjunction matched, so the disjunctive retry must not run"
    );
}

/// A single term is identical under either operator; the retry must not fire
/// and turn "genuinely absent" into a second pointless statement.
#[test]
fn a_single_term_miss_stays_a_miss() {
    let mut corpus = Corpus::new();
    corpus.write("a.md", "the daemon owns the index");

    assert!(
        corpus.search("nonexistentterm").is_empty(),
        "one term that matches nothing has nothing to relax to"
    );
}

/// Sanitization runs before relaxation, so punctuation-only input still short
/// circuits instead of building an `OR` of zero terms.
#[test]
fn unusable_input_never_reaches_the_retry() {
    let mut corpus = Corpus::new();
    corpus.write("a.md", "the daemon owns the index");

    for query in [")))", "   ", "*"] {
        assert!(
            corpus.search(query).is_empty(),
            "{query:?} contains no usable term"
        );
    }
}
