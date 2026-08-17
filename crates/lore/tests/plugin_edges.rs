//! Chunker plugins at their edges: the inputs a plugin author does not write
//! on purpose, and the ones a hostile author writes on purpose.
//!
//! `plugin_registry.rs` and `plugin_chunking.rs` establish that the mechanism
//! works. This file asks the next question — what happens at the boundary, in
//! the wrong case, one byte over, in the wrong order — because every one of
//! those answers is load-bearing:
//!
//! * an extension read from anything but the file name would route a file by
//!   its *directory*;
//! * a bound that admits one byte too many is a bound the manifest can walk
//!   past;
//! * a registry whose answer depends on directory iteration order gives two
//!   machines two different indexes of one repository;
//! * and `.md` staying core's — by every route a manifest can take — is the
//!   single reason the privileged set exists (2026-08-17 contract, "Routing and
//!   precedence").

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::TempDir;

use lore::chunk::{
    FileChunks, MAX_FILE_BYTES, Route, SkipReason, TEXT_WINDOW_LINES, WINDOW_OVERLAP_LINES,
    chunk_file, chunk_file_with,
};
use lore::daemon::index::content_stamp;
use lore::plugin::{Diagnostic, PluginRegistry, Unavailable};
use lore::repo_config::Profile;
use lore::types::{Chunk, ChunkKind};

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("test paths are UTF-8")
}

fn write_plugin(dir: &Utf8Path, folder: &str, manifest: &str) -> Utf8PathBuf {
    let root = dir.join(folder);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("lore-plugin.toml"), manifest).unwrap();
    root
}

/// A windows-strategy plugin claiming `extensions`. The one strategy that
/// behaves identically with and without `wasm-grammars`, so everything built on
/// it asserts the same thing in both builds.
fn windows_manifest(name: &str, extensions: &[&str]) -> String {
    let list = extensions
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[plugin]\nname = \"{name}\"\n\n[[chunker]]\nextensions = [{list}]\n\
         strategy = \"windows\"\nwindow_lines = 4\noverlap_lines = 1\n\
         language_tag = \"{name}data\"\n"
    )
}

/// One plugin in its own directory, loaded as the daemon loads it.
fn one_plugin(dir: &Utf8Path, manifest: &str) -> (PluginRegistry, Vec<Diagnostic>) {
    write_plugin(dir, "p", manifest);
    PluginRegistry::load(dir)
}

fn lines(count: usize) -> String {
    (0..count).map(|i| format!("line {i}\n")).collect()
}

fn chunks_of(registry: &PluginRegistry, name: &str, src: &str) -> Vec<Chunk> {
    match chunk_file_with(
        Utf8Path::new(name),
        src.as_bytes(),
        Some(Profile::LoreV1),
        Some(registry),
    )
    .chunks
    {
        FileChunks::Chunked(chunks) => chunks,
        other => panic!("{name}: unexpected skip: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Which file an extension actually names
// ---------------------------------------------------------------------------

/// A claim is answered from the file name's final extension and nothing else.
///
/// This matters more than it looks: the claim is taken from the path *before
/// the file is read*, and it decides both the routing and the content stamp. A
/// claim that read a directory component would route every file under
/// `Assets/toydata/` through a plugin that has never seen one.
#[test]
fn an_extension_is_read_from_the_file_name_and_from_nothing_else() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let (registry, diagnostics) = one_plugin(&dir, &windows_manifest("toy", &["toydata"]));
    assert_eq!(diagnostics, vec![]);

    for claimed in [
        "Assets/Level.toydata",
        // Case-folded, which is what makes the manifest's lowercase-only rule
        // safe on a case-insensitive filesystem.
        "Assets/LEVEL.TOYDATA",
        "Assets/Level.ToyData",
        // Only the *last* extension counts.
        "Assets/Level.bak.toydata",
        // A directory named after the extension changes nothing either way.
        "toydata/Level.toydata",
    ] {
        assert!(
            registry.claim(Utf8Path::new(claimed)).is_some(),
            "{claimed} was not claimed"
        );
    }

    for unclaimed in [
        // A dotfile is all stem and no extension.
        ".toydata",
        "Assets/.toydata",
        // The extension is not the whole file name.
        "toydata",
        "Assets/toydata",
        // A directory component is not a file name.
        "Assets/toydata/notes.txt",
        "Assets/Level.toydata/notes.txt",
        // A trailing extension that is not the plugin's.
        "Assets/Level.toydata.bak",
        // No extension at all.
        "Assets/README",
    ] {
        assert!(
            registry.claim(Utf8Path::new(unclaimed)).is_none(),
            "{unclaimed} was claimed"
        );
    }
}

/// The uppercase spelling routes, chunks and *stamps* as the lowercase one.
///
/// Three code paths lowercase independently — the registry, `chunk_file_with`,
/// and the content stamp — and a file whose stamp disagreed with the chunker
/// that produced it would re-chunk on every pass forever.
#[test]
fn an_uppercase_extension_takes_the_same_route_and_the_same_stamp() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let (registry, _) = one_plugin(&dir, &windows_manifest("toy", &["toydata"]));
    let src = lines(10);

    let shouty = chunk_file_with(
        Utf8Path::new("Assets/LEVEL.TOYDATA"),
        src.as_bytes(),
        None,
        Some(&registry),
    );
    assert!(
        matches!(&shouty.route, Route::Plugin { plugin, .. } if plugin == "toy"),
        "{:?}",
        shouty.route
    );
    // Same geometry and same language tag as the lowercase spelling: only the
    // path differs, and the path is not part of the routing decision.
    let quiet = chunks_of(&registry, "Assets/level.toydata", &src);
    assert_eq!(shouty.chunks.chunks().len(), quiet.len());
    assert!(
        shouty
            .chunks
            .chunks()
            .iter()
            .all(|c| c.language.as_deref() == Some("toydata"))
    );

    let hash = "abc123";
    let stamp = content_stamp(
        Utf8Path::new("Assets/LEVEL.TOYDATA"),
        hash,
        None,
        Some(&registry),
    );
    assert!(stamp.contains("+toy@"), "{stamp}");
    assert_eq!(
        stamp,
        content_stamp(
            Utf8Path::new("Assets/level.toydata"),
            hash,
            None,
            Some(&registry)
        ),
        "the case of an extension must not change a stamp"
    );
}

// ---------------------------------------------------------------------------
// The `.md` wall
// ---------------------------------------------------------------------------

/// Markdown is core's by every route a manifest can take, and a plugin that
/// gets anywhere near it cannot mint authority metadata even so.
///
/// This is the test the contract exists for. `design_status`, `decision_refs`
/// and the `D-NNNN` body scan are what `lore search` ranks by; a plugin that
/// could claim `.md` could declare its own documents canonical.
#[test]
fn no_manifest_route_lets_a_plugin_near_markdown_authority() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    // Route 1: claim `md` outright, beside an extension the plugin would
    // otherwise have got. The whole entry is voided, so it gets neither.
    write_plugin(
        &dir,
        "greedy",
        &windows_manifest("greedy", &["md", "mdextra"]),
    );
    // Route 2: claim a Markdown-*shaped* extension that is genuinely free.
    // Allowed — and still incapable of producing vault metadata, because
    // plugins never emit a chunk at all.
    write_plugin(
        &dir,
        "adjacent",
        &windows_manifest("adjacent", &["markdown"]),
    );
    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert!(
        diagnostics.iter().any(
            |d| matches!(d, Diagnostic::BuiltinExtension { extension, .. } if extension == "md")
        ),
        "{diagnostics:?}"
    );

    let vault_bait = "---\ndesign_status: decided\ndecision_refs: [D-0003]\n---\n\n\
                      # Ranking\n\nBody citing D-0004.\n";

    // Neither the claimed spelling nor the shouty one leaves core's hands, and
    // both keep the vault vocabulary that only core can extract.
    for path in ["design/3.1.md", "design/3.1.MD", "design/3.1.Md"] {
        let out = chunk_file_with(
            Utf8Path::new(path),
            vault_bait.as_bytes(),
            Some(Profile::LoreV1),
            Some(&registry),
        );
        assert_eq!(out.route, Route::Builtin, "{path}");
        let chunks = out.chunks.chunks();
        assert!(
            chunks.iter().any(|c| c.vault.is_some()),
            "{path} lost its vault metadata"
        );
        // Byte-identical to the same file on a daemon with no plugins at all.
        assert_eq!(
            out.chunks,
            chunk_file(
                Utf8Path::new(path),
                vault_bait.as_bytes(),
                Some(Profile::LoreV1)
            ),
            "{path} chunked differently with a plugin installed"
        );
    }

    // The voided entry's *other* extension is gone too — a chunker that
    // misunderstood the contract keeps none of it.
    assert!(registry.claim_extension("mdextra").is_none());

    // The adjacent claim does route... and produces nothing a ranking pass
    // could mistake for authority. This is the structural half of the wall:
    // there is no code path from a plugin to `VaultMeta`.
    let out = chunk_file_with(
        Utf8Path::new("notes.markdown"),
        vault_bait.as_bytes(),
        Some(Profile::LoreV1),
        Some(&registry),
    );
    assert!(
        matches!(&out.route, Route::Plugin { plugin, .. } if plugin == "adjacent"),
        "{:?}",
        out.route
    );
    assert!(out.chunks.chunks().iter().all(|c| c.vault.is_none()));
    assert!(
        out.chunks
            .chunks()
            .iter()
            .all(|c| matches!(c.kind, ChunkKind::Window { .. })),
        "a plugin's chunks are windows or code, never sections"
    );
}

/// The uppercase spelling of a built-in extension is refused at parse time,
/// before it can even reach the precedence rule: extensions are compared
/// case-folded, so `"MD"` could never match anything and pretending otherwise
/// would leave an author believing they had claimed something.
#[test]
fn an_uppercase_builtin_claim_is_refused_by_the_parser_itself() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    for spelling in ["MD", "Md", ".md"] {
        let root = write_plugin(&dir, "p", &windows_manifest("p", &[spelling]));
        let err = lore::plugin::Plugin::load(&root).unwrap_err().to_string();
        assert!(err.contains("extensions"), "{spelling}: {err}");
    }
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

/// Two machines with the same `plugins/` directory get the same registry: the
/// same winners, the same losers, the same diagnostics, in the same order.
///
/// Conflict resolution is order-sensitive *by construction* — a duplicate name
/// costs whoever comes second, and a contested extension is taken back off
/// whoever held it first — so the only thing standing between this feature and
/// two machines indexing one repository differently is that roots are visited
/// in sorted order. That is asserted here twice over: once against a directory
/// built in the reverse order, and once by pinning which claimant actually
/// loses, since a rule nobody states is a rule that can silently invert.
#[test]
fn the_registry_is_the_same_whatever_order_its_roots_arrived_in() {
    // Directory names deliberately disagree with plugin names, so a load that
    // sorted by either one would land somewhere different.
    let plugins: [(&str, String); 4] = [
        ("z-first", windows_manifest("alpha", &["shared", "onlya"])),
        ("m-second", windows_manifest("beta", &["shared", "onlyb"])),
        ("a-third", windows_manifest("gamma", &["onlyc"])),
        // A second plugin calling itself `gamma`: the duplicate-name path.
        ("q-fourth", windows_manifest("gamma", &["onlyd"])),
    ];

    let load = |order: &[usize]| {
        let dir = TempDir::new().unwrap();
        let path = utf8(&dir);
        for &at in order {
            write_plugin(&path, plugins[at].0, &plugins[at].1);
        }
        let (registry, diagnostics) = PluginRegistry::load(&path);
        let names: Vec<String> = registry
            .plugins()
            .iter()
            .map(|p| format!("{}:{}", p.name, p.fingerprint))
            .collect();
        let claims: Vec<String> = ["shared", "onlya", "onlyb", "onlyc", "onlyd"]
            .iter()
            .map(|ext| match registry.claim_extension(ext) {
                Some(claim) => format!("{ext}={}", claim.plugin),
                None => format!("{ext}=none"),
            })
            .collect();
        let said: Vec<String> = diagnostics
            .iter()
            // The root is a temp directory, and it reaches the message with the
            // platform's own separator.
            .map(|d| {
                d.to_string()
                    .replace(path.as_str(), "<dir>")
                    .replace('\\', "/")
            })
            .collect();
        (names, claims, said)
    };

    let forward = load(&[0, 1, 2, 3]);
    let reverse = load(&[3, 2, 1, 0]);
    let shuffled = load(&[2, 0, 3, 1]);
    assert_eq!(forward, reverse);
    assert_eq!(forward, shuffled);

    let (names, claims, said) = forward;
    // Sorted by *root*: a-third, m-second, q-fourth, z-first. So `gamma` is
    // held by a-third and it is q-fourth that loses the name.
    assert_eq!(
        names
            .iter()
            .map(|n| n.split(':').next().unwrap())
            .collect::<Vec<_>>(),
        ["gamma", "beta", "alpha"],
        "the surviving plugins, in root order"
    );
    assert_eq!(
        claims,
        [
            "shared=none",
            "onlya=alpha",
            "onlyb=beta",
            "onlyc=gamma",
            // The rejected duplicate takes its extensions with it.
            "onlyd=none",
        ]
    );
    assert_eq!(
        said,
        [
            "plugin at <dir>/q-fourth declares the name \"gamma\", which another plugin \
             already holds; it was not loaded",
            "plugins \"beta\", \"alpha\" all claim \"shared\"; none of them gets it",
        ]
    );
}

// ---------------------------------------------------------------------------
// Adversarial installations
// ---------------------------------------------------------------------------

/// A stray file in `plugins/` is not a half-installed plugin and must not be
/// reported as one. Only directories are candidates, so an archive left behind
/// by `lore plugin add` costs nothing and says nothing.
#[test]
fn a_file_sitting_in_the_plugins_directory_is_not_a_plugin() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    write_plugin(&dir, "toy", &windows_manifest("toy", &["toydata"]));
    std::fs::write(dir.join("toy.zip"), b"not a plugin").unwrap();
    std::fs::write(
        dir.join("lore-plugin.toml"),
        b"[plugin]\nname = \"loose\"\n",
    )
    .unwrap();

    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert_eq!(registry.plugins().len(), 1);
    assert_eq!(registry.plugins()[0].name, "toy");
    assert_eq!(diagnostics, vec![], "{diagnostics:?}");
}

/// A grammar asset that is a directory reads as an unreadable asset, not as a
/// panic and not as a grammar. The chunker still owns its extensions, which is
/// what keeps the fallback visible.
#[test]
fn an_asset_that_is_a_directory_is_unreadable_rather_than_fatal() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let root = write_plugin(
        &dir,
        "p",
        "[plugin]\nname = \"p\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
         strategy = \"grammar\"\ngrammar = \"grammars/xml.wasm\"\nlanguage_tag = \"xml\"\n\
         containers = [\"element\"]\n",
    );
    // The referenced *asset path* is itself a directory.
    std::fs::create_dir_all(root.join("grammars/xml.wasm")).unwrap();

    let (registry, diagnostics) = PluginRegistry::load(&dir);
    let claim = registry.claim(Utf8Path::new("a.toydata")).expect("claimed");
    assert!(
        matches!(
            claim.unavailable(),
            Some(Unavailable::AssetUnreadable { path, .. }) if path == "grammars/xml.wasm"
        ),
        "{:?}",
        claim.unavailable()
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::GrammarUnavailable { .. })),
        "{diagnostics:?}"
    );
}

/// A structurally valid wasm module — real type, function, code and export
/// sections, exporting exactly the `tree_sitter_<symbol>` name the manifest
/// asks for — is still refused, because a tree-sitter grammar must be a *side
/// module* carrying a `dylink.0` section.
///
/// The distinction matters: without the gate, any wasm module with the right
/// export name would be handed to the parser, and "a plugin is data, not code"
/// would be a naming convention rather than a property.
#[test]
fn a_valid_wasm_module_that_is_not_a_grammar_is_still_refused() {
    #[rustfmt::skip]
    const MODULE: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version 1
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,       // type: () -> i32
        0x03, 0x02, 0x01, 0x00,                         // func 0 has type 0
        // export "tree_sitter_toy" (15 bytes) as func 0
        0x07, 0x13, 0x01, 0x0f,
        b't', b'r', b'e', b'e', b'_', b's', b'i', b't', b't', b'e', b'r', b'_',
        b't', b'o', b'y',
        0x00, 0x00,
        0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x2a, 0x0b, // code: i32.const 42
    ];

    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let root = write_plugin(
        &dir,
        "p",
        "[plugin]\nname = \"p\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
         strategy = \"grammar\"\ngrammar = \"toy.wasm\"\nlanguage_tag = \"toy\"\n\
         containers = [\"element\"]\n",
    );
    std::fs::write(root.join("toy.wasm"), MODULE).unwrap();

    let (registry, diagnostics) = PluginRegistry::load(&dir);
    let claim = registry.claim(Utf8Path::new("a.toydata")).expect("claimed");
    let reason = claim
        .unavailable()
        .expect("a module without dylink.0 is not a grammar");
    if cfg!(feature = "wasm-grammars") {
        assert!(matches!(reason, Unavailable::Rejected { .. }), "{reason:?}");
    } else {
        assert_eq!(reason, &Unavailable::WasmUnsupported);
    }
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::GrammarUnavailable { .. })),
        "{diagnostics:?}"
    );

    // And the file is still indexed, by the ordinary fallback path.
    let src = lines(6);
    let out = chunk_file_with(
        Utf8Path::new("a.toydata"),
        src.as_bytes(),
        None,
        Some(&registry),
    );
    assert!(
        matches!(out.route, Route::FellBack { .. }),
        "{:?}",
        out.route
    );
    assert!(!out.chunks.chunks().is_empty());
}

// ---------------------------------------------------------------------------
// Boundaries
// ---------------------------------------------------------------------------

/// Caps that *equal* core's are accepted; one past is refused. Tighten-only
/// resolves the contract's open question as `<=`, and which side of the
/// boundary "equal" falls on is exactly the kind of thing that inverts under a
/// refactor without any test noticing.
#[test]
fn a_cap_equal_to_cores_is_a_tighter_cap() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let root = write_plugin(
        &dir,
        "p",
        &format!(
            "[plugin]\nname = \"p\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
             strategy = \"windows\"\nmax_file_bytes = {MAX_FILE_BYTES}\n\
             window_lines = {TEXT_WINDOW_LINES}\noverlap_lines = {WINDOW_OVERLAP_LINES}\n"
        ),
    );
    let plugin = lore::plugin::Plugin::load(&root).expect("equal is not above");
    assert_eq!(plugin.extensions(), ["toydata"]);
    let (registry, _) = PluginRegistry::load(&dir);
    assert_eq!(
        registry
            .claim(Utf8Path::new("a.toydata"))
            .unwrap()
            .max_file_bytes(),
        Some(MAX_FILE_BYTES)
    );
}

/// The file-size cap is a ceiling on the file, inclusive: a file of exactly
/// `max_file_bytes` is chunked and one byte more is skipped. A plugin cap that
/// was off by one would drop a whole class of files with no diagnostic.
#[test]
fn the_plugins_file_cap_admits_exactly_its_own_size() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let (registry, _) = one_plugin(
        &dir,
        "[plugin]\nname = \"p\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
         strategy = \"windows\"\nmax_file_bytes = 64\n",
    );

    let exact = "x".repeat(64);
    let over = "x".repeat(65);
    let chunk = |src: &str| {
        chunk_file_with(
            Utf8Path::new("a.toydata"),
            src.as_bytes(),
            None,
            Some(&registry),
        )
        .chunks
    };
    assert!(
        matches!(chunk(&exact), FileChunks::Chunked(_)),
        "{}",
        exact.len()
    );
    assert_eq!(chunk(&over).skip_reason(), Some(SkipReason::TooLarge));
}

/// Names and extensions are bounded, and the bound is inclusive on the side the
/// manifest documents. A manifest is untrusted input that reaches log lines and
/// the push wire, so "generous" has to still mean "bounded".
#[test]
fn names_and_extensions_stop_at_the_documented_length() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let long = |n: usize| "a".repeat(n);

    for (n, ok) in [(64, true), (65, false)] {
        let root = write_plugin(&dir, "n", &windows_manifest(&long(n), &["toydata"]));
        assert_eq!(
            lore::plugin::Plugin::load(&root).is_ok(),
            ok,
            "a {n}-byte name"
        );
        let root = write_plugin(&dir, "e", &windows_manifest("p", &[&long(n)]));
        assert_eq!(
            lore::plugin::Plugin::load(&root).is_ok(),
            ok,
            "a {n}-byte extension"
        );
    }
}

/// A manifest claiming hundreds of extensions is legal and stays coherent:
/// every claim resolves to its own plugin, and nothing about the size of the
/// list changes what a claim means.
#[test]
fn a_manifest_may_claim_a_great_many_extensions() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let many: Vec<String> = (0..500).map(|i| format!("toydata{i}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let (registry, diagnostics) = one_plugin(&dir, &windows_manifest("toy", &refs));
    assert_eq!(diagnostics, vec![]);
    assert_eq!(registry.plugins()[0].extensions().len(), 500);
    for ext in ["toydata0", "toydata250", "toydata499"] {
        assert_eq!(registry.claim_extension(ext).unwrap().plugin, "toy");
    }
    assert!(registry.claim_extension("toydata500").is_none());
}

/// A manifest that is not UTF-8 is refused by name, not by panic. The bytes
/// reach the parser straight off the disk, so this is the first thing a
/// corrupted or wrongly-encoded file hits.
#[test]
fn a_manifest_that_is_not_utf8_is_refused_by_name() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let root = dir.join("p");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("lore-plugin.toml"), [0xff, 0xfe, 0x00, 0x41]).unwrap();

    let err = lore::plugin::Plugin::load(&root).unwrap_err().to_string();
    assert!(err.contains("UTF-8"), "{err}");
    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert!(registry.is_empty());
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
}

// ---------------------------------------------------------------------------
// Conflicts and enablement
// ---------------------------------------------------------------------------

/// Conflict resolution happens at *install* scope, not enablement scope: an
/// extension two installed plugins claim belongs to nobody even in a project
/// that enabled only one of them.
///
/// The contract's sentence is "two **enabled** plugins claiming the same
/// extension is a loud registration error"; the implementation resolves the
/// conflict once, at load, for every project on the machine. The consequence a
/// user feels is the one asserted here — installing an unrelated plugin can
/// take an extension away from a project that was already using it — and it is
/// pinned rather than assumed, because the alternative (resolving per project)
/// would make which plugin wins depend on who asked.
#[test]
fn a_contested_extension_stays_contested_even_when_one_claimant_is_enabled() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    write_plugin(&dir, "a", &windows_manifest("alpha", &["shared"]));
    write_plugin(&dir, "b", &windows_manifest("beta", &["shared"]));
    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");

    let only_alpha = registry.enabled_only(&["alpha".to_string()].into_iter().collect());
    assert_eq!(only_alpha.plugins().len(), 1);
    assert!(
        only_alpha.claim(Utf8Path::new("a.shared")).is_none(),
        "enabling one claimant does not settle a conflict the other caused"
    );
    // ...and the file is chunked as if no plugin existed, not as a fallback:
    // nobody claims it, so there is nothing to report.
    let src = lines(10);
    let out = chunk_file_with(
        Utf8Path::new("a.shared"),
        src.as_bytes(),
        None,
        Some(&only_alpha),
    );
    assert_eq!(out.route, Route::Builtin);
    assert_eq!(
        out.chunks,
        chunk_file(Utf8Path::new("a.shared"), src.as_bytes(), None)
    );
}

// ---------------------------------------------------------------------------
// What a plugin's chunks carry
// ---------------------------------------------------------------------------

/// A `windows`-strategy chunk carries the plugin's language tag and no symbol
/// path — and deliberately no [`lore::types::WindowFamily`], because
/// consecutive windows of a serialized asset are different content rather than
/// one split span, and collapsing them would hide half the file from a search.
#[test]
fn plugin_windows_carry_a_language_tag_and_no_collapsible_family() {
    let dir = TempDir::new().unwrap();
    let dir = utf8(&dir);
    let (registry, _) = one_plugin(&dir, &windows_manifest("toy", &["toydata"]));
    let out = chunks_of(&registry, "Assets/Level.toydata", &lines(20));

    assert!(out.len() > 1, "{}", out.len());
    for chunk in &out {
        assert_eq!(chunk.language.as_deref(), Some("toydata"));
        assert!(
            matches!(chunk.kind, ChunkKind::Window { .. }),
            "{:?}",
            chunk.kind
        );
        assert_eq!(chunk.kind.window_family(), None);
    }
    // Window ordinals are per-file and consecutive, which is what the anchor —
    // and therefore the chunk id — is derived from.
    let indices: Vec<u32> = out
        .iter()
        .map(|c| match c.kind {
            ChunkKind::Window { index } => index,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(indices, (0..out.len() as u32).collect::<Vec<_>>());
}

/// An oversized span produced by a *plugin's* grammar splits into one window
/// family, exactly as a built-in grammar's does.
///
/// Ranking collapses a family to a single result, and it can only do that from
/// the family recorded at chunk time. A plugin whose split spans carried no
/// family would put every window of one big element in the results separately —
/// the duplicate-result bug `CHUNK_FORMAT_VERSION` 4 exists to fix, reopened for
/// every plugin-routed file.
#[cfg(feature = "wasm-grammars")]
#[test]
fn an_oversized_plugin_span_splits_into_one_collapsible_family() {
    let fixtures = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins");
    let (registry, diagnostics) = PluginRegistry::load(&fixtures);
    assert_eq!(diagnostics, vec![], "the fixture plugin must load cleanly");

    // One element far past core's 4 KB chunk ceiling, with no inner structure
    // to split on: the only way to chunk it is to window it.
    let body = "        <!-- padding text, long enough to be worth splitting -->\n".repeat(200);
    let src = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ui:UXML xmlns:ui=\"UnityEngine.UIElements\">\n\
         {body}</ui:UXML>\n"
    );
    let out = chunks_of(&registry, "Assets/UI/Big.uxml", &src);

    let families: Vec<_> = out.iter().filter_map(|c| c.kind.window_family()).collect();
    assert!(
        families.len() > 1,
        "the oversized span did not split: {:?}",
        out.iter()
            .map(|c| (&c.kind, c.text.len()))
            .collect::<Vec<_>>()
    );
    // One span, one family, consecutive indices — that is what collapse keys on.
    let first = families[0].family;
    assert!(families.iter().all(|f| f.family == first), "{families:?}");
    assert_eq!(
        families.iter().map(|f| f.index).collect::<Vec<_>>(),
        (0..families.len() as u32).collect::<Vec<_>>()
    );
    // The family is bookkeeping about an anchor, never part of it: ids stay
    // derived from path, anchor and text.
    for chunk in &out {
        assert_eq!(
            chunk.id,
            Chunk::derive_id(&chunk.path, &chunk.kind, &chunk.text)
        );
        assert_eq!(
            &src[chunk.byte_start as usize..chunk.byte_end as usize],
            chunk.text
        );
    }
}
