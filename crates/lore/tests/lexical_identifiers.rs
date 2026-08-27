//! Identifier-aware lexical search: a natural-language word reaches inside a
//! compound identifier, and spelling the identifier still finds it.
//!
//! The tokenizer keeps `_` as a token character so `content_hash` and
//! `_privateField` survive whole (`store::schema`). That made exact identifier
//! search work and natural-language identifier search impossible: nothing a
//! human types as "dispatch fanout" could ever reach a token spelled
//! `_dispatch_fanout`, and FTS5 has no infix search to fall back on. A
//! retrieval eval put the consequence in numbers — implementing source files
//! recalled at 0.34 against 0.71–1.00 for prose-y material.
//!
//! What is asserted here is the *contract*, not the mechanism: both spellings
//! reach the same chunk, neither reaches a chunk that has nothing to do with
//! the query, and prose is found by its own plain words exactly as before.

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

    /// One code chunk, anchored on a symbol name the way the tree-sitter
    /// chunkers anchor theirs.
    fn code(&mut self, path: &str, symbol: &str, text: &str) {
        self.write(
            path,
            ChunkKind::Code {
                symbol_kind: "function".to_string(),
                symbol_path: symbol.to_string(),
                window: None,
            },
            text,
        );
    }

    fn prose(&mut self, path: &str, heading: &str, text: &str) {
        self.write(
            path,
            ChunkKind::Section {
                heading_path: vec![heading.to_string()],
                window: None,
            },
            text,
        );
    }

    fn write(&mut self, path: &str, kind: ChunkKind, text: &str) {
        let path_buf = Utf8PathBuf::from(path);
        let chunk = Chunk {
            id: Chunk::derive_id(&path_buf, &kind, text),
            path: path_buf,
            kind,
            language: Some("rust".to_string()),
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

/// A corpus whose identifiers are the only place the query words appear: no
/// chunk contains "dispatch", "fanout", "concurrent" or "orchestration" as a
/// word of its own, so every hit below has to have come through the subword
/// column.
fn identifiers() -> Corpus {
    let mut corpus = Corpus::new();
    corpus.code(
        "src/messaging.rs",
        "Router.send",
        "fn _dispatch_fanout(&self, msg: Message) { self.sink.push(msg) }",
    );
    corpus.code(
        "src/runner.rs",
        "ConcurrentOrchestration",
        "struct ConcurrentOrchestration { workers: Vec<Worker> }",
    );
    corpus.prose(
        "docs/overview.md",
        "Overview",
        "The runner starts workers and waits for every one of them to finish.",
    );
    corpus
}

/// The regression itself, both spellings of it.
#[test]
fn natural_language_words_match_the_identifiers_they_were_taken_from() {
    let mut corpus = identifiers();

    assert_eq!(
        corpus.search("dispatch fanout"),
        vec!["src/messaging.rs".to_string()],
        "`_dispatch_fanout` is one token, so this used to match nothing at all"
    );
    assert_eq!(
        corpus.search("concurrent orchestration"),
        vec!["src/runner.rs".to_string()],
        "`ConcurrentOrchestration` is one opaque token to unicode61"
    );
    // A single component is enough; the query does not have to reconstruct the
    // whole name.
    assert_eq!(
        corpus.search("fanout"),
        vec!["src/messaging.rs".to_string()]
    );
}

/// The property `tokenchars '_'` was chosen for, and the one this change was
/// not allowed to spend.
#[test]
fn spelling_the_identifier_exactly_still_finds_it() {
    let mut corpus = identifiers();

    assert_eq!(
        corpus.search("_dispatch_fanout"),
        vec!["src/messaging.rs".to_string()]
    );
    assert_eq!(
        corpus.search("ConcurrentOrchestration"),
        vec!["src/runner.rs".to_string()]
    );
    // Prefix search still reaches into the front of the whole token.
    assert_eq!(
        corpus.search("_dispatch_fan*"),
        vec!["src/messaging.rs".to_string()]
    );
}

/// Reaching inside identifiers must not mean reaching everywhere: a word that
/// is in no identifier and no body still matches nothing.
#[test]
fn subwords_add_recall_without_matching_everything() {
    let mut corpus = identifiers();

    assert!(corpus.search("nonexistentterm").is_empty());
    // "orchestration" belongs to one chunk's identifier and nothing else in
    // the corpus, so the prose file must not be dragged in with it.
    assert_eq!(
        corpus.search("orchestration"),
        vec!["src/runner.rs".to_string()]
    );
}

/// Lore's original job. Prose has no compound identifiers, so its subword
/// column is empty and its ranking cannot have moved — asserted the way a user
/// would notice, by finding the document with its own plain words.
#[test]
fn prose_is_still_found_by_its_plain_words() {
    let mut corpus = identifiers();
    corpus.prose(
        "docs/handshake.md",
        "Handshake",
        "The daemon publishes a handshake file and heartbeats on the loopback port.",
    );

    assert_eq!(
        corpus.search("handshake heartbeats"),
        vec!["docs/handshake.md".to_string()]
    );
    // A multi-word prose question still resolves conjunctively over the body
    // text, unchanged by the presence of a fourth column.
    assert_eq!(
        corpus.search("runner starts workers"),
        vec!["docs/overview.md".to_string()]
    );
}

/// Anchors carry symbol names, which are the densest identifiers there are,
/// and they are indexed at the highest BM25 weight. A subword of a symbol name
/// has to be reachable even when the body never spells it out.
#[test]
fn a_symbol_name_is_reachable_by_one_of_its_words() {
    let mut corpus = Corpus::new();
    corpus.code(
        "src/pipeline.rs",
        "Pipeline.parseJSONResponse",
        "// body deliberately says nothing the query could match",
    );

    assert_eq!(
        corpus.search("parse response"),
        vec!["src/pipeline.rs".to_string()]
    );
    assert_eq!(
        corpus.search("parseJSONResponse"),
        vec!["src/pipeline.rs".to_string()]
    );
}
