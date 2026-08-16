//! Generating a project's `.loreignore`.
//!
//! The exclusion policy is a file in the project, not a list compiled into
//! the daemon: `target/` is right for Rust and wrong for everyone else, and a
//! rule the user cannot see is a rule the user cannot fix. So Lore writes the
//! ecosystem defaults once, as a commented file, and never touches it again —
//! every later edit is the user's.
//!
//! Detection walks the root and a few levels below it because markers are
//! frequently *not* at the root: a folder holding a Unity project in one
//! subdirectory and Python tooling in another is a real layout, and the
//! patterns are therefore emitted unanchored (`Library/`, not `/Library/`) so
//! they still match where the marker actually was.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;

use camino::{Utf8Path, Utf8PathBuf};

use super::walk::LORE_IGNORE_FILE;

/// How far below the root markers are looked for. Deep enough for the
/// "container folder holding two real projects" case, shallow enough that
/// detection stays a bounded number of `read_dir` calls on a large tree.
const DETECT_DEPTH: usize = 3;

/// Directories detection never descends into: everything the templates below
/// exclude, plus the tool caches the old hardcoded list named. A `venv` or a
/// `node_modules` full of vendored `setup.py` files would otherwise make
/// detection both slow and wrong.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "Library",
    "Temp",
    "Logs",
    "obj",
    "bin",
    "UserSettings",
    "MemoryCaptures",
    "Build",
    "Builds",
    "__pycache__",
    "venv",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecosystem {
    Rust,
    Node,
    Unity,
    DotNet,
    Python,
}

impl Ecosystem {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Node => "Node",
            Self::Unity => "Unity",
            Self::DotNet => ".NET",
            Self::Python => "Python",
        }
    }

    /// What was found on disk. Written into the generated file so the user can
    /// tell why a group is there — and delete it if the marker was a red
    /// herring.
    const fn marker(self) -> &'static str {
        match self {
            Self::Rust => "Cargo.toml",
            Self::Node => "package.json",
            Self::Unity => "ProjectSettings/ and Assets/",
            Self::DotNet => "*.sln or *.csproj",
            Self::Python => "pyproject.toml, setup.py or requirements.txt",
        }
    }

    const fn patterns(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["target/"],
            Self::Node => &["node_modules/", "dist/"],
            Self::Unity => &[
                "Library/",
                "Temp/",
                "Logs/",
                "obj/",
                "UserSettings/",
                "MemoryCaptures/",
                "Build/",
                "Builds/",
                "*.meta",
            ],
            Self::DotNet => &["bin/", "obj/"],
            Self::Python => &["__pycache__/", "venv/", ".venv/", "*.pyc"],
        }
    }
}

const HEADER: &str = "\
# .loreignore — what Lore does not index in this project.
#
# gitignore syntax: one pattern per line, a trailing `/` matches directories
# only, and `!pattern` re-includes something an earlier rule (or .gitignore)
# excluded. Patterns are unanchored, so `Library/` matches at any depth.
#
# Lore generated this file once, from the markers it found below, and will
# never rewrite or extend it. It is yours now — edit it freely.
#
# An empty file means \"index everything the VCS rules allow\": Lore falls back
# to its own built-in exclusions only while this file does not exist.
";

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("{0} already exists")]
    Exists(Utf8PathBuf),
    #[error("writing {0}")]
    Io(Utf8PathBuf, #[source] std::io::Error),
}

pub fn path(root: &Utf8Path) -> Utf8PathBuf {
    root.join(LORE_IGNORE_FILE)
}

/// Write a fresh `.loreignore` for `root`, returning what it was generated
/// from.
///
/// Refuses an existing file rather than merging into it: a generator that
/// appends would fight every edit the user makes, and one that overwrites
/// would lose them.
pub fn write_new(root: &Utf8Path) -> Result<Vec<Ecosystem>, WriteError> {
    let path = path(root);
    // Checked before detection so the common case (the file is already there,
    // on every full scan) costs one stat rather than a directory walk. The
    // `create_new` below is still what actually guarantees no overwrite.
    if path.exists() {
        return Err(WriteError::Exists(path));
    }

    let ecosystems = detect(root);
    let body = render(&ecosystems);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => match file.write_all(body.as_bytes()) {
            Ok(()) => Ok(ecosystems),
            Err(err) => Err(WriteError::Io(path, err)),
        },
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(WriteError::Exists(path))
        }
        Err(err) => Err(WriteError::Io(path, err)),
    }
}

/// Create the file if it is missing. Returns whether it created one.
///
/// The daemon's entry point, so it logs rather than returning an error: a
/// project on a read-only share must still be scanned, and the walker's
/// built-in fallback is what keeps that scan from indexing a `Library/`.
pub fn ensure(root: &Utf8Path) -> bool {
    match write_new(root) {
        Ok(ecosystems) => {
            tracing::info!(
                root = %root,
                ecosystems = %labels(&ecosystems),
                "generated {LORE_IGNORE_FILE}"
            );
            true
        }
        Err(WriteError::Exists(_)) => false,
        Err(err) => {
            tracing::warn!(
                root = %root,
                error = %format!("{err}"),
                "could not generate {LORE_IGNORE_FILE}; falling back to the built-in exclusions"
            );
            false
        }
    }
}

/// Comma-separated labels, or `none` — for logs and the CLI.
pub fn labels(ecosystems: &[Ecosystem]) -> String {
    if ecosystems.is_empty() {
        return "none".to_string();
    }
    ecosystems
        .iter()
        .map(|eco| eco.label())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn render(ecosystems: &[Ecosystem]) -> String {
    let mut out = String::from(HEADER);
    for eco in ecosystems {
        let _ = write!(out, "\n# {} ({})\n", eco.label(), eco.marker());
        for pattern in eco.patterns() {
            out.push_str(pattern);
            out.push('\n');
        }
    }
    out
}

/// Every ecosystem with a marker at or under `root`, deduplicated and in a
/// fixed order so the generated file does not depend on directory iteration
/// order.
pub fn detect(root: &Utf8Path) -> Vec<Ecosystem> {
    let mut found = BTreeSet::new();
    scan(root, 0, &mut found);
    found.into_iter().collect()
}

fn scan(dir: &Utf8Path, depth: usize, found: &mut BTreeSet<Ecosystem>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Unreadable directory: nothing to detect here, and a permissions
        // failure is not worth failing generation over.
        return;
    };

    let mut files: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // Dot-directories are skipped for the same reason the walker skips
        // them, and a dot-file is never one of these markers.
        if name.starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => dirs.push(name),
            Ok(kind) if kind.is_file() => files.push(name),
            _ => {}
        }
    }

    let has_file = |wanted: &str| files.iter().any(|name| name.eq_ignore_ascii_case(wanted));
    let has_dir = |wanted: &str| dirs.iter().any(|name| name.eq_ignore_ascii_case(wanted));
    let has_extension = |wanted: &str| {
        files.iter().any(|name| {
            Utf8Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(wanted))
        })
    };

    if has_file("Cargo.toml") {
        found.insert(Ecosystem::Rust);
    }
    if has_file("package.json") {
        found.insert(Ecosystem::Node);
    }
    // Both, because either alone is ambiguous: plenty of non-Unity projects
    // have an `Assets` directory.
    if has_dir("ProjectSettings") && has_dir("Assets") {
        found.insert(Ecosystem::Unity);
    }
    if has_extension("sln") || has_extension("csproj") {
        found.insert(Ecosystem::DotNet);
    }
    if has_file("pyproject.toml") || has_file("setup.py") || has_file("requirements.txt") {
        found.insert(Ecosystem::Python);
    }

    if depth >= DETECT_DEPTH {
        return;
    }
    for name in dirs {
        if SKIP_DIRS
            .iter()
            .any(|skip| skip.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        scan(&dir.join(name), depth + 1, found);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(spec: &[&str]) -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        for entry in spec {
            let abs = root.join(entry);
            if entry.ends_with('/') {
                std::fs::create_dir_all(&abs).unwrap();
            } else {
                std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
                std::fs::write(&abs, "x").unwrap();
            }
        }
        (dir, root)
    }

    fn detected(spec: &[&str]) -> Vec<Ecosystem> {
        let (_dir, root) = fixture(spec);
        detect(&root)
    }

    #[test]
    fn each_ecosystem_is_recognised_from_its_marker() {
        assert_eq!(detected(&["Cargo.toml"]), [Ecosystem::Rust]);
        assert_eq!(detected(&["package.json"]), [Ecosystem::Node]);
        assert_eq!(
            detected(&["ProjectSettings/", "Assets/"]),
            [Ecosystem::Unity]
        );
        assert_eq!(detected(&["Game.sln"]), [Ecosystem::DotNet]);
        assert_eq!(detected(&["src/Tool.csproj"]), [Ecosystem::DotNet]);
        assert_eq!(detected(&["pyproject.toml"]), [Ecosystem::Python]);
        assert_eq!(detected(&["setup.py"]), [Ecosystem::Python]);
        assert_eq!(detected(&["requirements.txt"]), [Ecosystem::Python]);
    }

    /// Half a Unity project is not a Unity project — `Assets/` alone is far
    /// too common to spend nine patterns on.
    #[test]
    fn an_assets_directory_alone_is_not_unity() {
        assert!(detected(&["Assets/art.png"]).is_empty());
        assert!(detected(&["ProjectSettings/x.asset"]).is_empty());
    }

    /// The layout the unanchored patterns exist for: a container folder whose
    /// Unity project is one level down, next to unrelated Python tooling.
    #[test]
    fn a_unity_project_below_the_root_is_found_and_emitted_unanchored() {
        let (_dir, root) = fixture(&[
            "game/ProjectSettings/ProjectVersion.txt",
            "game/Assets/Scripts/Board.cs",
            "tools/requirements.txt",
        ]);
        let found = detect(&root);
        assert_eq!(found, [Ecosystem::Unity, Ecosystem::Python]);

        let rendered = render(&found);
        assert!(rendered.contains("\nLibrary/\n"), "{rendered}");
        assert!(!rendered.contains("/Library/"), "{rendered}");
        assert!(!rendered.contains("game/Library/"), "{rendered}");
    }

    #[test]
    fn several_ecosystems_are_grouped_under_their_own_headings() {
        let (_dir, root) = fixture(&["Cargo.toml", "web/package.json", "api/pyproject.toml"]);
        let found = detect(&root);
        assert_eq!(found, [Ecosystem::Rust, Ecosystem::Node, Ecosystem::Python]);

        let rendered = render(&found);
        assert!(rendered.starts_with("# .loreignore"), "{rendered}");
        assert!(
            rendered.contains("\n# Rust (Cargo.toml)\ntarget/\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\n# Node (package.json)\nnode_modules/\ndist/\n"),
            "{rendered}"
        );
        assert!(rendered.contains("__pycache__/"), "{rendered}");
        // The three promises the header has to make.
        assert!(rendered.contains("gitignore syntax"), "{rendered}");
        assert!(rendered.contains("never rewrite"), "{rendered}");
        assert!(
            rendered.contains("index everything the VCS rules allow"),
            "{rendered}"
        );
    }

    #[test]
    fn nothing_detected_emits_the_header_and_nothing_else() {
        let (_dir, root) = fixture(&["notes.md", "src/main.txt"]);
        assert!(detect(&root).is_empty());
        assert_eq!(render(&[]), HEADER);
        assert!(HEADER.lines().all(|line| line.starts_with('#')));
    }

    /// A vendored marker must not enrol the whole project — and, more to the
    /// point, detection must not walk a `venv` to find out.
    #[test]
    fn markers_inside_excluded_directories_are_not_detected() {
        assert!(detected(&["node_modules/pkg/package.json"]).is_empty());
        assert!(detected(&["venv/lib/setup.py"]).is_empty());
        assert!(detected(&["target/vendor/Cargo.toml"]).is_empty());
        assert!(detected(&[".hidden/Cargo.toml"]).is_empty());
    }

    #[test]
    fn detection_stops_at_the_depth_bound() {
        assert_eq!(detected(&["a/b/c/Cargo.toml"]), [Ecosystem::Rust]);
        assert!(detected(&["a/b/c/d/Cargo.toml"]).is_empty());
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let (_dir, root) = fixture(&["Cargo.toml"]);
        let generated = path(&root);
        std::fs::write(&generated, "").unwrap();

        let err = write_new(&root).unwrap_err();
        assert!(matches!(err, WriteError::Exists(_)), "{err}");
        assert_eq!(std::fs::read_to_string(&generated).unwrap(), "");
        assert!(!ensure(&root), "ensure must report that it created nothing");
        assert_eq!(std::fs::read_to_string(&generated).unwrap(), "");
    }

    #[test]
    fn ensure_creates_the_file_once() {
        let (_dir, root) = fixture(&["Cargo.toml"]);
        assert!(ensure(&root));
        let body = std::fs::read_to_string(path(&root)).unwrap();
        assert!(body.contains("target/"), "{body}");
        assert!(!ensure(&root));
    }

    #[test]
    fn labels_read_as_prose_in_a_log_line() {
        assert_eq!(labels(&[]), "none");
        assert_eq!(
            labels(&[Ecosystem::Unity, Ecosystem::DotNet]),
            "Unity, .NET"
        );
    }
}
