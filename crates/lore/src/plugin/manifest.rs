//! `lore-plugin.toml` — the entire authored surface of a chunker plugin.
//!
//! A plugin is data, not code: this file plus its assets is everything a
//! plugin *is*. Parsing is therefore strict on purpose — unknown keys are
//! errors, not ignored typos — and every error message names the plugin and
//! the field, because the author's only feedback channel is `lore status` and
//! the daemon log.
//!
//! # Field choices
//!
//! The contract sketch left field names to the implementation. What is fixed
//! here:
//!
//! * `[plugin] name` is the plugin's identity, in diagnostics, in `status` and
//!   (per the contract) advertised across a push handshake. It is validated as
//!   a lowercase slug rather than reusing project *display* names, which
//!   D-0016 deliberately keeps loose (spaces, case, punctuation) because they
//!   label a local checkout rather than travelling between machines.
//! * `strategy` is the serde tag, so `grammar`-only keys on a `windows`
//!   chunker are rejected by the parser rather than silently ignored.
//! * `extensions` are bare and lowercase (`uxml`, never `.uxml`, never
//!   `UXML`), matching how [`crate::chunk`] derives an extension from a path.
//! * `grammar` names an asset **relative to the plugin root**; absolute paths
//!   and `..` are refused so a manifest cannot reach outside its own directory.
//! * `symbol` defaults to `language_tag` with `-` folded to `_`. A `.wasm`
//!   grammar is loaded by its exported `tree_sitter_<symbol>` function, which
//!   is usually the language tag but not always (`c_sharp` vs `csharp`), so it
//!   is overridable and defaulted rather than derived from the file name.
//! * Windows caps are **tighten-only** against the core constants, resolving
//!   the contract's open question in the direction it was leaning: a plugin may
//!   make Lore read less of a file, never more.

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;

use crate::chunk::{MAX_FILE_BYTES, TEXT_WINDOW_LINES, WINDOW_OVERLAP_LINES, WindowCaps};

/// Manifest file name at a plugin root.
pub const MANIFEST_FILE: &str = "lore-plugin.toml";

/// Longest accepted plugin name / extension. Generous for anything real; a
/// bound at all is what keeps a hostile manifest from filling a log line.
const MAX_NAME_BYTES: usize = 64;

/// A parsed, validated manifest. Construction is the validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub chunkers: Vec<Chunker>,
}

/// One `[[chunker]]` entry: the extensions it claims and how it chunks them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunker {
    pub extensions: Vec<String>,
    pub strategy: Strategy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    /// Boxed only to keep the two variants a similar size; a grammar
    /// chunker's role vocabulary is much the larger of the two.
    Grammar(Box<GrammarChunker>),
    Windows(WindowsChunker),
}

/// A tree-sitter grammar compiled to `.wasm`, plus the role vocabulary that
/// drives the existing [`crate::chunk`] walker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarChunker {
    /// Asset path relative to the plugin root.
    pub grammar: Utf8PathBuf,
    /// Exported symbol suffix: the artifact must export `tree_sitter_<symbol>`.
    pub symbol: String,
    /// Value for `Chunk::language`, and the first word of the embedding header.
    pub language_tag: String,
    pub path_only: Vec<String>,
    pub containers: Vec<String>,
    pub symbols: Vec<String>,
    pub wrappers: Vec<String>,
    pub bodies: Vec<String>,
    pub attachments: Vec<String>,
    pub trailing_scope: Vec<String>,
    /// Tree-sitter field consulted first for a declaration's name.
    pub name_field: String,
    /// Fallback name extraction: the first node of one of these kinds within
    /// two levels of the declaration. See [`crate::chunk`]'s note on why this
    /// exists and why it is not a query engine.
    pub name_kinds: Vec<String>,
}

/// The existing line windower, with the plugin's (tighter) caps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsChunker {
    /// Files above this are skipped. Never above core's [`MAX_FILE_BYTES`].
    pub max_file_bytes: usize,
    pub caps: WindowCaps,
    /// Optional `Chunk::language` tag. Windows carry no symbol, so this is the
    /// only thing that can tell an embedding header `yaml` from `text`.
    pub language_tag: Option<String>,
}

/// Parses and validates `bytes` as the manifest at `path`.
///
/// The `Err` string is the whole diagnostic: it already names the file, the
/// plugin (once known) and the field.
pub fn parse(path: &Utf8Path, bytes: &[u8]) -> Result<Manifest, String> {
    let text = std::str::from_utf8(bytes).map_err(|err| format!("{path}: not UTF-8: {err}"))?;
    let file: FileToml = toml::from_str(text).map_err(|err| format!("{path}: {err}"))?;

    let name = file.plugin.name.trim().to_string();
    validate_name(&name).map_err(|why| format!("{path}: [plugin] name {why}"))?;

    let mut chunkers = Vec::with_capacity(file.chunkers.len());
    let mut claimed: Vec<String> = Vec::new();
    for (index, chunker) in file.chunkers.into_iter().enumerate() {
        let at = |field: &str| format!("plugin \"{name}\" ({path}): chunker[{index}].{field}");
        let (extensions, strategy) = match chunker {
            ChunkerToml::Grammar(grammar) => {
                (grammar.extensions.clone(), grammar_strategy(*grammar, &at)?)
            }
            ChunkerToml::Windows(windows) => {
                (windows.extensions.clone(), windows_strategy(windows, &at)?)
            }
        };
        if extensions.is_empty() {
            return Err(format!("{} claims no extensions", at("extensions")));
        }
        let mut normalized = Vec::with_capacity(extensions.len());
        for extension in extensions {
            validate_extension(&extension)
                .map_err(|why| format!("{}: \"{extension}\" {why}", at("extensions")))?;
            if claimed.contains(&extension) {
                return Err(format!(
                    "{}: \"{extension}\" is claimed twice by this plugin",
                    at("extensions")
                ));
            }
            claimed.push(extension.clone());
            normalized.push(extension);
        }
        chunkers.push(Chunker {
            extensions: normalized,
            strategy,
        });
    }

    Ok(Manifest { name, chunkers })
}

fn grammar_strategy(toml: GrammarToml, at: &dyn Fn(&str) -> String) -> Result<Strategy, String> {
    let grammar = Utf8PathBuf::from(toml.grammar.replace('\\', "/"));
    validate_asset_path(&grammar)
        .map_err(|why| format!("{}: \"{grammar}\" {why}", at("grammar")))?;
    let language_tag = toml.language_tag.trim().to_string();
    if language_tag.is_empty() {
        return Err(format!("{} is empty", at("language_tag")));
    }
    let symbol = match toml.symbol {
        Some(symbol) => symbol.trim().to_string(),
        None => language_tag.replace('-', "_"),
    };
    if !symbol
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || symbol.is_empty()
    {
        return Err(format!(
            "{}: \"{symbol}\" must be the suffix of an exported `tree_sitter_<symbol>` \
             function (ASCII letters, digits and underscores)",
            at("symbol")
        ));
    }
    Ok(Strategy::Grammar(Box::new(GrammarChunker {
        grammar,
        symbol,
        language_tag,
        path_only: toml.path_only,
        containers: toml.containers,
        symbols: toml.symbols,
        wrappers: toml.wrappers,
        bodies: toml.bodies,
        attachments: toml.attachments,
        trailing_scope: toml.trailing_scope,
        name_field: toml.name_field.unwrap_or_else(|| "name".to_string()),
        name_kinds: toml.name_kinds,
    })))
}

fn windows_strategy(toml: WindowsToml, at: &dyn Fn(&str) -> String) -> Result<Strategy, String> {
    let default = WindowCaps::default();
    let max_file_bytes = toml.max_file_bytes.unwrap_or(MAX_FILE_BYTES);
    tighten_only("max_file_bytes", max_file_bytes, MAX_FILE_BYTES, at)?;
    let window_lines = toml.window_lines.unwrap_or(default.window_lines);
    tighten_only(
        "window_lines",
        window_lines as usize,
        TEXT_WINDOW_LINES as usize,
        at,
    )?;
    let overlap_lines = toml.overlap_lines.unwrap_or(default.overlap_lines);
    // Zero overlap is legal (and cheaper); the ceiling is core's, and an
    // overlap that reaches the window height would stop the windower making
    // forward progress by lines alone.
    if overlap_lines > WINDOW_OVERLAP_LINES {
        return Err(format!(
            "{} is {overlap_lines}, above core's {WINDOW_OVERLAP_LINES}; \
             a plugin may only tighten a cap",
            at("overlap_lines")
        ));
    }
    if overlap_lines >= window_lines {
        return Err(format!(
            "{} is {overlap_lines}, which is not below window_lines ({window_lines})",
            at("overlap_lines")
        ));
    }
    let language_tag = match toml.language_tag {
        Some(tag) if tag.trim().is_empty() => {
            return Err(format!("{} is empty", at("language_tag")));
        }
        Some(tag) => Some(tag.trim().to_string()),
        None => None,
    };
    Ok(Strategy::Windows(WindowsChunker {
        max_file_bytes,
        caps: WindowCaps {
            window_lines,
            overlap_lines,
            ..default
        },
        language_tag,
    }))
}

fn tighten_only(
    field: &str,
    value: usize,
    ceiling: usize,
    at: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    if value == 0 {
        return Err(format!("{} is zero", at(field)));
    }
    if value > ceiling {
        return Err(format!(
            "{} is {value}, above core's {ceiling}; a plugin may only tighten a cap",
            at(field)
        ));
    }
    Ok(())
}

/// Plugin names are lowercase slugs: `[a-z0-9]`, then `[a-z0-9_-]`.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("is empty".to_string());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(format!("is longer than {MAX_NAME_BYTES} bytes"));
    }
    let first_ok = name.as_bytes()[0].is_ascii_lowercase() || name.as_bytes()[0].is_ascii_digit();
    let rest_ok = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !first_ok || !rest_ok {
        return Err(format!(
            "\"{name}\" must be a lowercase slug (a-z, 0-9, `-`, `_`) starting with a \
             letter or digit"
        ));
    }
    Ok(())
}

/// Extensions are bare and lowercase, as [`camino::Utf8Path::extension`]
/// reports them once the chunker has lowercased the result.
fn validate_extension(extension: &str) -> Result<(), String> {
    if extension.is_empty() {
        return Err("is empty".to_string());
    }
    if extension.len() > MAX_NAME_BYTES {
        return Err(format!("is longer than {MAX_NAME_BYTES} bytes"));
    }
    if extension.starts_with('.') {
        return Err("must not start with a dot".to_string());
    }
    if !extension
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(
            "must be lowercase ASCII (a-z, 0-9, `-`, `_`); extensions are compared \
             case-folded, so an uppercase spelling would never match"
                .to_string(),
        );
    }
    Ok(())
}

/// Asset paths stay inside the plugin root: relative, no `..`, no root, no
/// drive prefix.
fn validate_asset_path(path: &Utf8Path) -> Result<(), String> {
    use camino::Utf8Component;
    if path.as_str().is_empty() {
        return Err("is empty".to_string());
    }
    if path.is_absolute() {
        return Err("must be relative to the plugin root".to_string());
    }
    for component in path.components() {
        match component {
            Utf8Component::Normal(_) | Utf8Component::CurDir => {}
            _ => {
                return Err(
                    "must not leave the plugin root (no `..`, no root, no drive)".to_string(),
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Serde shapes. Kept separate from the validated types above so the validated
// ones cannot be constructed without going through `parse`.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileToml {
    plugin: PluginToml,
    #[serde(default, rename = "chunker")]
    chunkers: Vec<ChunkerToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginToml {
    name: String,
}

#[derive(Deserialize)]
#[serde(tag = "strategy", rename_all = "lowercase")]
enum ChunkerToml {
    Grammar(Box<GrammarToml>),
    Windows(WindowsToml),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrammarToml {
    extensions: Vec<String>,
    grammar: String,
    language_tag: String,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    path_only: Vec<String>,
    #[serde(default)]
    containers: Vec<String>,
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default)]
    wrappers: Vec<String>,
    #[serde(default)]
    bodies: Vec<String>,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default)]
    trailing_scope: Vec<String>,
    #[serde(default)]
    name_field: Option<String>,
    #[serde(default)]
    name_kinds: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsToml {
    extensions: Vec<String>,
    #[serde(default)]
    max_file_bytes: Option<usize>,
    #[serde(default)]
    window_lines: Option<u32>,
    #[serde(default)]
    overlap_lines: Option<u32>,
    #[serde(default)]
    language_tag: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = "plugins/toy/lore-plugin.toml";

    fn parse_str(src: &str) -> Result<Manifest, String> {
        parse(Utf8Path::new(PATH), src.as_bytes())
    }

    fn err(src: &str) -> String {
        parse_str(src).expect_err("manifest should have been rejected")
    }

    const GRAMMAR: &str = r#"
[plugin]
name = "toy"

[[chunker]]
extensions = ["uxml"]
strategy = "grammar"
grammar = "xml.wasm"
language_tag = "xml"
containers = ["element"]
bodies = ["content"]
attachments = ["Comment"]
name_kinds = ["Name"]
"#;

    #[test]
    fn a_grammar_chunker_round_trips_with_defaulted_optionals() {
        let manifest = parse_str(GRAMMAR).unwrap();
        assert_eq!(manifest.name, "toy");
        let Strategy::Grammar(grammar) = &manifest.chunkers[0].strategy else {
            panic!("expected a grammar strategy");
        };
        assert_eq!(manifest.chunkers[0].extensions, ["uxml"]);
        assert_eq!(grammar.grammar, "xml.wasm");
        // `symbol` defaults to the language tag: `tree_sitter_xml`.
        assert_eq!(grammar.symbol, "xml");
        assert_eq!(grammar.name_field, "name");
        assert_eq!(grammar.name_kinds, ["Name"]);
        assert!(grammar.symbols.is_empty());
    }

    #[test]
    fn a_windows_chunker_defaults_to_core_caps_and_accepts_tighter_ones() {
        let manifest = parse_str(
            r#"
[plugin]
name = "toy"

[[chunker]]
extensions = ["unity", "prefab"]
strategy = "windows"
"#,
        )
        .unwrap();
        let Strategy::Windows(windows) = &manifest.chunkers[0].strategy else {
            panic!("expected a windows strategy");
        };
        assert_eq!(windows.max_file_bytes, MAX_FILE_BYTES);
        assert_eq!(windows.caps, WindowCaps::default());
        assert_eq!(windows.language_tag, None);

        let manifest = parse_str(
            r#"
[plugin]
name = "toy"

[[chunker]]
extensions = ["unity"]
strategy = "windows"
max_file_bytes = 262144
window_lines = 40
overlap_lines = 4
language_tag = "yaml"
"#,
        )
        .unwrap();
        let Strategy::Windows(windows) = &manifest.chunkers[0].strategy else {
            panic!("expected a windows strategy");
        };
        assert_eq!(windows.max_file_bytes, 262_144);
        assert_eq!(windows.caps.window_lines, 40);
        assert_eq!(windows.caps.overlap_lines, 4);
        assert_eq!(windows.language_tag.as_deref(), Some("yaml"));
    }

    #[test]
    fn window_caps_may_only_tighten() {
        for (field, value) in [
            ("max_file_bytes", (MAX_FILE_BYTES + 1).to_string()),
            ("window_lines", (TEXT_WINDOW_LINES + 1).to_string()),
            ("overlap_lines", (WINDOW_OVERLAP_LINES + 1).to_string()),
        ] {
            let message = err(&format!(
                "[plugin]\nname = \"toy\"\n\n[[chunker]]\nextensions = [\"unity\"]\n\
                 strategy = \"windows\"\n{field} = {value}\n"
            ));
            assert!(message.contains("only tighten"), "{field}: {message}");
            assert!(message.contains(field), "{field}: {message}");
            assert!(message.contains("toy"), "{field}: {message}");
        }
        // Zero is not "tighter", it is a chunker that can never chunk.
        assert!(
            err(
                "[plugin]\nname = \"toy\"\n\n[[chunker]]\nextensions = [\"unity\"]\n\
             strategy = \"windows\"\nmax_file_bytes = 0\n"
            )
            .contains("zero")
        );
        // An overlap that reaches the window height stalls the windower.
        assert!(
            err(
                "[plugin]\nname = \"toy\"\n\n[[chunker]]\nextensions = [\"unity\"]\n\
             strategy = \"windows\"\nwindow_lines = 4\noverlap_lines = 4\n"
            )
            .contains("not below window_lines")
        );
    }

    #[test]
    fn unknown_and_misplaced_fields_are_errors_not_silence() {
        // A typo in a role name would otherwise be an empty role list, which
        // presents as "the grammar mapping does nothing" much later.
        assert!(err(&GRAMMAR.replace("containers", "contaners")).contains("contaners"));
        // Grammar keys on a windows chunker: the strategy tag decides which
        // fields exist at all.
        assert!(
            err(
                "[plugin]\nname = \"toy\"\n\n[[chunker]]\nextensions = [\"unity\"]\n\
             strategy = \"windows\"\ngrammar = \"xml.wasm\"\n"
            )
            .contains("grammar")
        );
        assert!(err("[plugin]\nname = \"toy\"\nversion = \"1\"\n").contains("version"));
        assert!(err("[plugins]\nname = \"toy\"\n").contains("plugins"));
    }

    #[test]
    fn extensions_are_bare_lowercase_and_claimed_once_per_plugin() {
        for bad in [".uxml", "UXML", "u xml", ""] {
            let message = err(&GRAMMAR.replace("\"uxml\"", &format!("\"{bad}\"")));
            assert!(message.contains("extensions"), "{bad}: {message}");
        }
        let twice =
            format!("{GRAMMAR}\n[[chunker]]\nextensions = [\"uxml\"]\nstrategy = \"windows\"\n");
        assert!(err(&twice).contains("claimed twice"));
        // Also within one entry.
        assert!(
            err(&GRAMMAR.replace("[\"uxml\"]", "[\"uxml\", \"uxml\"]")).contains("claimed twice")
        );
        assert!(err(&GRAMMAR.replace("[\"uxml\"]", "[]")).contains("claims no extensions"));
    }

    #[test]
    fn plugin_names_are_slugs() {
        for bad in ["", "Toy", "my plugin", "-toy", "toy!"] {
            let message = err(&GRAMMAR.replace("name = \"toy\"", &format!("name = \"{bad}\"")));
            assert!(message.contains("[plugin] name"), "{bad}: {message}");
        }
        assert!(parse_str(&GRAMMAR.replace("name = \"toy\"", "name = \"unity-2022_3\"")).is_ok());
    }

    #[test]
    fn asset_paths_stay_inside_the_plugin_root() {
        for bad in ["../xml.wasm", "/etc/xml.wasm", "C:/xml.wasm", ""] {
            let message = err(&GRAMMAR.replace("\"xml.wasm\"", &format!("\"{bad}\"")));
            assert!(message.contains("grammar"), "{bad}: {message}");
        }
        // Nested asset directories are fine.
        assert!(parse_str(&GRAMMAR.replace("\"xml.wasm\"", "\"grammars/xml.wasm\"")).is_ok());
    }

    #[test]
    fn the_grammar_symbol_defaults_to_the_tag_but_can_be_overridden() {
        let manifest = parse_str(
            &GRAMMAR
                .replace("language_tag = \"xml\"", "language_tag = \"c-sharp\"")
                .to_string(),
        )
        .unwrap();
        let Strategy::Grammar(grammar) = &manifest.chunkers[0].strategy else {
            panic!()
        };
        assert_eq!(grammar.symbol, "c_sharp");

        let manifest = parse_str(&format!("{GRAMMAR}symbol = \"c_sharp\"\n")).unwrap();
        let Strategy::Grammar(grammar) = &manifest.chunkers[0].strategy else {
            panic!()
        };
        assert_eq!(grammar.symbol, "c_sharp");
        assert_eq!(grammar.language_tag, "xml");
    }
}
