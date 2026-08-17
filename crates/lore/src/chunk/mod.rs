//! Chunking pipeline — file content → [`crate::types::Chunk`]s.
//!
//! Strategies per 3.1 Chunking_and_Ranking: tree-sitter symbol chunks for
//! code (C# flagship), heading-tree leaves for Markdown with vault schema
//! awareness, line windows for unknown text, skip binary. No call or
//! reference extraction anywhere (D-0005).
//!
//! Everything downstream depends on one property: **re-chunking identical
//! bytes must produce identical [`crate::types::ChunkId`]s**, so vectors and
//! FTS rows survive a re-index. Chunk text is always a verbatim slice of the
//! file, byte and line spans are exact, and IDs come from
//! [`crate::types::Chunk::derive_id`].

mod code;
mod common;
mod markdown;
mod text;

use camino::{Utf8Path, Utf8PathBuf};

pub use common::{
    BINARY_SNIFF_BYTES, MAX_CHUNK_BYTES, MAX_FILE_BYTES, SMALL_CONTAINER_BYTES, TEXT_WINDOW_LINES,
    TEXT_WINDOW_MAX_BYTES, TINY_CHUNK_BYTES, WINDOW_OVERLAP_LINES,
};

/// `D-NNNN` extraction, shared with [`crate::authority`]'s ledger parser so
/// the two agree on what a decision reference looks like.
use crate::repo_config::Profile;
use crate::types::Chunk;

/// Bump whenever chunking policy changes in a way that should re-chunk
/// unchanged files. The indexer mixes this into its per-file content hash,
/// so a version bump invalidates the hash short-circuit and the next scan
/// re-chunks (and prunes newly-skipped) files whose bytes never moved.
///
/// 4: windowed chunks carry an explicit [`crate::types::WindowFamily`] instead
/// of leaving ranking to infer membership from the `#w<n>` anchor suffix.
/// Rows written by version 3 deserialize with no family, which makes them
/// *uncollapsible* — duplicate windows would show up in results until the file
/// next changed. This bump converges every file in one CPU-only re-chunk pass;
/// because the family is deliberately absent from `ChunkKind::anchor`, chunk
/// ids do not move and nothing is re-embedded.
///
/// 5: two chunk-text corrections. A leading UTF-8 BOM no longer rides inside
/// the first chunk of a code or plain-text file, and an ATX heading's trailing
/// `#`s are trimmed only where CommonMark calls them a closing sequence, so
/// `# Learning C#` keeps its name. Both change chunk *text* — and the heading
/// one also changes the anchor — so the affected chunks get new ids and are
/// re-embedded. Only files that actually carry a BOM or such a heading move.
pub const CHUNK_FORMAT_VERSION: u32 = 5;

/// A single line longer than this marks a file as machine text (minified
/// bundles, ML vocab dumps, serialized blobs). Dogfooding on the Lexomancy
/// repo showed one giant one-line `tokenizer.json` outranking real code on
/// every multi-word query — such files are token noise, not context.
pub const MAX_TEXT_LINE_BYTES: usize = 4096;

/// Why a file produced no chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Larger than [`MAX_FILE_BYTES`].
    TooLarge,
    /// NUL byte within the first [`BINARY_SNIFF_BYTES`].
    Binary,
    /// Not valid UTF-8.
    InvalidUtf8,
    /// A single line exceeds [`MAX_TEXT_LINE_BYTES`] in a file that would be
    /// window-chunked — minified/generated machine text, not human context.
    MachineText,
}

/// Result of chunking one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChunks {
    Skipped(SkipReason),
    Chunked(Vec<Chunk>),
}

impl FileChunks {
    /// Chunks produced, empty for a skipped or empty file.
    pub fn chunks(&self) -> &[Chunk] {
        match self {
            FileChunks::Skipped(_) => &[],
            FileChunks::Chunked(chunks) => chunks,
        }
    }

    pub fn skip_reason(&self) -> Option<SkipReason> {
        match self {
            FileChunks::Skipped(reason) => Some(*reason),
            FileChunks::Chunked(_) => None,
        }
    }
}

/// Does this path go down the Markdown path (and therefore care which
/// authority profile is in force)?
///
/// Exposed because the indexer needs the answer *before* reading the file, to
/// decide whether the profile belongs in the file's content hash.
pub fn is_markdown(rel_path: &Utf8Path) -> bool {
    rel_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// Chunks one file. `rel_path` is project-relative; it participates in every
/// derived ID, so it is normalized to forward slashes first.
///
/// `profile` is the repository's active authority profile (D-0012). It reaches
/// only the Markdown path, where it decides whether frontmatter is a schema or
/// just a header: `None` means no `design_status`, no `decision_refs`, no
/// `D-NNNN` body scan, and no [`crate::types::VaultMeta`] on any chunk. Chunk
/// *text*, spans and ids are identical either way — only the metadata differs
/// — so flipping a profile costs a re-chunk, never a re-embed.
pub fn chunk_file(rel_path: &Utf8Path, content: &[u8], profile: Option<Profile>) -> FileChunks {
    if content.len() > MAX_FILE_BYTES {
        return FileChunks::Skipped(SkipReason::TooLarge);
    }
    let sniff = &content[..content.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        return FileChunks::Skipped(SkipReason::Binary);
    }
    let Ok(src) = std::str::from_utf8(content) else {
        return FileChunks::Skipped(SkipReason::InvalidUtf8);
    };

    let path = Utf8PathBuf::from(rel_path.as_str().replace('\\', "/"));
    let extension = path
        .extension()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    if extension == "md" {
        return FileChunks::Chunked(markdown::chunk_markdown(&path, src, profile));
    }
    if let Some(spec) = language_tag(&extension).and_then(code::spec_for) {
        if let Some(chunks) = code::chunk_code(&spec, &path, src) {
            return FileChunks::Chunked(chunks);
        }
        // Grammar refused the file: degrade to windows rather than lose it —
        // unless it degrades because it is machine text (minified bundles).
        if is_machine_text(src) {
            return FileChunks::Skipped(SkipReason::MachineText);
        }
        return FileChunks::Chunked(text::chunk_text(&path, src, Some(spec.tag)));
    }
    if is_machine_text(src) {
        return FileChunks::Skipped(SkipReason::MachineText);
    }
    FileChunks::Chunked(text::chunk_text(&path, src, None))
}

/// Only window-chunked paths get this guard: code that parses and Markdown
/// keep their own structure even with the odd long line (tables, data URIs).
fn is_machine_text(src: &str) -> bool {
    src.lines().any(|line| line.len() > MAX_TEXT_LINE_BYTES)
}

/// Internal parser key for an extension (`tsx` selects the TSX grammar; the
/// `language` tag it reports is still `typescript`).
fn language_tag(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "cs" => "csharp",
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DesignStatus;

    /// The vault-aware profile, which is what every assertion here predates
    /// and therefore what keeps them testing the same thing (D-0012).
    const LORE_V1: Option<Profile> = Some(Profile::LoreV1);

    fn chunks(name: &str, body: &str) -> Vec<Chunk> {
        match chunk_file(Utf8Path::new(name), body.as_bytes(), LORE_V1) {
            FileChunks::Chunked(chunks) => chunks,
            other => panic!("unexpected skip: {other:?}"),
        }
    }

    #[test]
    fn skips_binary_and_oversize() {
        let binary = chunk_file(Utf8Path::new("a.bin"), b"abc\0def", LORE_V1);
        assert_eq!(binary.skip_reason(), Some(SkipReason::Binary));
        let big = vec![b'a'; MAX_FILE_BYTES + 1];
        assert_eq!(
            chunk_file(Utf8Path::new("a.txt"), &big, LORE_V1).skip_reason(),
            Some(SkipReason::TooLarge)
        );
        let invalid = chunk_file(Utf8Path::new("a.txt"), &[0xf0, 0x28, 0x8c, 0x28], LORE_V1);
        assert_eq!(invalid.skip_reason(), Some(SkipReason::InvalidUtf8));
    }

    /// D-0012: a repository that did not opt in gets the same chunks with none
    /// of the vocabulary. Same ids, same text, same spans — only the metadata
    /// disappears, which is what makes enabling a profile a re-chunk rather
    /// than a re-embed.
    #[test]
    fn a_neutral_repo_gets_markdown_without_the_vault_vocabulary() {
        let src = "---\ndesign_status: decided\ndecision_refs: [D-0003]\n---\n\n\
                   # H\n\nbody citing D-0004\n";
        let vault = chunks("a.md", src);
        let FileChunks::Chunked(neutral) = chunk_file(Utf8Path::new("a.md"), src.as_bytes(), None)
        else {
            panic!("markdown is still chunked without a profile")
        };

        assert!(vault[0].vault.is_some());
        assert!(neutral.iter().all(|c| c.vault.is_none()));
        assert_eq!(
            vault.iter().map(|c| &c.id).collect::<Vec<_>>(),
            neutral.iter().map(|c| &c.id).collect::<Vec<_>>(),
            "ids must not move, or a profile flip would discard every vector"
        );
        // The YAML header is still a header, not prose.
        assert!(neutral.iter().all(|c| !c.text.contains("design_status")));
    }

    #[test]
    fn machine_text_is_skipped_only_on_window_paths() {
        // One giant line in a window-chunked file (the Lexomancy
        // tokenizer.json case) is skipped...
        let vocab = format!("{{\"vocab\":{}}}", "\"word\",".repeat(2000));
        assert!(vocab.len() > MAX_TEXT_LINE_BYTES);
        assert_eq!(
            chunk_file(Utf8Path::new("tokenizer.json"), vocab.as_bytes(), LORE_V1).skip_reason(),
            Some(SkipReason::MachineText)
        );
        // ...but the same line inside Markdown or parseable code is kept:
        // those paths have real structure and the guard must not fire.
        let md = format!("# Title\n\n{vocab}\n");
        assert!(!chunks("data.md", &md).is_empty());
        let rust = format!(
            "const V: &str = \"{}\";\n",
            "x".repeat(MAX_TEXT_LINE_BYTES + 1)
        );
        assert!(!chunks("data.rs", &rust).is_empty());
    }

    #[test]
    fn empty_file_yields_no_chunks() {
        assert!(chunks("empty.txt", "").is_empty());
        assert!(chunks("blank.md", "\n\n   \n").is_empty());
    }

    #[test]
    fn path_separators_are_normalized_before_id_derivation() {
        let windows = chunks(r"src\a.txt", "hello");
        let posix = chunks("src/a.txt", "hello");
        assert_eq!(windows[0].id, posix[0].id);
        assert_eq!(windows[0].path.as_str(), "src/a.txt");
    }

    #[test]
    fn malformed_source_still_yields_exact_chunks() {
        // Truncated mid-method: tree-sitter returns a tree with ERROR nodes
        // rather than failing, and the spans must stay exact regardless.
        let src = "namespace A {\n  class B {\n    void C() {\n      if (x\n";
        let out = chunks("Broken.cs", src);
        assert!(!out.is_empty(), "a broken file is still indexed");
        for chunk in &out {
            let slice = &src[chunk.byte_start as usize..chunk.byte_end as usize];
            assert_eq!(slice, chunk.text);
            assert_eq!(chunk.language.as_deref(), Some("csharp"));
        }
    }

    #[test]
    fn frontmatter_variants_parse() {
        let inline = chunks(
            "a.md",
            "---\ndesign_status: decided\ndecision_refs: [D-0003, D-0004]\n---\n\n# H\n\nbody\n",
        );
        let vault = inline[0].vault.as_ref().unwrap();
        assert_eq!(vault.design_status, Some(DesignStatus::Decided));
        assert_eq!(vault.decision_refs, ["D-0003", "D-0004"]);

        // Unterminated frontmatter is not frontmatter.
        let broken = chunks("b.md", "---\ndesign_status: decided\n\n# H\n\nbody\n");
        assert_eq!(broken[0].vault.as_ref().unwrap().design_status, None);
        assert!(broken.iter().any(|c| c.text.contains("design_status")));
    }

    /// A Windows editor saving a vault document as "UTF-8 with BOM" must not
    /// silently downgrade it to unclassified: U+FEFF is not whitespace, so
    /// nothing trims it away before the `---` fence is compared.
    #[test]
    fn utf8_bom_does_not_hide_frontmatter() {
        for body in [
            "---\ndesign_status: decided\ndecision_refs: [D-0003, D-0004]\n---\n\n# H\n\nbody\n",
            // Same document as a Windows editor writes it: BOM + CRLF.
            "---\r\ndesign_status: decided\r\ndecision_refs:\r\n  - D-0003\r\n  - D-0004\r\n---\r\n\r\n# H\r\n\r\nbody\r\n",
        ] {
            let src = format!("\u{feff}{body}");
            let out = chunks("bom.md", &src);
            let vault = out[0].vault.as_ref().expect("markdown carries vault meta");
            assert_eq!(vault.design_status, Some(DesignStatus::Decided));
            assert_eq!(vault.decision_refs, ["D-0003", "D-0004"]);

            // The YAML is metadata, never body text, and the BOM is in no chunk.
            assert!(out.iter().all(|c| !c.text.contains("design_status")));
            assert!(out.iter().all(|c| !c.text.contains('\u{feff}')));
            assert!(out.iter().any(|c| c.text.contains("body")));

            // Spans stay relative to the *original* bytes, BOM included.
            for chunk in &out {
                let slice = &src[chunk.byte_start as usize..chunk.byte_end as usize];
                assert_eq!(slice, chunk.text, "span is not the file slice");
            }
        }
    }

    /// The same mark in front of a plain document must not swallow the first
    /// heading either — without the skip, `\u{feff}#` is not an ATX marker.
    #[test]
    fn utf8_bom_without_frontmatter_keeps_the_first_heading() {
        let src = "\u{feff}# Title\n\nsome prose\n";
        let out = chunks("bom2.md", src);
        assert!(out[0].text.starts_with("# Title"), "{:?}", out[0].text);
        assert_eq!(
            &src[out[0].byte_start as usize..out[0].byte_end as usize],
            out[0].text
        );
    }

    /// The same mark in a code or plain-text file is encoding, not content:
    /// it must not ride inside the first chunk, where it would pollute both
    /// the embedded text and the FTS row.
    #[test]
    fn utf8_bom_never_lands_in_a_code_or_text_chunk() {
        for (name, body) in [
            ("a.rs", "pub fn alpha() -> u32 {\n    41\n}\n"),
            (
                "A.cs",
                "namespace N {\n  class C {\n    void M() { }\n  }\n}\n",
            ),
            ("a.py", "def alpha():\n    return 41\n"),
            ("notes.txt", "first line\nsecond line\n"),
            ("notes.rst", "first line\nsecond line\n"),
        ] {
            let src = format!("\u{feff}{body}");
            let out = chunks(name, &src);
            assert!(!out.is_empty(), "{name} produced no chunks");
            assert!(
                out.iter().all(|c| !c.text.contains('\u{feff}')),
                "{name} leaked the BOM: {:?}",
                out[0].text
            );
            // Spans stay relative to the *original* bytes, BOM included.
            for chunk in &out {
                let slice = &src[chunk.byte_start as usize..chunk.byte_end as usize];
                assert_eq!(slice, chunk.text, "{name}: span is not the file slice");
            }
            // And the mark is invisible to identity, so re-saving a file with
            // or without it is not a re-embed.
            let plain = chunks(name, body);
            assert_eq!(
                out.iter().map(|c| &c.id).collect::<Vec<_>>(),
                plain.iter().map(|c| &c.id).collect::<Vec<_>>(),
                "{name}: the BOM moved chunk ids"
            );
        }
    }

    /// A rule short enough to fit in one sentence is exactly the kind of
    /// prose that must stay searchable: the intro-size heuristic decides
    /// which chunk holds it, never whether it is indexed at all.
    #[test]
    fn a_short_container_intro_is_folded_into_the_first_child() {
        let src = "# Safety\n\nNever upload.\n\n## Details\n\nLocal-only, bound to loopback.\n";
        let out = chunks("safety.md", src);
        assert!(
            out.iter().any(|c| c.text.contains("Never upload.")),
            "short intro dropped: {:?}",
            out.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
        );
        for chunk in &out {
            let slice = &src[chunk.byte_start as usize..chunk.byte_end as usize];
            assert_eq!(slice, chunk.text, "span is not the file slice");
        }

        // An intro past the threshold still earns its own chunk, and an
        // empty one still leaves the child's span untouched.
        let long = "# Safety\n\nNever upload anything to a remote service, ever.\n\n## Details\n\nLocal-only.\n";
        assert_eq!(chunks("long.md", long).len(), 2);
        let empty = "# Safety\n\n## Details\n\nLocal-only.\n";
        let out = chunks("empty-intro.md", empty);
        assert_eq!(out.len(), 1);
        assert!(out[0].text.starts_with("## Details"));
    }

    #[test]
    fn unknown_extension_falls_back_to_windows() {
        let body = (0..10).map(|i| format!("line {i}\n")).collect::<String>();
        let out = chunks("notes.rst", &body);
        assert_eq!(out.len(), 1);
        assert!(out[0].language.is_none());
    }

    #[test]
    fn decision_ref_scanner_respects_word_boundaries() {
        assert_eq!(common::decision_refs("see D-0004."), vec!["D-0004"]);
        assert_eq!(common::decision_refs("D-0004 and D-0004"), vec!["D-0004"]);
        assert!(common::decision_refs("XD-0004").is_empty());
        assert!(common::decision_refs("D-00041").is_empty());
        assert!(common::decision_refs("D-004").is_empty());
    }
}
