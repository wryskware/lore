//! Structural snapshots of the chunker's output, one per fixture.
//!
//! Snapshots record anchors, spans and the first line of each chunk — never
//! whole bodies, so they stay readable and fail loudly on real structural
//! regressions rather than on prose edits.

mod chunk_support;

use chunk_support::{chunk_fixture, render};
use lore::types::{ChunkKind, DesignStatus};

macro_rules! fixture_snapshot {
    ($test:ident, $name:expr) => {
        #[test]
        fn $test() {
            insta::assert_snapshot!(render(&chunk_fixture($name)));
        }
    };
}

// C# — the flagship target. Between them these three cover block and
// file-scoped namespaces, records, primary constructors, top-level
// statements, attributes, interpolated strings, preprocessor directives and
// XML doc comments.
fixture_snapshot!(csharp_block_namespace, "csharp/BoardController.cs");
fixture_snapshot!(csharp_file_scoped_namespace, "csharp/Models.cs");
fixture_snapshot!(csharp_top_level_statements, "csharp/Program.cs");

fixture_snapshot!(rust_module, "rust/index.rs");
fixture_snapshot!(python_module, "python/retriever.py");
fixture_snapshot!(typescript_module, "typescript/store.ts");
fixture_snapshot!(javascript_module, "javascript/widget.jsx");

fixture_snapshot!(markdown_vault_document, "markdown/retrieval.md");
fixture_snapshot!(markdown_without_frontmatter, "markdown/notes.md");

fixture_snapshot!(unknown_text_windows, "text/indexer.log");

#[test]
fn markdown_heading_paths_nest_root_to_leaf() {
    let chunks = chunk_fixture("markdown/retrieval.md");
    let paths: Vec<Vec<String>> = chunks
        .iter()
        .filter_map(|chunk| match &chunk.kind {
            ChunkKind::Section { heading_path } => Some(heading_path.clone()),
            _ => None,
        })
        .collect();

    assert!(
        paths.contains(&vec![]),
        "preamble chunk keeps an empty path"
    );
    assert!(paths.contains(&vec!["Retrieval".to_string()]));
    assert!(paths.contains(&vec![
        "Retrieval".to_string(),
        "Chunking".to_string(),
        "Code".to_string(),
    ]));
    // `## Chunking` is a pure container with no intro text of its own.
    assert!(!paths.contains(&vec!["Retrieval".to_string(), "Chunking".to_string()]));
}

#[test]
fn markdown_vault_metadata_propagates_to_every_chunk() {
    let chunks = chunk_fixture("markdown/retrieval.md");
    for chunk in &chunks {
        let vault = chunk.vault.as_ref().expect("every .md chunk carries vault");
        assert_eq!(vault.design_status, Some(DesignStatus::Leaning));
        assert_eq!(vault.decision_refs, vec!["D-0004", "D-0005"]);
    }
    let code_section = chunks
        .iter()
        .find(|c| matches!(&c.kind, ChunkKind::Section { heading_path } if heading_path.last().is_some_and(|h| h == "Code")))
        .expect("### Code section");
    assert_eq!(
        code_section.vault.as_ref().unwrap().body_decision_refs,
        vec!["D-0005"]
    );

    // Fenced code inside that section must not be read as headings.
    assert!(code_section.text.contains("# not a heading"));
}

#[test]
fn markdown_without_frontmatter_is_unclassified() {
    for chunk in chunk_fixture("markdown/notes.md") {
        let vault = chunk.vault.expect("vault metadata is always present");
        assert_eq!(vault.design_status, None);
        assert!(vault.decision_refs.is_empty());
    }
}
