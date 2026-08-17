//! Chunking through a plugin: the invariants that must survive a chunker
//! nobody in this repo wrote.
//!
//! The load-bearing claims, in order of how much damage breaking them does:
//!
//! 1. re-chunking identical bytes yields identical [`lore::types::ChunkId`]s;
//! 2. chunk text is a verbatim file slice and byte/line spans are exact;
//! 3. `vault` is `None` on every plugin chunk, always;
//! 4. a file no plugin claims is chunked *byte-identically* to before plugins
//!    existed;
//! 5. a plugin that cannot run is a visible fallback, not a silent one.

#[cfg(feature = "wasm-grammars")]
use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::TempDir;

use lore::chunk::{ChunkOutcome, FileChunks, Route, SkipReason, chunk_file, chunk_file_with};
use lore::plugin::PluginRegistry;
use lore::repo_config::Profile;
use lore::types::{Chunk, ChunkKind};

fn fixture_registry() -> PluginRegistry {
    let dir = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins");
    let (registry, diagnostics) = PluginRegistry::load(&dir);
    if cfg!(feature = "wasm-grammars") {
        assert_eq!(diagnostics, vec![], "the fixture plugin must load cleanly");
    }
    registry
}

fn chunk(registry: &PluginRegistry, name: &str, src: &str) -> ChunkOutcome {
    chunk_file_with(
        Utf8Path::new(name),
        src.as_bytes(),
        Some(Profile::LoreV1),
        Some(registry),
    )
}

fn chunks(registry: &PluginRegistry, name: &str, src: &str) -> Vec<Chunk> {
    match chunk(registry, name, src).chunks {
        FileChunks::Chunked(chunks) => chunks,
        other => panic!("{name}: unexpected skip: {other:?}"),
    }
}

/// A UXML document big enough to exercise container splitting, so the symbol
/// paths in the assertions are the nested ones a real file produces.
fn uxml() -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <ui:UXML xmlns:ui=\"UnityEngine.UIElements\">\n\
         \x20   <Style src=\"MainPanel.uss\" />\n\
         \x20   <ui:VisualElement name=\"root\" class=\"panel\">\n",
    );
    for i in 0..20 {
        out.push_str(&format!(
            "        <!-- row {i}, citing D-0005 -->\n        \
             <ui:Button name=\"run{i}\" text=\"Run {i}\" tooltip=\"Runs thing {i} in the list\" />\n"
        ));
    }
    out.push_str("    </ui:VisualElement>\n</ui:UXML>\n");
    out
}

/// Every span a chunker emits must slice the source exactly, and the 1-based
/// line span must agree with it.
fn assert_spans_exact(chunks: &[Chunk], src: &str) {
    for chunk in chunks {
        let slice = &src[chunk.byte_start as usize..chunk.byte_end as usize];
        assert_eq!(slice, chunk.text, "chunk text is not the file slice");
        let line_of = |byte: usize| src[..byte].bytes().filter(|b| *b == b'\n').count() as u32 + 1;
        assert_eq!(chunk.line_start, line_of(chunk.byte_start as usize));
        assert_eq!(chunk.line_end, line_of(chunk.byte_end as usize - 1));
    }
}

#[cfg(feature = "wasm-grammars")]
#[test]
fn plugin_grammar_chunks_are_verbatim_slices_with_stable_ids() {
    let registry = fixture_registry();
    let src = uxml();
    let first = chunks(&registry, "Assets/UI/Main.uxml", &src);
    assert!(
        first.len() > 3,
        "the document should split: {}",
        first.len()
    );
    assert_spans_exact(&first, &src);

    // Re-chunking the same bytes — through a *freshly loaded* registry, so a
    // reload cannot move an id either — must be identical in every field.
    let again = chunks(&fixture_registry(), "Assets/UI/Main.uxml", &src);
    assert_eq!(first, again, "identical bytes must chunk identically");

    // Ids are unique within the file and derived, not invented.
    let ids: HashSet<_> = first.iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids.len(), first.len());
    for chunk in &first {
        assert_eq!(
            chunk.id,
            Chunk::derive_id(&chunk.path, &chunk.kind, &chunk.text)
        );
    }

    // Windows path separators are normalized before ids are derived, exactly
    // as on the built-in paths.
    let windows_path = chunks(&registry, r"Assets\UI\Main.uxml", &src);
    assert_eq!(
        windows_path.iter().map(|c| &c.id).collect::<Vec<_>>(),
        first.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    assert_eq!(windows_path[0].path.as_str(), "Assets/UI/Main.uxml");
}

#[cfg(feature = "wasm-grammars")]
#[test]
fn a_declared_grammar_mapping_produces_meaningful_symbol_paths() {
    let registry = fixture_registry();
    let src = uxml();
    let out = chunks(&registry, "Assets/UI/Main.uxml", &src);

    let paths: Vec<&str> = out
        .iter()
        .filter_map(|c| match &c.kind {
            ChunkKind::Code { symbol_path, .. } => Some(symbol_path.as_str()),
            _ => None,
        })
        .collect();
    // `name_kinds = ["Name"]` reaches `element > STag > Name`, which no field
    // lookup can: without it every path would spell `element`.
    assert!(paths.contains(&"ui:UXML"), "{paths:?}");
    assert!(paths.contains(&"ui:UXML.ui:VisualElement"), "{paths:?}");
    assert!(
        paths.iter().any(|p| p.contains("ui:Button")),
        "nested member names are missing: {paths:?}"
    );
    assert!(out.iter().all(|c| c.language.as_deref() == Some("xml")));
}

#[cfg(feature = "wasm-grammars")]
#[test]
fn plugin_chunks_never_carry_vault_metadata() {
    let registry = fixture_registry();
    // Vault-flavoured bait: a decision reference in the body and a
    // frontmatter-looking header. Authority metadata is core's alone, and a
    // plugin must have no way to mint it — here structurally, because plugins
    // never emit chunks at all.
    let uxml = uxml();
    let toydata =
        "--- \ndesign_status: decided\ndecision_refs: [D-0003]\n--- \nbody D-0004\n".repeat(4);
    for (name, src) in [
        ("Assets/UI/Main.uxml", uxml.as_str()),
        ("Assets/Level.toydata", toydata.as_str()),
    ] {
        let out = chunks(&registry, name, src);
        assert!(!out.is_empty(), "{name}");
        assert!(
            out.iter().all(|c| c.vault.is_none()),
            "{name} produced vault metadata"
        );
    }
}

#[cfg(feature = "wasm-grammars")]
#[test]
fn the_same_file_chunks_identically_from_several_threads() {
    // The store-per-thread claim, exercised: a `WasmStore` must never be
    // shared, so each thread builds its own on first use. If that were wrong
    // this is where it would show up — as a crash, a hang, or divergent trees.
    let registry = fixture_registry();
    let src = uxml();
    let expected = chunks(&registry, "Assets/UI/Main.uxml", &src);

    let outputs: Vec<Vec<Chunk>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let registry = &registry;
                let src = &src;
                scope.spawn(move || {
                    // Twice per thread, so the second pass goes through the
                    // thread-local parser cache rather than a fresh store.
                    let _ = chunks(registry, "Assets/UI/Main.uxml", src);
                    chunks(registry, "Assets/UI/Main.uxml", src)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for output in outputs {
        assert_eq!(output, expected, "a thread produced different chunks");
    }
}

#[test]
fn the_windows_strategy_uses_the_plugins_tighter_caps() {
    let registry = fixture_registry();
    let src: String = (0..10).map(|i| format!("line {i}\n")).collect();

    // The fixture declares window_lines = 4, overlap_lines = 1: lines 1-4,
    // 4-7, 7-10. Core's defaults (80/10) would make this one window.
    let out = chunks(&registry, "Assets/Level.toydata", &src);
    assert_eq!(
        out.len(),
        3,
        "{:?}",
        out.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
    assert_eq!(
        out.iter()
            .map(|c| (c.line_start, c.line_end))
            .collect::<Vec<_>>(),
        [(1, 4), (4, 7), (7, 10)]
    );
    assert_spans_exact(&out, &src);
    assert!(
        out.iter()
            .all(|c| matches!(c.kind, ChunkKind::Window { .. }))
    );
    // `language_tag` on a windows chunker is the only thing that can tell the
    // embedding header `toydata` from `text`.
    assert!(out.iter().all(|c| c.language.as_deref() == Some("toydata")));

    // The same content under an extension nobody claims takes core's geometry.
    let unclaimed = chunks(&registry, "Assets/Level.rst", &src);
    assert_eq!(unclaimed.len(), 1);
    assert!(unclaimed[0].language.is_none());
}

#[test]
fn a_plugins_file_size_cap_skips_rather_than_truncates() {
    let registry = fixture_registry();
    let big: String = (0..1200).map(|i| format!("line {i}\n")).collect();
    assert!(big.len() > 8192, "{}", big.len());

    let out = chunk(&registry, "Assets/Level.toydata", &big);
    assert_eq!(out.chunks.skip_reason(), Some(SkipReason::TooLarge));
    // Still attributed to the plugin: the daemon must be able to say *whose*
    // cap dropped the file.
    assert!(matches!(out.route, Route::Plugin { .. }), "{:?}", out.route);

    // Core's own 2 MiB ceiling is what an unclaimed file still gets.
    assert!(matches!(
        chunk(&registry, "Assets/Level.rst", &big).chunks,
        FileChunks::Chunked(_)
    ));
}

#[test]
fn the_machine_text_guard_still_fires_on_a_plugin_window_path() {
    // A serialized blob with one enormous line is token noise whoever claimed
    // the extension; the guard is a property of the window path, not of who
    // routed the file there.
    let registry = fixture_registry();
    let src = format!("head\n{}\n", "x".repeat(5000));
    assert!(src.len() < 8192);
    assert_eq!(
        chunk(&registry, "Assets/Level.toydata", &src)
            .chunks
            .skip_reason(),
        Some(SkipReason::MachineText)
    );
}

#[test]
fn a_plugin_that_cannot_run_falls_back_visibly() {
    // A grammar chunker whose asset is absent. Its files must still be
    // indexed — through the ordinary fallback — but the route has to say a
    // named plugin was expected, because `status` counts exactly this.
    let dir = TempDir::new().unwrap();
    let dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    let root = dir.join("ghost");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("lore-plugin.toml"),
        "[plugin]\nname = \"ghost\"\n\n[[chunker]]\nextensions = [\"uxml\"]\n\
         strategy = \"grammar\"\ngrammar = \"absent.wasm\"\nlanguage_tag = \"xml\"\n\
         containers = [\"element\"]\n",
    )
    .unwrap();
    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");

    let src = uxml();
    let out = chunk(&registry, "Assets/UI/Main.uxml", &src);
    let Route::FellBack { plugin, reason } = &out.route else {
        panic!("expected a visible fallback, got {:?}", out.route)
    };
    assert_eq!(plugin, "ghost");
    assert!(!reason.to_string().is_empty());

    // ...and the file is chunked, by the same path an unclaimed file takes.
    let FileChunks::Chunked(fallen_back) = out.chunks else {
        panic!("a fallback still indexes the file")
    };
    assert!(fallen_back.iter().all(|c| c.language.is_none()));
    assert_eq!(
        FileChunks::Chunked(fallen_back),
        chunk_file(Utf8Path::new("Assets/UI/Main.uxml"), src.as_bytes(), None)
    );
}

#[test]
fn files_no_plugin_claims_chunk_exactly_as_they_did_before_plugins() {
    // The whole feature rests on this: installing a plugin must not move a
    // single chunk it does not own. Compared field by field, against the
    // pre-plugin entry point, over one file per built-in path plus the
    // unknown-text fallback.
    let registry = fixture_registry();
    assert!(!registry.is_empty(), "with a real registry loaded");

    let long_run: String = (0..200).map(|i| format!("line {i}\n")).collect();
    let cases: Vec<(&str, String)> = vec![
        (
            "design/3.1.md",
            "---\ndesign_status: decided\ndecision_refs: [D-0003]\n---\n\n\
             # Ranking\n\nBody citing D-0004.\n\n## Fusion\n\nMore prose.\n"
                .to_string(),
        ),
        (
            "Assets/Board.cs",
            "namespace Game {\n  class Board {\n    void Update() { Step(); }\n  }\n}\n"
                .to_string(),
        ),
        (
            "src/lib.rs",
            "pub fn alpha() -> u32 {\n    41\n}\n\npub struct S;\n".to_string(),
        ),
        ("notes.rst", long_run),
        ("empty.txt", String::new()),
        ("README", "no extension at all\n".to_string()),
    ];

    for (name, src) in &cases {
        let before = chunk_file(Utf8Path::new(name), src.as_bytes(), Some(Profile::LoreV1));
        let after = chunk_file_with(
            Utf8Path::new(name),
            src.as_bytes(),
            Some(Profile::LoreV1),
            Some(&registry),
        );
        assert_eq!(after.route, Route::Builtin, "{name} was routed elsewhere");
        assert_eq!(before, after.chunks, "{name} chunked differently");
    }
}
