//! Oversize splitting and window overlap, on generated content so the
//! thresholds are exercised without carrying a 20 KB fixture in the repo.

mod chunk_support;

use camino::Utf8Path;
use chunk_support::{anchor_label, chunk_fixture, render};
use lore::chunk::{
    FileChunks, MAX_CHUNK_BYTES, TEXT_WINDOW_LINES, WINDOW_OVERLAP_LINES, chunk_file,
};
use lore::repo_config::Profile;
use lore::types::{Chunk, ChunkKind, WindowFamily};

fn chunk(name: &str, src: &str) -> Vec<Chunk> {
    match chunk_file(Utf8Path::new(name), src.as_bytes(), Some(Profile::LoreV1)) {
        FileChunks::Chunked(chunks) => chunks,
        FileChunks::Skipped(reason) => panic!("unexpected skip: {reason:?}"),
    }
}

fn oversized_rust() -> String {
    let mut src = String::from("/// A function with an unreasonable body.\npub fn big_fn() {\n");
    for i in 0..300 {
        src.push_str(&format!(
            "    let value_{i} = {i} * 2; // filler line keeping the body well over the split threshold\n"
        ));
    }
    src.push_str("}\n");
    src
}

fn oversized_markdown() -> String {
    let mut src = String::from("# Big section\n\n");
    for i in 0..300 {
        src.push_str(&format!(
            "Sentence {i} of a section that is far too long to embed as a single chunk.\n"
        ));
    }
    src
}

/// Consecutive windows must share exactly `WINDOW_OVERLAP_LINES` lines.
fn assert_overlap(chunks: &[Chunk]) {
    for pair in chunks.windows(2) {
        assert_eq!(
            pair[1].line_start,
            pair[0].line_end + 1 - WINDOW_OVERLAP_LINES,
            "windows {:?} / {:?} do not overlap as configured",
            anchor_label(&pair[0].kind),
            anchor_label(&pair[1].kind),
        );
    }
}

#[test]
fn oversized_symbols_split_into_anchored_windows() {
    let src = oversized_rust();
    let chunks = chunk("src/big.rs", &src);
    assert!(chunks.len() > 3, "expected several windows");

    for (i, c) in chunks.iter().enumerate() {
        match &c.kind {
            ChunkKind::Code {
                symbol_path,
                symbol_kind,
                window,
            } => {
                assert_eq!(symbol_path, &format!("big_fn#w{i}"));
                assert_eq!(symbol_kind, "function_item");
                // Every window of one split span shares a family; the index
                // is the same ordinal the `#w` suffix spells.
                assert_eq!(
                    window,
                    &Some(WindowFamily {
                        family: 0,
                        index: i as u32
                    })
                );
            }
            other => panic!("unexpected kind {other:?}"),
        }
        let size = (c.byte_end - c.byte_start) as usize;
        assert!(size <= MAX_CHUNK_BYTES, "window {i} is {size} B");
    }
    assert_overlap(&chunks);

    // The windows still cover the whole declaration.
    assert_eq!(chunks[0].byte_start, 0);
    assert_eq!(
        chunks.last().unwrap().byte_end as usize,
        src.trim_end().len()
    );

    // Splitting is stable: identical bytes ⇒ identical ids.
    let again = chunk("src/big.rs", &src);
    assert_eq!(chunks, again);
    insta::assert_snapshot!(render(&chunks));
}

#[test]
fn oversized_markdown_sections_keep_their_heading_path() {
    let src = oversized_markdown();
    let chunks = chunk("docs/big.md", &src);
    assert!(chunks.len() > 3);
    for (i, c) in chunks.iter().enumerate() {
        match &c.kind {
            ChunkKind::Section {
                heading_path,
                window,
            } => {
                assert_eq!(heading_path, &["Big section".to_string(), format!("#w{i}")]);
                assert_eq!(
                    window,
                    &Some(WindowFamily {
                        family: 0,
                        index: i as u32
                    })
                );
            }
            other => panic!("unexpected kind {other:?}"),
        }
        assert!(c.vault.is_some(), "markdown chunks always carry vault meta");
    }
    assert_overlap(&chunks);
}

#[test]
fn unknown_text_windows_are_configured_height_with_overlap() {
    let chunks = chunk_fixture("text/indexer.log");
    assert_eq!(chunks.len(), 3);
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.kind, ChunkKind::Window { index: i as u32 });
    }
    // Full windows are exactly TEXT_WINDOW_LINES tall; the tail is shorter.
    assert_eq!(
        chunks[0].line_end - chunks[0].line_start + 1,
        TEXT_WINDOW_LINES
    );
    assert_eq!(
        chunks[1].line_end - chunks[1].line_start + 1,
        TEXT_WINDOW_LINES
    );
    assert_overlap(&chunks);
}
