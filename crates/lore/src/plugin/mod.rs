//! Chunker plugins: a manifest plus assets, interpreted by the two chunking
//! engines core already has.
//!
//! Per the 2026-08-17 contract, **a plugin is data, not code**. It cannot emit
//! a chunk, mint an id, build an embedding header or set authority metadata,
//! because it never emits anything — [`crate::chunk`] does all of it. That is
//! structural rather than policed: everything in this module produces is a
//! *routing decision* and a *parameterization*, and both feed the existing
//! walker and windower.
//!
//! What lives here:
//!
//! * [`manifest`] — `lore-plugin.toml`, parsed strictly.
//! * [`grammar`] — runtime `.wasm` grammar loading and the per-thread parser
//!   pool, behind the `wasm-grammars` Cargo feature.
//! * [`PluginRegistry`] — a directory of plugin roots resolved into "which
//!   plugin owns this extension", with the conflicts reported rather than
//!   silently resolved.
//!
//! # Fingerprints, not versions
//!
//! A plugin declares no version. Its identity is [`Plugin::fingerprint`]: a
//! blake3 hash over the manifest bytes and every asset it references, in a
//! deterministic order. The indexer folds that into the per-file content hash
//! of the files the plugin owns, so editing a plugin re-chunks exactly those
//! files and nothing else — with no version bookkeeping an author can forget
//! to do.
//!
//! # Precedence
//!
//! Built-ins win every conflict, unconditionally ([`crate::chunk::is_builtin_extension`]);
//! `.md` in particular is never claimable, because frontmatter and authority
//! extraction are core's and laundering authority metadata through a plugin is
//! the thing that must be impossible. Two plugins claiming one extension is a
//! loud registration error and **neither** gets it: a precedence rule there
//! would make which plugin wins depend on directory iteration order.
//!
//! That contest is settled **per registry view**, not once per machine. A
//! plugin's [`LoadedChunker::extensions`] are what it *claims*; which claim
//! wins is a property of the set it is resolved against ([`resolve`]), and the
//! set that governs a project is the one that project enabled. So two installed
//! plugins fighting over `unity` leaves the extension to neither *machine-wide*
//! — reported at load, as before — while a project that enabled only one of
//! them routes to it. Resolving at install scope instead would let installing
//! an unrelated plugin strip an extension from a project already using the
//! other claimant, which is the exact opposite of "installing a plugin
//! re-chunks nothing until a repository enables it".

pub mod grammar;
pub mod manifest;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

pub use grammar::Grammar;
pub use manifest::{Chunker as ManifestChunker, GrammarChunker, MANIFEST_FILE, WindowsChunker};

/// Why a declared chunker cannot run. Typed, because `lore status` reports it
/// and "the plugin did not work" is not a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// Built without the `wasm-grammars` feature. The manifest is fine; this
    /// binary simply carries no wasm engine.
    WasmUnsupported,
    /// The referenced asset could not be read.
    AssetUnreadable { path: Utf8PathBuf, detail: String },
    /// The wasm engine itself refused to start.
    EngineFailed { detail: String },
    /// tree-sitter refused the artifact — not wasm, no `dylink.0` section, or
    /// missing the `tree_sitter_<symbol>` export.
    Rejected { kind: String, detail: String },
    /// A valid grammar, but built for an ABI this runtime does not accept.
    AbiUnsupported { abi: usize, min: usize, max: usize },
    /// Loaded, but not wasm-backed — the loader's own invariant broke.
    NotWasm,
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unavailable::WasmUnsupported => {
                write!(f, "this build has no wasm engine (feature `wasm-grammars`)")
            }
            Unavailable::AssetUnreadable { path, detail } => {
                write!(f, "asset `{path}` is unreadable: {detail}")
            }
            Unavailable::EngineFailed { detail } => write!(f, "the wasm engine failed: {detail}"),
            Unavailable::Rejected { kind, detail } => {
                write!(f, "tree-sitter rejected the grammar ({kind}): {detail}")
            }
            Unavailable::AbiUnsupported { abi, min, max } => write!(
                f,
                "grammar ABI {abi} is outside this runtime's {min}..={max} window"
            ),
            Unavailable::NotWasm => write!(f, "the loaded grammar is not wasm-backed"),
        }
    }
}

/// Something the registry refused, or accepted only partly. Never fatal: a
/// broken plugin costs its own extensions and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// A plugin root whose manifest is missing, unreadable or invalid.
    Manifest { root: Utf8PathBuf, detail: String },
    /// A second plugin declaring a name already taken.
    DuplicateName { name: String, root: Utf8PathBuf },
    /// A chunker claiming an extension a built-in owns. The whole entry is
    /// dropped, so a plugin cannot half-claim `.md`.
    BuiltinExtension { plugin: String, extension: String },
    /// Two plugins claim one extension; neither gets it.
    ExtensionConflict {
        extension: String,
        plugins: Vec<String>,
    },
    /// A grammar chunker registered but cannot run. It still owns its
    /// extensions — that is what makes the fallback *visible* instead of
    /// looking like a file nobody ever claimed.
    GrammarUnavailable {
        plugin: String,
        grammar: Utf8PathBuf,
        reason: Unavailable,
    },
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Diagnostic::Manifest { root, detail } => {
                write!(f, "plugin at {root} was not loaded: {detail}")
            }
            Diagnostic::DuplicateName { name, root } => write!(
                f,
                "plugin at {root} declares the name \"{name}\", which another plugin \
                 already holds; it was not loaded"
            ),
            Diagnostic::BuiltinExtension { plugin, extension } => write!(
                f,
                "plugin \"{plugin}\" claims \"{extension}\", which a built-in chunker owns; \
                 that chunker entry was ignored"
            ),
            Diagnostic::ExtensionConflict { extension, plugins } => write!(
                f,
                "plugins {} all claim \"{extension}\"; none of them gets it",
                plugins
                    .iter()
                    .map(|p| format!("\"{p}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Diagnostic::GrammarUnavailable {
                plugin,
                grammar,
                reason,
            } => write!(
                f,
                "plugin \"{plugin}\" grammar `{grammar}` is unavailable, so its files fall \
                 back to line windows: {reason}"
            ),
        }
    }
}

/// One chunker entry, with its grammar resolved (or its reason for not being).
#[derive(Debug)]
pub struct LoadedChunker {
    /// Extensions this entry **claims**. Whether a claim wins is not a property
    /// of the entry: it depends on which other plugins it is resolved against,
    /// which is [`resolve`]'s question and differs between the installed set and
    /// a project's enabled one. An entry that claimed a built-in extension is
    /// voided at load and claims nothing at all.
    pub extensions: Vec<String>,
    pub strategy: LoadedStrategy,
}

#[derive(Debug)]
pub enum LoadedStrategy {
    Grammar {
        /// Boxed to keep the enum's variants a similar size; the walker
        /// vocabulary is the bulk of a grammar chunker.
        config: Box<GrammarChunker>,
        grammar: Result<Grammar, Unavailable>,
    },
    Windows(WindowsChunker),
}

/// One loaded plugin.
#[derive(Debug)]
pub struct Plugin {
    pub name: String,
    pub root: Utf8PathBuf,
    /// blake3 over the manifest bytes and every referenced asset. This is the
    /// plugin's entire version identity.
    pub fingerprint: String,
    pub chunkers: Vec<LoadedChunker>,
}

impl Plugin {
    /// Read, parse and resolve one plugin root.
    ///
    /// The single-root half of [`PluginRegistry::load`], public because
    /// `lore plugin add` validates an installation candidate by *actually
    /// loading it* — a manifest checked by any other reader would eventually
    /// disagree with the one the daemon runs.
    ///
    /// The `Err` is the same [`Diagnostic`] the registry would have reported,
    /// so the CLI and `lore status` say the same thing about the same file.
    pub fn load(root: &Utf8Path) -> Result<Self, Diagnostic> {
        let mut diagnostics = Vec::new();
        load_one(root, &mut diagnostics).ok_or_else(|| {
            diagnostics.pop().unwrap_or_else(|| Diagnostic::Manifest {
                root: root.to_owned(),
                detail: "the plugin could not be loaded".to_string(),
            })
        })
    }

    /// Every extension this plugin claims, in declaration order. What
    /// `lore status` and `lore plugin list` report.
    ///
    /// Claims, not holdings: an extension another installed plugin also claims
    /// is listed here and routes for whichever projects enable only one of
    /// them. Contested-ness belongs in the diagnostics beside this list, which
    /// name both claimants — a display that quietly dropped the extension
    /// instead would leave an author reading `lore plugin list` unable to tell
    /// "I never claimed it" from "somebody else claims it too".
    pub fn extensions(&self) -> Vec<&str> {
        self.chunkers
            .iter()
            .flat_map(|chunker| chunker.extensions.iter().map(String::as_str))
            .collect()
    }
}

/// Every plugin the daemon has loaded, and the extension → plugin map that
/// routing consults.
///
/// Plugins are held behind [`Arc`] so [`Self::enabled_only`] can hand out a
/// narrowed registry — one project's opt-in — without reloading a grammar or
/// re-hashing an asset. The narrowed registry is an ordinary `PluginRegistry`
/// on purpose: routing takes `Option<&PluginRegistry>` and knows nothing about
/// enablement, which keeps "which plugins exist" and "which this project wants"
/// from becoming two questions the chunker has to answer.
///
/// The extension map is [`resolve`]d against *this registry's* plugins, so a
/// narrowed registry is not a filtered view of the wide one's map — it is the
/// same resolution run over fewer claimants, which is the whole point.
#[derive(Debug, Default)]
pub struct PluginRegistry {
    plugins: Vec<Arc<Plugin>>,
    /// Extension → (plugin index, chunker index). Built-in extensions and
    /// extensions contested *within this registry* are absent by construction.
    by_extension: BTreeMap<String, (usize, usize)>,
}

/// Which plugin owns an extension, answerable **before the file is read** —
/// the same shape as [`crate::chunk::is_markdown`], and for the same reason:
/// the indexer folds the answer into the per-file content hash and must not
/// have to fetch content to do it.
#[derive(Debug, Clone, Copy)]
pub struct Claim<'a> {
    pub plugin: &'a str,
    pub fingerprint: &'a str,
    pub(crate) strategy: &'a LoadedStrategy,
}

impl Claim<'_> {
    /// `Some` when the claim cannot actually be honored, so the file will take
    /// the built-in fallback path. The daemon counts these for `status`.
    pub fn unavailable(&self) -> Option<&Unavailable> {
        match &self.strategy {
            LoadedStrategy::Grammar { grammar, .. } => grammar.as_ref().err(),
            LoadedStrategy::Windows(_) => None,
        }
    }

    /// The plugin's own file-size cap, if it declared a tighter one.
    pub fn max_file_bytes(&self) -> Option<usize> {
        match &self.strategy {
            LoadedStrategy::Windows(windows) => Some(windows.max_file_bytes),
            LoadedStrategy::Grammar { .. } => None,
        }
    }
}

impl PluginRegistry {
    /// A registry with no plugins. Behaves exactly like no registry at all.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.by_extension.is_empty()
    }

    /// Every loaded plugin, in the order their roots were visited (sorted, so
    /// it is the same on every machine). Shared rather than owned so that
    /// [`Self::enabled_only`] is a filter and not a reload; a caller reads
    /// straight through the [`Arc`].
    pub fn plugins(&self) -> &[Arc<Plugin>] {
        &self.plugins
    }

    /// The plugin loaded under this name, if any.
    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins
            .iter()
            .map(Arc::as_ref)
            .find(|plugin| plugin.name == name)
    }

    /// This registry narrowed to the plugins a project enabled — the
    /// intersection of *installed* and `[plugins] enable` (see
    /// [`crate::repo_config::enabled_plugins`]).
    ///
    /// Cheap: the plugins themselves are shared, and only the extension map is
    /// rebuilt. Cheap matters because it happens once per index pass per
    /// project, and the alternative — threading an enabled set through
    /// [`crate::chunk::chunk_file_with`] and every stamping call beside it —
    /// would put the same filter in two places that must never disagree.
    ///
    /// An enabled name nobody installed simply contributes nothing, which is
    /// what makes it a reportable gap rather than an error.
    ///
    /// Conflicts are re-[`resolve`]d over the narrowed set rather than
    /// inherited from the wide one. Two installed plugins fighting over an
    /// extension must not cost it to a project that enabled only one of them:
    /// resolving at install scope would let `lore plugin add` re-chunk a
    /// project that never asked for the new plugin, which is precisely the
    /// property "installing a plugin re-chunks nothing until a repository
    /// enables it" promises. Resolution stays deterministic because it is a
    /// pure function of this registry's plugin order and the enabled set — not
    /// of who asked or when.
    ///
    /// The narrowed resolution's diagnostics are dropped, and nothing is lost
    /// by it: a conflict *within* an enabled set is by definition a conflict
    /// within the installed set, so it was already reported at load, by name,
    /// with both claimants.
    pub fn enabled_only(&self, enabled: &BTreeSet<String>) -> Self {
        let plugins: Vec<Arc<Plugin>> = self
            .plugins
            .iter()
            .filter(|plugin| enabled.contains(&plugin.name))
            .map(Arc::clone)
            .collect();
        let (by_extension, _reported_at_load) = resolve(&plugins);
        Self {
            plugins,
            by_extension,
        }
    }

    /// The plugin owning `extension` (lowercase, bare), if any.
    pub fn claim_extension(&self, extension: &str) -> Option<Claim<'_>> {
        let (plugin, chunker) = *self.by_extension.get(extension)?;
        let plugin = &self.plugins[plugin];
        Some(Claim {
            plugin: &plugin.name,
            fingerprint: &plugin.fingerprint,
            strategy: &plugin.chunkers[chunker].strategy,
        })
    }

    /// The plugin owning `rel_path`, if any.
    pub fn claim(&self, rel_path: &Utf8Path) -> Option<Claim<'_>> {
        let extension = rel_path.extension()?.to_ascii_lowercase();
        self.claim_extension(&extension)
    }

    /// Loads every plugin root under `dir`.
    ///
    /// `dir` is a parameter rather than derived from the data directory: which
    /// path the daemon points at is the daemon's business, and tests point at a
    /// fixture tree.
    ///
    /// A missing `dir` is not a problem — it is what a daemon with no plugins
    /// looks like. Everything else that goes wrong comes back as a
    /// [`Diagnostic`] and costs only what it has to.
    pub fn load(dir: &Utf8Path) -> (Self, Vec<Diagnostic>) {
        let mut diagnostics = Vec::new();
        let mut loaded: Vec<Arc<Plugin>> = Vec::new();

        let mut roots: Vec<Utf8PathBuf> = Vec::new();
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                        continue; // A non-UTF-8 path cannot name a plugin we could report.
                    };
                    if path.is_dir() {
                        roots.push(path);
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::empty(), diagnostics);
            }
            Err(err) => {
                diagnostics.push(Diagnostic::Manifest {
                    root: dir.to_owned(),
                    detail: format!("the plugin directory is unreadable: {err}"),
                });
                return (Self::empty(), diagnostics);
            }
        }
        // Directory order is not a promise any filesystem makes, and conflict
        // reporting has to be the same on every machine.
        roots.sort();

        let mut names: BTreeSet<String> = BTreeSet::new();
        for root in roots {
            let Some(mut plugin) = load_one(&root, &mut diagnostics) else {
                continue;
            };
            if !names.insert(plugin.name.clone()) {
                diagnostics.push(Diagnostic::DuplicateName {
                    name: plugin.name,
                    root,
                });
                continue;
            }
            admit(&mut plugin, &mut diagnostics);
            loaded.push(Arc::new(plugin));
        }

        // Conflicts are settled *over* the loaded plugins rather than *into*
        // them: a claim is a property of the manifest, and which claim wins is
        // a property of the set it competes in. This is the machine-wide set,
        // so these are the machine-wide conflicts — the ones a human is told
        // about, whichever projects go on to enable which claimant.
        let (by_extension, conflicts) = resolve(&loaded);
        diagnostics.extend(conflicts);
        let registry = Self {
            plugins: loaded,
            by_extension,
        };
        (registry, diagnostics)
    }
}

/// Everything about one plugin that is true regardless of what else is
/// installed: the built-in extensions it may not have, and the grammars it
/// declared but cannot run.
fn admit(plugin: &mut Plugin, diagnostics: &mut Vec<Diagnostic>) {
    let name = plugin.name.clone();
    for chunker in &mut plugin.chunkers {
        // A built-in claim voids the whole entry, not just the offending
        // extension: a chunker that claims `md` alongside `uxml` has
        // misunderstood the contract, and half-honoring it would be worse
        // than honoring none of it.
        if let Some(builtin) = chunker
            .extensions
            .iter()
            .find(|ext| crate::chunk::is_builtin_extension(ext))
        {
            diagnostics.push(Diagnostic::BuiltinExtension {
                plugin: name.clone(),
                extension: builtin.clone(),
            });
            chunker.extensions.clear();
            continue;
        }

        if let LoadedStrategy::Grammar {
            config,
            grammar: Err(reason),
        } = &chunker.strategy
            && !chunker.extensions.is_empty()
        {
            diagnostics.push(Diagnostic::GrammarUnavailable {
                plugin: name.clone(),
                grammar: config.grammar.clone(),
                reason: reason.clone(),
            });
        }
    }
}

/// Settle every extension claim in `plugins` against every other, without
/// touching a plugin.
///
/// A contested extension goes to **nobody**, and stays contested for every
/// later claimant — a precedence rule would make the winner depend on the order
/// the roots happened to be visited in. Order still decides *which pair* is
/// reported first, which is why [`PluginRegistry::load`] sorts them.
///
/// Pure, so the same plugins in the same order always resolve the same way.
/// That is what lets one project's enabled set be resolved on its own without
/// any risk that two views of one machine disagree about anything except the
/// claimants they were given.
fn resolve(plugins: &[Arc<Plugin>]) -> (BTreeMap<String, (usize, usize)>, Vec<Diagnostic>) {
    let mut by_extension: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // Extensions two plugins fought over. Kept so a third claimant does not
    // quietly inherit a contested extension.
    let mut contested: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut diagnostics = Vec::new();

    for (index, plugin) in plugins.iter().enumerate() {
        for (chunker_index, chunker) in plugin.chunkers.iter().enumerate() {
            for extension in &chunker.extensions {
                if let Some(holders) = contested.get_mut(extension) {
                    holders.push(plugin.name.clone());
                    diagnostics.push(Diagnostic::ExtensionConflict {
                        extension: extension.clone(),
                        plugins: holders.clone(),
                    });
                    continue;
                }
                match by_extension.remove(extension) {
                    Some((held_by, _)) => {
                        let holders = vec![plugins[held_by].name.clone(), plugin.name.clone()];
                        diagnostics.push(Diagnostic::ExtensionConflict {
                            extension: extension.clone(),
                            plugins: holders.clone(),
                        });
                        contested.insert(extension.clone(), holders);
                    }
                    None => {
                        by_extension.insert(extension.clone(), (index, chunker_index));
                    }
                }
            }
        }
    }
    (by_extension, diagnostics)
}

/// Reads, parses and resolves one plugin root. `None` (with a diagnostic) when
/// the root is not a usable plugin at all.
fn load_one(root: &Utf8Path, diagnostics: &mut Vec<Diagnostic>) -> Option<Plugin> {
    let manifest_path = root.join(MANIFEST_FILE);
    let bytes = match std::fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            diagnostics.push(Diagnostic::Manifest {
                root: root.to_owned(),
                detail: format!("{MANIFEST_FILE} is unreadable: {err}"),
            });
            return None;
        }
    };
    let manifest = match manifest::parse(&manifest_path, &bytes) {
        Ok(manifest) => manifest,
        Err(detail) => {
            diagnostics.push(Diagnostic::Manifest {
                root: root.to_owned(),
                detail,
            });
            return None;
        }
    };

    // Every referenced asset is read exactly once: the bytes feed both the
    // fingerprint and the grammar loader, so the fingerprint always describes
    // the artifact that was actually loaded.
    let mut assets: BTreeMap<Utf8PathBuf, Result<Vec<u8>, String>> = BTreeMap::new();
    for chunker in &manifest.chunkers {
        if let manifest::Strategy::Grammar(config) = &chunker.strategy {
            assets.entry(config.grammar.clone()).or_insert_with(|| {
                std::fs::read(root.join(&config.grammar)).map_err(|err| err.to_string())
            });
        }
    }

    let fingerprint = fingerprint(&bytes, &assets);

    let chunkers = manifest
        .chunkers
        .into_iter()
        .map(|chunker| LoadedChunker {
            extensions: chunker.extensions,
            strategy: match chunker.strategy {
                manifest::Strategy::Windows(windows) => LoadedStrategy::Windows(windows),
                manifest::Strategy::Grammar(config) => {
                    let grammar = match &assets[&config.grammar] {
                        Ok(bytes) => grammar::load(&config.symbol, bytes),
                        Err(detail) => Err(Unavailable::AssetUnreadable {
                            path: config.grammar.clone(),
                            detail: detail.clone(),
                        }),
                    };
                    LoadedStrategy::Grammar { config, grammar }
                }
            },
        })
        .collect();

    Some(Plugin {
        name: manifest.name,
        root: root.to_owned(),
        fingerprint,
        chunkers,
    })
}

/// blake3 over the manifest and every referenced asset, length-prefixed and
/// domain-separated so no rearrangement of the same bytes can collide.
///
/// A *missing* asset hashes distinctly from a present one, so installing the
/// artifact a manifest referenced all along changes the fingerprint and
/// re-chunks the files — which is exactly what has to happen, because the
/// chunks were produced by the fallback path.
fn fingerprint(manifest: &[u8], assets: &BTreeMap<Utf8PathBuf, Result<Vec<u8>, String>>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lore-plugin-fingerprint-v1\0");
    hasher.update(&(manifest.len() as u64).to_le_bytes());
    hasher.update(manifest);
    // A BTreeMap iterates in path order, on every platform.
    for (path, bytes) in assets {
        hasher.update(path.as_str().as_bytes());
        hasher.update(&[0]);
        match bytes {
            Ok(bytes) => {
                hasher.update(&[1]);
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(bytes);
            }
            Err(_) => {
                hasher.update(&[0]);
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}
