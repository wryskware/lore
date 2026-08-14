//! Invariants that must hold for every fixture, in every language.
//!
//! These are the properties the rest of the system leans on: exact spans,
//! verbatim text, unique IDs, and ID stability across re-chunking (vectors
//! and FTS rows survive a re-index only because of the last one).

mod chunk_support;

use std::collections::HashSet;

use camino::Utf8Path;
use chunk_support::{FIXTURES, chunk_fixture, read};
use lore::chunk::{MAX_CHUNK_BYTES, TEXT_WINDOW_MAX_BYTES, chunk_file};
use lore::types::ChunkKind;

#[test]
fn spans_are_exact_and_text_is_a_verbatim_slice() {
    for name in FIXTURES {
        let bytes = read(name);
        let src = std::str::from_utf8(&bytes).expect("fixture is UTF-8");
        for chunk in chunk_fixture(name) {
            let (start, end) = (chunk.byte_start as usize, chunk.byte_end as usize);
            assert!(start < end, "{name}: empty span in {:?}", chunk.kind);
            assert!(
                end <= src.len(),
                "{name}: span past EOF in {:?}",
                chunk.kind
            );
            assert!(
                src.is_char_boundary(start) && src.is_char_boundary(end),
                "{name}: span splits a character in {:?}",
                chunk.kind
            );
            assert_eq!(
                &src[start..end],
                chunk.text,
                "{name}: text is not the file slice for {:?}",
                chunk.kind
            );

            let line_start = 1 + src[..start].matches('\n').count() as u32;
            let line_end = 1 + src[..end - 1].matches('\n').count() as u32;
            assert_eq!(chunk.line_start, line_start, "{name}: {:?}", chunk.kind);
            assert_eq!(chunk.line_end, line_end, "{name}: {:?}", chunk.kind);
        }
    }
}

#[test]
fn chunk_ids_are_unique_within_a_file() {
    for name in FIXTURES {
        let mut seen = HashSet::new();
        for chunk in chunk_fixture(name) {
            assert!(
                seen.insert(chunk.id.clone()),
                "{name}: duplicate id for {:?}",
                chunk.kind
            );
        }
    }
}

#[test]
fn rechunking_identical_content_is_idempotent() {
    for name in FIXTURES {
        let first = chunk_fixture(name);
        let second = chunk_fixture(name);
        assert_eq!(first, second, "{name}: chunking is not deterministic");

        // The load-bearing property: same bytes, same path ⇒ same IDs, even
        // through a fresh parser instance and a differently-spelled path.
        let bytes = read(name);
        let windows_path = name.replace('/', "\\");
        let again = chunk_file(Utf8Path::new(&windows_path), &bytes);
        let ids: Vec<_> = again.chunks().iter().map(|c| c.id.clone()).collect();
        let expected: Vec<_> = first.iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids, expected, "{name}: ids changed across re-chunk");
    }
}

#[test]
fn chunks_are_ordered_and_sized() {
    for name in FIXTURES {
        let chunks = chunk_fixture(name);
        let mut previous = 0;
        for chunk in &chunks {
            assert!(
                chunk.byte_start >= previous,
                "{name}: chunks out of order at {:?}",
                chunk.kind
            );
            previous = chunk.byte_start;

            let size = (chunk.byte_end - chunk.byte_start) as usize;
            let cap = match chunk.kind {
                ChunkKind::Window { .. } => TEXT_WINDOW_MAX_BYTES,
                _ => MAX_CHUNK_BYTES,
            };
            // A single line longer than the cap cannot be split further.
            let single_line = chunk.line_start == chunk.line_end;
            assert!(
                size <= cap || single_line,
                "{name}: oversized chunk {size} B at {:?}",
                chunk.kind
            );
        }
    }
}

#[test]
fn language_tags_follow_the_extension() {
    for name in FIXTURES {
        let expected = match name.rsplit('.').next().unwrap() {
            "cs" => Some("csharp"),
            "rs" => Some("rust"),
            "py" => Some("python"),
            "jsx" | "js" => Some("javascript"),
            "ts" | "tsx" => Some("typescript"),
            "md" => Some("markdown"),
            _ => None,
        };
        for chunk in chunk_fixture(name) {
            assert_eq!(chunk.language.as_deref(), expected, "{name}");
            assert_eq!(chunk.path.as_str(), *name, "{name}: path is preserved");
        }
    }
}

#[test]
fn vault_metadata_is_markdown_only() {
    for name in FIXTURES {
        let is_markdown = name.ends_with(".md");
        for chunk in chunk_fixture(name) {
            assert_eq!(chunk.vault.is_some(), is_markdown, "{name}");
        }
    }
}

/// Content is allowed to fall between chunks — closing braces, blank lines,
/// `namespace Foo {` headers that survive only as a `symbol_path` — but the
/// bulk of every file must be indexed. A dropped comment block or a lost
/// declaration shows up here immediately.
#[test]
fn chunks_cover_almost_all_content() {
    for name in FIXTURES {
        let bytes = read(name);
        let src = std::str::from_utf8(&bytes).unwrap();
        let mut covered = vec![false; src.len()];
        // YAML frontmatter is metadata, never a chunk (3.1).
        if let Some(rest) = src.strip_prefix("---\n") {
            let end = rest.find("\n---").map_or(0, |i| 4 + i + 5);
            for flag in &mut covered[..end.min(src.len())] {
                *flag = true;
            }
        }
        for chunk in chunk_fixture(name) {
            for flag in &mut covered[chunk.byte_start as usize..chunk.byte_end as usize] {
                *flag = true;
            }
        }
        let significant = |b: u8| !b.is_ascii_whitespace() && !b"{}();,".contains(&b);
        let total = src.bytes().filter(|b| significant(*b)).count();
        let missed = src
            .bytes()
            .zip(&covered)
            .filter(|(b, seen)| significant(*b) && !**seen)
            .count();
        let ratio = 1.0 - (missed as f64 / total as f64);
        assert!(
            ratio >= 0.95,
            "{name}: only {:.1}% of content is indexed ({missed} of {total} bytes missed)",
            ratio * 100.0
        );
    }
}
