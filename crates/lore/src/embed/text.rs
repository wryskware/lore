//! Embedding text construction — 3.1 §"Embedding text construction".
//!
//! A chunk is not embedded verbatim. It is embedded as
//!
//! ```text
//! <document_prefix><header>\n<chunk text>
//! ```
//!
//! where the header is one compact line naming the language, the
//! project-relative path and the structural anchor (symbol kind + path, or
//! heading path). That header is opencode's technique and measurably helps
//! code retrieval: without it a three-line method body is nearly contentless,
//! and every `Update()` in the repository embeds to the same point.
//!
//! Queries get the mirror treatment: `<query_prefix><query>`. The two
//! prefixes are model-card instruction strings and are part of the persisted
//! fingerprint, so changing either forces a re-embed rather than silently
//! mixing two encodings.
//!
//! # Discriminators are stripped
//!
//! The chunker appends bookkeeping suffixes to anchors so that derived chunk
//! ids stay unique and stable: `#w<n>` for an oversized span split into
//! windows, `#d<n>` for a genuine id collision, `#s<n>` for an unnamed run of
//! statements. They are storage identity, not meaning — an embedding model
//! reading `Board.Update#w1` learns nothing except noise — so the header
//! shows `Board.Update`.
//!
//! Stripping here is *readability only*, and stays deliberately naive: a
//! heading a human typed as `#w0` costs nothing to hide from an embedding
//! header. Ranking makes the opposite choice and reads no anchor strings at
//! all — window membership reaches it as [`crate::types::WindowFamily`]
//! metadata, because there a wrong guess deletes a result.
//!
//! # Fused identifiers are glossed
//!
//! A code header also carries a short parenthetical spelling the symbol out in
//! words: `function item search_page_final (search page final)`. The reason is
//! the same one the lexical side has (see [`crate::store::subword`]) — a
//! sentencepiece tokenizer shreds `search_page_final` into pieces that share
//! very little with how the query "search the final page" is tokenized — and it
//! is the *same splitter*, so the two arms cannot come to disagree about what a
//! name is made of.
//!
//! Two limits keep the gloss from diluting the header it decorates:
//!
//! - it appears **only** when splitting actually says something, so
//!   `Board.Update` (already two plain words to any tokenizer) gets nothing,
//!   and only genuinely fused names like `UpdateAll` or `parseJSONResponse` pay
//!   for one; and
//! - it is one parenthetical of deduplicated words, never a second copy of the
//!   header. A header is a handful of tokens next to a whole chunk body; every
//!   token spent restating what is already there moves the embedding away from
//!   the code it is supposed to represent.
//!
//! Prose anchors — Markdown heading paths — are untouched. A heading is already
//! words, and splitting it could only fabricate ones its author did not write.
//!
//! # Changing any of this forces a re-embed
//!
//! The recipe above is identity, not presentation: two chunks embedded under
//! different header rules are not comparable. [`EMBED_TEXT_RECIPE`] is folded
//! into the persisted [`crate::store::EmbeddingFingerprint`] for exactly that
//! reason, so editing the construction here and bumping the tag costs one full
//! re-embed at the next worker start and nothing subtler.

use crate::store::subword;
use crate::types::{Chunk, ChunkKind};

/// Version tag for the *construction rules* in this module, carried in the
/// persisted embedding fingerprint.
///
/// Bump it whenever [`document_text`] or [`query_text`] would produce different
/// bytes for the same chunk. The fingerprint comparison then reports a changed
/// embedding space and the worker re-embeds everything, which is the honest
/// outcome: vectors built from two different header recipes rank against each
/// other badly and nothing downstream can tell.
///
/// - `v1` — language, path and anchor phrase, symbol paths left as written.
/// - `v2` — adds the subword gloss described above.
pub const EMBED_TEXT_RECIPE: &str = "v2";

/// Per-chunk byte ceiling **for embedding only**; the stored chunk and its
/// FTS postings are untouched.
///
/// Chunks are already capped at [`crate::chunk::MAX_CHUNK_BYTES`] (4 KiB), so
/// this only ever bites on the unknown-text window path, and it exists to keep
/// one pathological input from blowing a server's context window and failing
/// the whole batch with it.
pub const MAX_EMBED_TEXT_BYTES: usize = 8 * 1024;

/// Discriminator markers that carry no meaning for a reader: window split,
/// duplicate disambiguation, unnamed-statement filler.
pub const ALL_MARKERS: [char; 3] = ['w', 'd', 's'];

/// Text actually sent for a chunk.
pub fn document_text(chunk: &Chunk, document_prefix: &str) -> String {
    format!("{document_prefix}{}\n{}", header(chunk), chunk.text)
}

/// Text actually sent for a query.
pub fn query_text(query: &str, query_prefix: &str) -> String {
    format!("{query_prefix}{query}")
}

/// The compact provenance line: `csharp Assets/Board.cs method Board.Update`,
/// `markdown design/3.1.md Ranking > Fusion`.
pub fn header(chunk: &Chunk) -> String {
    let language = chunk.language.as_deref().unwrap_or("text");
    let mut out = format!("{language} {}", chunk.path);
    if let Some(anchor) = anchor_phrase(&chunk.kind) {
        out.push(' ');
        out.push_str(&anchor);
    }
    out
}

/// The human-readable part of the header, or `None` for a plain text window
/// (whose ordinal is bookkeeping, not description).
fn anchor_phrase(kind: &ChunkKind) -> Option<String> {
    match kind {
        ChunkKind::Code {
            symbol_kind,
            symbol_path,
            ..
        } => {
            let symbol = strip_discriminators(symbol_path, &ALL_MARKERS);
            // "group" means the chunker merged several tiny siblings; naming
            // only the first one would misdescribe the chunk's extent.
            let mut phrase = if symbol_kind == "group" {
                match symbol.is_empty() {
                    true => "group".to_string(),
                    false => format!("group starting at {symbol}"),
                }
            } else {
                // Tree-sitter node kinds are snake_case ("method_declaration");
                // spaces read as language rather than as an identifier.
                let kind = symbol_kind.replace('_', " ");
                match symbol.is_empty() {
                    true => kind,
                    false => format!("{kind} {symbol}"),
                }
            };
            if let Some(gloss) = split_gloss(&symbol)
                && !phrase.is_empty()
            {
                phrase = format!("{phrase} ({gloss})");
            }
            (!phrase.is_empty()).then_some(phrase)
        }
        ChunkKind::Section { heading_path, .. } => {
            let titles: Vec<&str> = heading_path
                .iter()
                .filter(|title| !is_discriminator(title, &ALL_MARKERS))
                .map(String::as_str)
                .collect();
            (!titles.is_empty()).then(|| titles.join(" > "))
        }
        ChunkKind::Window { .. } => None,
    }
}

/// The parenthetical word gloss for a symbol path, or `None` when splitting it
/// would only restate its own spelling.
///
/// The runs walked here are the ones [`crate::store::subword`] walks — maximal
/// spans of alphanumerics and `_` — so `.`, `::`, `<>` and the `#` in a Rust
/// trait-impl path all separate names without appearing in the gloss.
///
/// `None` is the common case and is the point. The gloss is emitted only when
/// at least one run is a real expansion; `Board.Update` and `store::subword`
/// are already word-per-token to any tokenizer, and glossing them would spend
/// header tokens to say nothing.
///
/// Words are lowercased (the gloss is a reading of the name, not a second
/// spelling of it) and deduplicated in first-seen order, so `Card.CardView`
/// glosses as `card view` rather than `card card view`.
fn split_gloss(symbol: &str) -> Option<String> {
    let mut expanded = false;
    let mut words: Vec<String> = Vec::new();
    for run in subword::runs(symbol) {
        let parts = subword::split_parts(run);
        expanded |= subword::is_expansion(run, &parts);
        for part in parts {
            let word = part.to_lowercase();
            if !words.contains(&word) {
                words.push(word);
            }
        }
    }
    (expanded && !words.is_empty()).then(|| words.join(" "))
}

/// Remove trailing discriminator suffixes from a dotted symbol path.
///
/// Handles both spellings the chunker produces: a suffix glued to the last
/// segment (`Board.Update#w1`) and a segment that *is* the discriminator
/// (`Lexomancy.#s0`, whose leftover separator dot goes too).
pub fn strip_discriminators(symbol_path: &str, markers: &[char]) -> String {
    let mut rest = symbol_path;
    while let Some(hash) = rest.rfind('#') {
        if !is_discriminator(&rest[hash..], markers) {
            break;
        }
        rest = rest[..hash].trim_end_matches('.');
    }
    rest.to_string()
}

/// Is `token` exactly a discriminator — `#`, one of `markers`, then digits?
pub fn is_discriminator(token: &str, markers: &[char]) -> bool {
    let Some(body) = token.strip_prefix('#') else {
        return false;
    };
    let mut chars = body.chars();
    match chars.next() {
        Some(marker) if markers.contains(&marker) => {}
        _ => return false,
    }
    let digits = chars.as_str();
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Clip to at most `max` bytes without splitting a character.
///
/// Chunk text is arbitrary UTF-8; a raw byte slice would panic on the first
/// multi-byte character that straddles the limit.
pub fn truncate_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Chunk, ChunkId, VaultMeta};
    use camino::Utf8PathBuf;

    fn chunk(path: &str, language: Option<&str>, kind: ChunkKind, text: &str) -> Chunk {
        Chunk {
            id: ChunkId("x".into()),
            path: Utf8PathBuf::from(path),
            kind,
            language: language.map(str::to_string),
            byte_start: 0,
            byte_end: text.len() as u32,
            line_start: 1,
            line_end: 1,
            text: text.to_string(),
            vault: None::<VaultMeta>,
        }
    }

    fn code(symbol_kind: &str, symbol_path: &str) -> ChunkKind {
        ChunkKind::Code {
            symbol_kind: symbol_kind.into(),
            symbol_path: symbol_path.into(),
            window: None,
        }
    }

    #[test]
    fn header_names_language_path_and_symbol() {
        let c = chunk(
            "Assets/Board.cs",
            Some("csharp"),
            code("method_declaration", "Board.Update"),
            "void Update() {}",
        );
        assert_eq!(
            header(&c),
            "csharp Assets/Board.cs method declaration Board.Update"
        );
    }

    /// The gloss exists so a fused identifier reaches a natural-language
    /// query. It is appended once, in parentheses, after the symbol itself.
    #[test]
    fn a_fused_identifier_gains_one_parenthetical_gloss() {
        let snake = chunk(
            "crates/lore/src/search.rs",
            Some("rust"),
            code("function_item", "search_page_final"),
            "fn search_page_final() {}",
        );
        assert_eq!(
            header(&snake),
            "rust crates/lore/src/search.rs function item search_page_final (search page final)"
        );

        // camelCase and an acronym run split by the same rules the lexical
        // column uses: `HTTP` stays whole, `Response` starts a word.
        let camel = chunk(
            "Assets/Net.cs",
            Some("csharp"),
            code("method_declaration", "Client.parseHTTPResponse"),
            "",
        );
        assert_eq!(
            header(&camel),
            "csharp Assets/Net.cs method declaration Client.parseHTTPResponse \
             (client parse http response)"
        );
    }

    /// The expensive half of the rule: a name that is *already* words costs no
    /// header tokens at all. `Board.Update` and `store::subword` are one word
    /// per token to any tokenizer, so restating them would be pure dilution.
    #[test]
    fn a_name_that_is_already_plain_words_gets_no_gloss() {
        for (kind, symbol) in [
            ("method_declaration", "Board.Update"),
            ("mod_item", "store::subword"),
            ("function_item", "main"),
            // A trait impl: `#` separates without appearing in any gloss, and
            // both halves are plain words, so there is nothing to say.
            ("impl_item", "Index#Display"),
        ] {
            let c = chunk("f.rs", Some("rust"), code(kind, symbol), "");
            assert_eq!(
                header(&c),
                format!("rust f.rs {} {symbol}", kind.replace('_', " ")),
                "{symbol} must not be glossed"
            );
        }
    }

    /// Discriminators are stripped *before* the gloss is built, so a windowed
    /// chunk glosses its symbol and not its bookkeeping.
    #[test]
    fn a_windowed_anchor_glosses_the_stripped_symbol() {
        let c = chunk(
            "src/index.rs",
            Some("rust"),
            code("function_item", "Indexer.fullScanAll#w2"),
            "",
        );
        assert_eq!(
            header(&c),
            "rust src/index.rs function item Indexer.fullScanAll (indexer full scan all)"
        );
    }

    /// Words are deduplicated in first-seen order, so a name that repeats a
    /// scope in its own leaf does not repeat it in the gloss either. Merged
    /// siblings keep their "group starting at" phrasing around it.
    #[test]
    fn the_gloss_is_deduplicated_and_survives_the_group_phrasing() {
        let c = chunk(
            "Ui/Card.cs",
            Some("csharp"),
            code("group", "Card.CardView#w0"),
            "",
        );
        assert_eq!(
            header(&c),
            "csharp Ui/Card.cs group starting at Card.CardView (card view)"
        );

        // Digits are their own subword, as they are on the lexical side.
        assert_eq!(
            split_gloss("readUtf8Blob").as_deref(),
            Some("read utf 8 blob")
        );
        // Nothing to split, nothing to say.
        assert_eq!(split_gloss("Board.Update"), None);
        assert_eq!(split_gloss(""), None);
        // A leading underscore is an expansion even though it yields one part.
        assert_eq!(split_gloss("_private"), Some("private".to_string()));
    }

    #[test]
    fn header_uses_the_heading_path_for_sections() {
        let c = chunk(
            "design/x.md",
            Some("markdown"),
            ChunkKind::Section {
                heading_path: vec!["Retrieval".into(), "Ranking".into()],
                window: None,
            },
            "RRF fuses the two arms.",
        );
        assert_eq!(header(&c), "markdown design/x.md Retrieval > Ranking");
    }

    /// Prose is byte-identical to what the `v1` recipe produced. A heading is
    /// already words; splitting `authority_laundering` into a gloss would put
    /// tokens the author never wrote into a document's embedding, and the
    /// non-regression claim for the design vault rests on prose not moving.
    #[test]
    fn prose_headers_are_unchanged_by_the_gloss() {
        let heading = chunk(
            "design/1_Architecture/authority_laundering.md",
            Some("markdown"),
            ChunkKind::Section {
                heading_path: vec!["authority_laundering".into(), "parseJSON".into()],
                window: None,
            },
            "body",
        );
        assert_eq!(
            header(&heading),
            "markdown design/1_Architecture/authority_laundering.md \
             authority_laundering > parseJSON"
        );

        let window = chunk(
            "logs/run_final.log",
            None,
            ChunkKind::Window { index: 7 },
            "boot",
        );
        assert_eq!(header(&window), "text logs/run_final.log");
        assert_eq!(
            document_text(&window, "passage: "),
            "passage: text logs/run_final.log\nboot"
        );
    }

    #[test]
    fn header_hides_every_chunker_discriminator() {
        let windowed = chunk(
            "Assets/Board.cs",
            Some("csharp"),
            code("method_declaration", "Board.Update#w1"),
            "",
        );
        assert!(header(&windowed).ends_with("method declaration Board.Update"));

        let filler = chunk(
            "Program.cs",
            Some("csharp"),
            code("statements", "Ns.#s0"),
            "",
        );
        assert_eq!(header(&filler), "csharp Program.cs statements Ns");

        let section = chunk(
            "notes.md",
            Some("markdown"),
            ChunkKind::Section {
                heading_path: vec!["Top".into(), "#w2".into()],
                window: Some(crate::types::WindowFamily {
                    family: 0,
                    index: 2,
                }),
            },
            "",
        );
        assert_eq!(header(&section), "markdown notes.md Top");
    }

    #[test]
    fn merged_siblings_say_so() {
        let c = chunk(
            "Models.cs",
            Some("csharp"),
            code("group", "Models.Card#w0"),
            "",
        );
        assert_eq!(header(&c), "csharp Models.cs group starting at Models.Card");
    }

    #[test]
    fn windows_and_unknown_languages_degrade_gracefully() {
        let c = chunk("logs/run.log", None, ChunkKind::Window { index: 3 }, "boot");
        assert_eq!(header(&c), "text logs/run.log");
        assert_eq!(
            document_text(&c, "passage: "),
            "passage: text logs/run.log\nboot"
        );
        assert_eq!(query_text("cache", "query: "), "query: cache");
    }

    #[test]
    fn stripping_is_repeated_and_marker_scoped() {
        assert_eq!(strip_discriminators("A.B#w1#d2", &ALL_MARKERS), "A.B");
        // Stripping is scoped to the markers it is handed, and stops at the
        // first suffix outside that set.
        assert_eq!(strip_discriminators("A.#s0#w1", &['w']), "A.#s0");
        assert_eq!(strip_discriminators("A.#s0#w1", &ALL_MARKERS), "A");
        // A `#` that is not a discriminator is left alone.
        assert_eq!(strip_discriminators("C#.Parser", &ALL_MARKERS), "C#.Parser");
        assert_eq!(strip_discriminators("A#wx", &ALL_MARKERS), "A#wx");
        assert_eq!(strip_discriminators("A#w", &ALL_MARKERS), "A#w");
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let text = "é".repeat(100); // two bytes each
        let clipped = truncate_bytes(&text, 9);
        assert_eq!(clipped.len(), 8);
        assert!(text.starts_with(clipped));
        assert_eq!(truncate_bytes("short", 99), "short");
    }
}
