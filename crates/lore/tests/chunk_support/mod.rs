//! Shared helpers for the `chunk_*` integration tests.
//!
//! Not a test target itself (subdirectories of `tests/` are not compiled as
//! integration tests), just a module each test file includes.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::PathBuf;

use camino::Utf8Path;
use lore::chunk::{FileChunks, chunk_file};
use lore::types::{Chunk, ChunkKind};

/// Every fixture, as a project-relative path under `tests/fixtures`.
pub const FIXTURES: &[&str] = &[
    "csharp/BoardController.cs",
    "csharp/Models.cs",
    "csharp/Program.cs",
    "rust/index.rs",
    "python/retriever.py",
    "typescript/store.ts",
    "javascript/widget.jsx",
    "markdown/retrieval.md",
    "markdown/notes.md",
    "text/indexer.log",
];

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn read(name: &str) -> Vec<u8> {
    std::fs::read(fixture_root().join(name)).expect("fixture is readable")
}

/// Chunks a fixture under its `tests/fixtures`-relative path.
pub fn chunk_fixture(name: &str) -> Vec<Chunk> {
    let bytes = read(name);
    match chunk_file(Utf8Path::new(name), &bytes) {
        FileChunks::Chunked(chunks) => chunks,
        FileChunks::Skipped(reason) => panic!("{name} unexpectedly skipped: {reason:?}"),
    }
}

pub fn anchor_label(kind: &ChunkKind) -> String {
    match kind {
        ChunkKind::Code {
            symbol_kind,
            symbol_path,
        } => format!("code[{symbol_kind}] {symbol_path}"),
        ChunkKind::Section { heading_path } => format!("section [{}]", heading_path.join(" > ")),
        ChunkKind::Window { index } => format!("window {index}"),
    }
}

/// Structure-only rendering: anchor, spans, size and the first line of the
/// chunk. Bodies stay out of the snapshot on purpose.
pub fn render(chunks: &[Chunk]) -> String {
    let mut out = String::new();
    for chunk in chunks {
        let first = chunk.text.lines().next().unwrap_or_default().trim_end();
        let _ = writeln!(
            out,
            "{}\n  lang={:?} lines {}-{} bytes {}..{} ({} B)\n  first: {first}",
            anchor_label(&chunk.kind),
            chunk.language.as_deref(),
            chunk.line_start,
            chunk.line_end,
            chunk.byte_start,
            chunk.byte_end,
            chunk.byte_end - chunk.byte_start,
        );
        if let Some(vault) = &chunk.vault {
            let _ = writeln!(
                out,
                "  vault: status={:?} refs={:?} body_refs={:?}",
                vault.design_status, vault.decision_refs, vault.body_decision_refs
            );
        }
    }
    out
}
