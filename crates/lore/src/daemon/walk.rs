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
//! 3. Otherwise ripgrep's rules via the `ignore` crate: `.gitignore`
//!    (including nested and parent files), `.git/info/exclude`, hidden files
//!    skipped.

use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;

use super::paths;

/// Directory names never descended into, at any depth, regardless of what
/// `.gitignore` says. Unity (`Library`, `Temp`, `obj`) is the flagship case
/// (D-0003): those trees are machine-generated, gigantic, and rewritten on
/// every editor launch.
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
pub fn walk_files(
    root: &Utf8Path,
    start: &Utf8Path,
    max_depth: Option<usize>,
    data_dir: &Utf8Path,
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
