//! Chunker-plugin registry: what it loads, what it refuses, and what it says
//! about both.
//!
//! The registry's whole job is to be *loud and deterministic*. A broken plugin
//! costs its own extensions and nothing else; a contested extension goes to
//! nobody; a built-in extension is never on the table. Every one of those is a
//! diagnostic a human can act on, not a silent fallback.

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::TempDir;

use lore::plugin::{Diagnostic, PluginRegistry, Unavailable};

/// The plugin fixture checked into the test tree: a `uxml` grammar chunker
/// backed by a real 47 KB `tree-sitter-xml.wasm`, plus a `toydata` windows
/// chunker with tightened caps.
fn fixture_dir() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins")
}

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("test paths are UTF-8")
}

/// Writes a plugin root under `dir` with the given manifest text.
fn write_plugin(dir: &Utf8Path, folder: &str, manifest: &str) -> Utf8PathBuf {
    let root = dir.join(folder);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("lore-plugin.toml"), manifest).unwrap();
    root
}

/// A copy of the checked-in fixture, so a test can mutate it.
fn copy_fixture(into: &Utf8Path) -> Utf8PathBuf {
    let root = into.join("toy");
    std::fs::create_dir_all(&root).unwrap();
    for file in ["lore-plugin.toml", "xml.wasm"] {
        std::fs::copy(fixture_dir().join("toy").join(file), root.join(file)).unwrap();
    }
    root
}

const WINDOWS_ONLY: &str = "[plugin]\nname = \"other\"\n\n[[chunker]]\n\
                            extensions = [\"toydata\"]\nstrategy = \"windows\"\n";

#[test]
fn the_fixture_loads_and_says_whether_its_grammar_can_actually_run() {
    let (registry, diagnostics) = PluginRegistry::load(&fixture_dir());

    assert_eq!(registry.plugins().len(), 1);
    assert_eq!(registry.plugins()[0].name, "toy");
    let claim = registry
        .claim(Utf8Path::new("Assets/UI/Main.uxml"))
        .expect("the fixture claims uxml");
    assert_eq!(claim.plugin, "toy");

    // The manifest parses identically in both builds — declaring a grammar
    // chunker is never a parse error. Only its *availability* differs, and it
    // differs with a typed reason rather than a missing claim.
    if cfg!(feature = "wasm-grammars") {
        assert_eq!(claim.unavailable(), None);
        assert_eq!(diagnostics, vec![], "a good plugin is a quiet plugin");
    } else {
        assert_eq!(claim.unavailable(), Some(&Unavailable::WasmUnsupported));
        assert!(
            matches!(
                diagnostics.as_slice(),
                [Diagnostic::GrammarUnavailable { plugin, .. }] if plugin == "toy"
            ),
            "{diagnostics:?}"
        );
    }

    // The windows chunker is unaffected by the wasm feature either way.
    let windows = registry
        .claim(Utf8Path::new("Assets/Level.toydata"))
        .expect("the fixture claims toydata");
    assert_eq!(windows.unavailable(), None);
    assert_eq!(windows.max_file_bytes(), Some(8192));
}

#[test]
fn extensions_are_matched_case_folded_and_only_where_declared() {
    let (registry, _) = PluginRegistry::load(&fixture_dir());
    assert!(registry.claim(Utf8Path::new("A.UXML")).is_some());
    assert!(registry.claim(Utf8Path::new("A.uXmL")).is_some());
    assert!(registry.claim(Utf8Path::new("A.uxm")).is_none());
    assert!(registry.claim(Utf8Path::new("uxml")).is_none());
    assert!(registry.claim(Utf8Path::new("A.md")).is_none());
}

#[test]
fn a_missing_plugin_directory_is_not_an_error() {
    let dir = TempDir::new().unwrap();
    let (registry, diagnostics) = PluginRegistry::load(&utf8(&dir).join("does-not-exist"));
    assert!(registry.is_empty());
    assert_eq!(diagnostics, vec![]);
    // And an empty registry routes nothing, which is exactly no registry.
    assert!(registry.claim(Utf8Path::new("a.uxml")).is_none());
}

#[test]
fn the_fingerprint_moves_with_the_manifest_and_with_every_asset() {
    let dir = TempDir::new().unwrap();
    let root = copy_fixture(&utf8(&dir));
    let fingerprint = |dir: &Utf8Path| {
        let (registry, _) = PluginRegistry::load(dir);
        registry.plugins()[0].fingerprint.clone()
    };

    let before = fingerprint(&utf8(&dir));
    // Deterministic: the same bytes hash the same, run to run.
    assert_eq!(before, fingerprint(&utf8(&dir)));
    // ...and equal to the checked-in fixture's, since the bytes are the same.
    assert_eq!(before, fingerprint(&fixture_dir()));

    // A manifest edit that changes no behaviour at all still moves it: the
    // fingerprint is over bytes, because deciding which edits "matter" is
    // exactly the bookkeeping a content hash exists to avoid.
    let manifest = root.join("lore-plugin.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n# a comment\n")).unwrap();
    let after_manifest = fingerprint(&utf8(&dir));
    assert_ne!(before, after_manifest);

    // An asset edit moves it too, which is what makes "the author rebuilt the
    // grammar" re-chunk the files that grammar owns.
    let mut wasm = std::fs::read(root.join("xml.wasm")).unwrap();
    wasm.push(0);
    std::fs::write(root.join("xml.wasm"), &wasm).unwrap();
    assert_ne!(after_manifest, fingerprint(&utf8(&dir)));

    // A *missing* asset hashes distinctly from a present one, so installing
    // the artifact later re-chunks the files that fell back without it.
    std::fs::remove_file(root.join("xml.wasm")).unwrap();
    let absent = fingerprint(&utf8(&dir));
    assert_ne!(absent, after_manifest);
    assert_ne!(absent, before);
}

#[test]
fn a_referenced_asset_that_is_missing_costs_only_that_chunker() {
    let dir = TempDir::new().unwrap();
    let root = copy_fixture(&utf8(&dir));
    std::fs::remove_file(root.join("xml.wasm")).unwrap();

    let (registry, diagnostics) = PluginRegistry::load(&utf8(&dir));
    let claim = registry.claim(Utf8Path::new("a.uxml")).unwrap();
    assert!(
        matches!(claim.unavailable(), Some(Unavailable::AssetUnreadable { path, .. }) if path == "xml.wasm"),
        "{:?}",
        claim.unavailable()
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::GrammarUnavailable { .. })),
        "{diagnostics:?}"
    );
    // The plugin's *other* chunker is untouched: a broken grammar is not a
    // broken plugin.
    assert_eq!(
        registry.claim(Utf8Path::new("a.toydata")).unwrap().plugin,
        "toy"
    );
}

#[test]
fn a_grammar_that_is_not_a_grammar_is_rejected_without_a_panic() {
    let dir = TempDir::new().unwrap();
    let root = copy_fixture(&utf8(&dir));
    // Structurally valid wasm (magic + version, no sections), exporting none
    // of the symbols tree-sitter needs.
    std::fs::write(
        root.join("xml.wasm"),
        [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
    )
    .unwrap();

    let (registry, diagnostics) = PluginRegistry::load(&utf8(&dir));
    let claim = registry.claim(Utf8Path::new("a.uxml")).unwrap();
    let reason = claim.unavailable().expect("junk is not a grammar");
    if cfg!(feature = "wasm-grammars") {
        assert!(matches!(reason, Unavailable::Rejected { .. }), "{reason:?}");
    } else {
        assert_eq!(reason, &Unavailable::WasmUnsupported);
    }
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::GrammarUnavailable { .. }))
    );
}

#[test]
fn a_bad_manifest_costs_its_own_plugin_and_no_other() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    write_plugin(&dir, "broken", "[plugin]\nname = \"broken\"\nwat = 1\n");
    write_plugin(&dir, "good", WINDOWS_ONLY);
    // A directory that is not a plugin at all is still worth saying out loud —
    // a half-installed plugin otherwise presents as one that does nothing.
    std::fs::create_dir_all(dir.join("empty")).unwrap();

    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert_eq!(registry.plugins().len(), 1, "the good plugin still loaded");
    assert_eq!(registry.plugins()[0].name, "other");
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| matches!(d, Diagnostic::Manifest { .. }))
            .count(),
        2,
        "{diagnostics:?}"
    );
    assert!(
        diagnostics.iter().any(|d| d.to_string().contains("wat")),
        "the diagnostic names the offending key: {diagnostics:?}"
    );
}

#[test]
fn two_plugins_claiming_one_extension_leaves_it_to_neither() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let entry = |name: &str| {
        format!(
            "[plugin]\nname = \"{name}\"\n\n[[chunker]]\nextensions = [\"toydata\", \"{name}\"]\n\
             strategy = \"windows\"\n"
        )
    };
    write_plugin(&dir, "a", &entry("alpha"));
    write_plugin(&dir, "b", &entry("beta"));

    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert!(
        registry.claim(Utf8Path::new("x.toydata")).is_none(),
        "a contested extension belongs to nobody"
    );
    // The uncontested extensions each plugin also claimed still work.
    assert_eq!(
        registry.claim(Utf8Path::new("x.alpha")).unwrap().plugin,
        "alpha"
    );
    assert_eq!(
        registry.claim(Utf8Path::new("x.beta")).unwrap().plugin,
        "beta"
    );
    assert!(
        matches!(
            diagnostics.as_slice(),
            [Diagnostic::ExtensionConflict { extension, plugins }]
                if extension == "toydata" && plugins == &["alpha".to_string(), "beta".to_string()]
        ),
        "{diagnostics:?}"
    );

    // A third claimant does not quietly inherit what the first two lost.
    write_plugin(&dir, "c", &entry("gamma"));
    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert!(registry.claim(Utf8Path::new("x.toydata")).is_none());
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
}

#[test]
fn a_plugin_can_never_claim_a_built_in_extension() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    // `md` in particular: frontmatter authority extraction is core's, and a
    // plugin that could claim it could launder authority metadata.
    for (folder, extension) in [("m", "md"), ("c", "cs"), ("t", "tsx")] {
        write_plugin(
            &dir,
            folder,
            &format!(
                "[plugin]\nname = \"{folder}\"\n\n[[chunker]]\n\
                 extensions = [\"{extension}\", \"{folder}only\"]\nstrategy = \"windows\"\n"
            ),
        );
    }

    let (registry, diagnostics) = PluginRegistry::load(&dir);
    for extension in ["md", "cs", "tsx"] {
        assert!(
            registry.claim_extension(extension).is_none(),
            "{extension} was claimable"
        );
    }
    // The whole entry is dropped, not just the offending extension: a chunker
    // that misunderstood the contract does not get to keep half of it.
    for extension in ["monly", "conly", "tonly"] {
        assert!(
            registry.claim_extension(extension).is_none(),
            "{extension} survived a voided entry"
        );
    }
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| matches!(d, Diagnostic::BuiltinExtension { .. }))
            .count(),
        3,
        "{diagnostics:?}"
    );
}

/// Per-project opt-in is a *filter over the loaded registry*, not a second
/// load: what a project enables decides which of the installed plugins can
/// claim its files, and nothing else about them changes.
#[test]
fn enabling_narrows_the_registry_without_reloading_anything() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    copy_fixture(&dir);
    write_plugin(
        &dir,
        "other",
        WINDOWS_ONLY.replace("toydata", "otherdata").as_str(),
    );
    let (registry, _) = PluginRegistry::load(&dir);
    assert_eq!(registry.plugins().len(), 2);

    let enabled =
        |names: &[&str]| registry.enabled_only(&names.iter().map(|n| n.to_string()).collect());

    // Nothing enabled is exactly no registry: every claim disappears, which is
    // what makes an un-opted-in project chunk byte-identically to one on a
    // daemon with no plugins installed at all.
    let none = enabled(&[]);
    assert!(none.is_empty());
    assert!(none.claim(Utf8Path::new("a.uxml")).is_none());
    assert!(none.claim(Utf8Path::new("a.otherdata")).is_none());

    // One of two: its own extensions still route, the other plugin's do not.
    let toy = enabled(&["toy"]);
    assert_eq!(toy.plugins().len(), 1);
    let claim = toy.claim(Utf8Path::new("a.uxml")).expect("toy is enabled");
    assert_eq!(claim.plugin, "toy");
    // The fingerprint is the same object the full registry reports — the
    // filter must not disturb the identity the content stamp folds in.
    assert_eq!(claim.fingerprint, registry.get("toy").unwrap().fingerprint);
    assert_eq!(toy.claim(Utf8Path::new("a.toydata")).unwrap().plugin, "toy");
    assert!(toy.claim(Utf8Path::new("a.otherdata")).is_none());

    // The *second* plugin's extension map survives the renumbering, which is
    // the one thing a filtered index map can plausibly get wrong.
    let other = enabled(&["other"]);
    assert_eq!(
        other.claim(Utf8Path::new("a.otherdata")).unwrap().plugin,
        "other"
    );
    assert!(other.claim(Utf8Path::new("a.uxml")).is_none());

    // Enabling a name nobody installed is not an error and not a claim.
    let missing = enabled(&["toy", "unity"]);
    assert_eq!(missing.plugins().len(), 1);
    assert!(missing.claim(Utf8Path::new("a.unity")).is_none());
}

/// `lore plugin add` validates a candidate by loading it exactly as the daemon
/// would, so the two can never disagree about what a plugin is.
#[test]
fn a_single_root_loads_or_says_why_in_the_registrys_own_words() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let root = copy_fixture(&dir);

    let plugin = lore::plugin::Plugin::load(&root).expect("the fixture is a plugin");
    assert_eq!(plugin.name, "toy");
    assert_eq!(plugin.extensions(), ["uxml", "toydata"]);
    // Identical to what the directory load reports, byte for byte.
    let (registry, _) = PluginRegistry::load(&dir);
    assert_eq!(plugin.fingerprint, registry.get("toy").unwrap().fingerprint);

    let broken = write_plugin(&dir, "broken", "[plugin]\nname = \"broken\"\nwat = 1\n");
    let err = lore::plugin::Plugin::load(&broken).unwrap_err();
    assert!(err.to_string().contains("wat"), "{err}");
    let absent = lore::plugin::Plugin::load(&dir.join("nope")).unwrap_err();
    assert!(absent.to_string().contains("lore-plugin.toml"), "{absent}");
}

#[test]
fn two_plugins_may_not_share_a_name() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    write_plugin(&dir, "first", WINDOWS_ONLY);
    write_plugin(
        &dir,
        "second",
        &WINDOWS_ONLY.replace("\"toydata\"", "\"otherdata\""),
    );

    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert_eq!(registry.plugins().len(), 1);
    // Roots are visited in sorted order, so which one loses is the same on
    // every machine.
    assert!(registry.claim_extension("toydata").is_some());
    assert!(registry.claim_extension("otherdata").is_none());
    assert!(
        matches!(
            diagnostics.as_slice(),
            [Diagnostic::DuplicateName { name, .. }] if name == "other"
        ),
        "{diagnostics:?}"
    );
}
