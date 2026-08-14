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

use crate::types::Chunk;

/// Bump whenever chunking policy changes in a way that should re-chunk
/// unchanged files. The indexer mixes this into its per-file content hash,
/// so a version bump invalidates the hash short-circuit and the next scan
/// re-chunks (and prunes newly-skipped) files whose bytes never moved.
pub const CHUNK_FORMAT_VERSION: u32 = 2;

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

/// Chunks one file. `rel_path` is project-relative; it participates in every
/// derived ID, so it is normalized to forward slashes first.
pub fn chunk_file(rel_path: &Utf8Path, content: &[u8]) -> FileChunks {
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
        return FileChunks::Chunked(markdown::chunk_markdown(&path, src));
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

    fn chunks(name: &str, body: &str) -> Vec<Chunk> {
        match chunk_file(Utf8Path::new(name), body.as_bytes()) {
            FileChunks::Chunked(chunks) => chunks,
            other => panic!("unexpected skip: {other:?}"),
        }
    }

    #[test]
    fn skips_binary_and_oversize() {
        let binary = chunk_file(Utf8Path::new("a.bin"), b"abc\0def");
        assert_eq!(binary.skip_reason(), Some(SkipReason::Binary));
        let big = vec![b'a'; MAX_FILE_BYTES + 1];
        assert_eq!(
            chunk_file(Utf8Path::new("a.txt"), &big).skip_reason(),
            Some(SkipReason::TooLarge)
        );
        let invalid = chunk_file(Utf8Path::new("a.txt"), &[0xf0, 0x28, 0x8c, 0x28]);
        assert_eq!(invalid.skip_reason(), Some(SkipReason::InvalidUtf8));
    }

    #[test]
    fn machine_text_is_skipped_only_on_window_paths() {
        // One giant line in a window-chunked file (the Lexomancy
        // tokenizer.json case) is skipped...
        let vocab = format!("{{\"vocab\":{}}}", "\"word\",".repeat(2000));
        assert!(vocab.len() > MAX_TEXT_LINE_BYTES);
        assert_eq!(
            chunk_file(Utf8Path::new("tokenizer.json"), vocab.as_bytes()).skip_reason(),
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
