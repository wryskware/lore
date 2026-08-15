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

use crate::types::{Chunk, ChunkKind};

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
            let phrase = if symbol_kind == "group" {
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
