//! What counts as an indexable file.
//!
//! One function decides this for the whole daemon, and both the full scan and
//! the incremental (watcher-driven) path go through it. That is the point:
//! if "is this file indexed?" had two implementations, a watcher event could
//! index something a rescan would then delete, forever.
//!
//! Rules, in order:
//! 1. Never anything inside the daemon's own data directory (its SQLite WAL
//!    is a busy file living outside every project root, but a project root
//!    could still be an ancestor of it).
//! 2. Never a directory named in [`HARD_EXCLUDES`] — build output and tool
//!    caches that are frequently enormous, frequently churning, and never
//!    worth retrieving.
//! 3. [`LORE_IGNORE_FILE`] (`.loreignore`) — gitignore syntax, nested like
//!    `.gitignore`, and honored **regardless of VCS**. This is the
//!    user-visible knob: a Unity VCS or Perforce workspace has no
//!    `.gitignore`, so without it the only filtering such a project gets is
//!    rule 2, and telemetry dumps or serialized-asset YAML index as if they
//!    were code.
//! 4. Otherwise ripgrep's rules via the `ignore` crate: `.gitignore`
//!    (including nested and parent files), `.git/info/exclude`, hidden files
//!    skipped.

use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;

use super::paths;

/// Directory names never descended into, at any depth, regardless of what
/// `.gitignore` says. Unity (`Library`, `Temp`, `obj`) is the flagship case
/// (D-0003): those trees are machine-generated, gigantic, and rewritten on
/// every editor launch.
/// Per-directory ignore file, gitignore syntax. Named for lore rather than
/// reusing `.ignore` (ripgrep's convention) so that a rule meant for search
/// tools and a rule meant for the index can differ.
pub const LORE_IGNORE_FILE: &str = ".loreignore";

pub const HARD_EXCLUDES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "Library",
    "Temp",
    "obj",
    "bin",
    ".obsidian",
    ".vs",
    ".idea",
];

pub fn is_hard_excluded(name: &str) -> bool {
    // Case-insensitive: `Library` on a case-insensitive Windows volume is the
    // same directory as `library`, and Unity is inconsistent about casing.
    HARD_EXCLUDES
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(name))
}

/// Cheap, stat-free rejection of a project-relative path: hard-excluded or
/// hidden (a component starting with `.`, matching the `ignore` crate's
/// `hidden(true)`).
///
/// Needed because a watcher event may name a file that no longer exists, so
/// there is nothing left to walk — and gitignore rules cannot be evaluated
/// for it either. Anything this rejects was never indexed in the first place,
/// so rejecting it again on removal is harmless.
pub fn is_excluded_rel(rel: &Utf8Path) -> bool {
    rel.components().any(|c| {
        let name = c.as_str();
        is_hard_excluded(name) || (name.starts_with('.') && name != "." && name != "..")
    })
}

/// Enumerate indexable files, as project-relative forward-slash paths.
///
/// `start` must be `root` or a directory under it; `max_depth` of `Some(1)`
/// lists just that directory's own files, which is how a watcher event is
/// checked against the ignore rules without re-walking the project.
///
/// A cancelled token stops the walk between entries, so shutdown is not
/// stuck behind a large or slow (UNC, reconnecting) tree. The returned list
/// is then *partial* — callers must not treat it as the complete truth of
/// what exists (a prune against it would delete everything unwalked).
pub fn walk_files(
    root: &Utf8Path,
    start: &Utf8Path,
    max_depth: Option<usize>,
    data_dir: &Utf8Path,
    cancel: Option<&CancellationToken>,
) -> Vec<Utf8PathBuf> {
    if paths::is_within(data_dir, start) {
        return Vec::new();
    }

    let mut builder = WalkBuilder::new(start);
    builder
        .max_depth(max_depth)
        .follow_links(false)
        .hidden(true)
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        // A `.gitignore` expresses intent whether or not the directory has
        // been `git init`ed yet; the default (`require_git(true)`) would
        // silently index everything in a not-yet-initialized project.
        .require_git(false)
        // The developer's *global* gitignore is deliberately not consulted:
        // it would make what Lore indexes depend on unrelated machine state,
        // and make this walk untestable.
        .git_global(false);
    // Registered after the builder chain because it returns `&mut` rather
    // than the builder. Custom ignore files outrank `.gitignore`, so a
    // `.loreignore` can also *re-include* (`!pattern`) something git ignores.
    builder.add_custom_ignore_filename(LORE_IGNORE_FILE);

    let data_dir = data_dir.to_owned();
    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy();
        if is_hard_excluded(&name) {
            return false;
        }
        match Utf8Path::from_path(entry.path()) {
            Some(path) => !paths::is_within(&data_dir, path),
            // Non-UTF-8 paths cannot be stored (chunk ids are derived from
            // the path string), so there is no point descending into them.
            None => false,
        }
    });

    let mut out = Vec::new();
    for entry in builder.build() {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            break;
        }
        match entry {
            Ok(entry) => {
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let Some(abs) = Utf8Path::from_path(entry.path()) else {
                    tracing::debug!(path = %entry.path().display(), "skipping non-UTF-8 path");
                    continue;
                };
                if let Some(rel) = paths::relative_to(root, abs) {
                    out.push(rel);
                }
            }
            Err(err) => tracing::debug!(error = %err, "walk error"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a fixture tree with no data-dir overlap and no cancellation.
    fn walk(root: &Utf8Path) -> Vec<String> {
        let far_away = Utf8PathBuf::from("Z:/nowhere/lore-data");
        let mut files: Vec<String> = walk_files(root, root, None, &far_away, None)
            .into_iter()
            .map(|p| p.to_string())
            .collect();
        files.sort();
        files
    }

    fn fixture(spec: &[(&str, &str)]) -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        for (path, contents) in spec {
            let abs = root.join(path);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(abs, contents).unwrap();
        }
        (dir, root)
    }

    #[test]
    fn loreignore_excludes_without_any_git_metadata() {
        // A VCS-less workspace: no .git anywhere, so gitignore semantics
        // alone would index the telemetry.
        let (_dir, root) = fixture(&[
            ("src/main.rs", "fn main() {}"),
            ("telemetry/run1.jsonl", "{\"tick\":1}"),
            ("assets/big.asset", "yaml: 1"),
            (".loreignore", "telemetry/\n*.asset\n"),
        ]);
        assert_eq!(walk(&root), ["src/main.rs"]);
    }

    #[test]
    fn loreignore_nests_like_gitignore() {
        let (_dir, root) = fixture(&[
            ("a/keep.txt", "k"),
            ("a/logs/noise.txt", "n"),
            ("a/.loreignore", "logs/\n"),
            ("b/logs/kept.txt", "k"),
        ]);
        // Only `a`'s logs are ignored; `b` has no rule.
        assert_eq!(walk(&root), ["a/keep.txt", "b/logs/kept.txt"]);
    }

    #[test]
    fn loreignore_outranks_gitignore_for_reinclusion() {
        let (_dir, root) = fixture(&[
            ("data/model.onnx", "bin"),
            ("data/readme.md", "docs"),
            (".gitignore", "data/\n"),
            (".loreignore", "!data/\n!data/readme.md\ndata/*.onnx\n"),
        ]);
        assert_eq!(walk(&root), ["data/readme.md"]);
    }

    #[test]
    fn the_ignore_file_itself_is_hidden_from_the_index() {
        let (_dir, root) = fixture(&[("kept.rs", "x"), (".loreignore", "")]);
        assert_eq!(walk(&root), ["kept.rs"]);
    }
}
