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
//! 2. Never a directory named in [`HARD_EXCLUDES`] — which is `.git` alone.
//!    Anything more opinionated than that is ecosystem policy, and ecosystem
//!    policy belongs in a file the user can read and edit.
//! 3. [`LORE_IGNORE_FILE`] (`.loreignore`) — gitignore syntax, nested like
//!    `.gitignore`, and honored **regardless of VCS**. This is the
//!    user-visible knob, generated per project by
//!    [`super::ignorefile`]: a Unity VCS or Perforce workspace has no
//!    `.gitignore`, so without it telemetry dumps and serialized-asset YAML
//!    index as if they were code.
//! 4. Otherwise ripgrep's rules via the `ignore` crate: `.gitignore`
//!    (including nested and parent files), `.git/info/exclude`, hidden files
//!    skipped.
//!
//! Plus one safety net: a project root with **no** `.loreignore` at all gets
//! [`FALLBACK_EXCLUDES`] applied in memory. See that constant for why.

use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;

use super::paths;

/// Per-directory ignore file, gitignore syntax. Named for lore rather than
/// reusing `.ignore` (ripgrep's convention) so that a rule meant for search
/// tools and a rule meant for the index can differ.
pub const LORE_IGNORE_FILE: &str = ".loreignore";

/// Directory names never descended into, at any depth, whatever any ignore
/// file says. Only `.git`: it is not an ecosystem opinion but the mechanism
/// the ignore rules are themselves read from.
pub const HARD_EXCLUDES: &[&str] = &[".git"];

/// The exclusion list Lore used before `.loreignore` existed, applied in
/// memory **only** while a project root has no `.loreignore` at all.
///
/// The file is generated at registration and at every full scan, so this list
/// is normally dead weight. It matters when generation could not happen — a
/// read-only root, a permissions failure — where the alternative is quietly
/// indexing a Unity `Library/` or a `node_modules/`, which is the failure
/// this whole module exists to prevent.
///
/// An existing `.loreignore` disables it entirely, **including an empty one**:
/// that is the deliberate "index everything" escape hatch, and a fallback
/// that survived it would make the escape hatch a lie.
pub const FALLBACK_EXCLUDES: &[&str] = &[
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

/// Case-insensitive: `Library` on a case-insensitive Windows volume is the
/// same directory as `library`, and Unity is inconsistent about casing.
fn matches_any(names: &[&str], name: &str) -> bool {
    names.iter().any(|entry| entry.eq_ignore_ascii_case(name))
}

pub fn is_hard_excluded(name: &str) -> bool {
    matches_any(HARD_EXCLUDES, name)
}

/// Cheap, stat-free rejection of a project-relative path: hard-excluded or
/// hidden (a component starting with `.`, matching the `ignore` crate's
/// `hidden(true)`).
///
/// Needed because a watcher event may name a file that no longer exists, so
/// there is nothing left to walk — and gitignore rules cannot be evaluated
/// for it either. Anything this rejects was never indexed in the first place,
/// so rejecting it again on removal is harmless.
///
/// Deliberately *not* consulting [`FALLBACK_EXCLUDES`]: this has no project
/// root to test for a `.loreignore`, and a hardcoded list here would silently
/// drop watcher events for a `target/` the user re-included. The cost is that
/// a build's churn now reaches [`walk_files`] instead of being rejected on
/// the name alone; the batch is coalesced and the walk still rejects it.
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

    // The walk always begins at the project root, even when only a
    // subdirectory was asked for; the predicate below prunes it back to that
    // subdirectory plus the chain of directories leading to it.
    //
    // Rooting it at `start` instead loses every ignore rule that applies to
    // `start`'s *ancestors*: the `ignore` crate matches a parent `.gitignore`
    // against the entry path alone, so a `target/` rule prunes the `target`
    // directory during a descent and matches nothing whatsoever once the walk
    // begins inside it. A watcher event for `target/debug/build.rs` would
    // then be indexed by the incremental path and deleted by the next full
    // scan, forever.
    let scope = paths::relative_to(root, start);
    let offset = scope.as_ref().map_or(0, |rel| rel.components().count());
    // Rebuilt component by component rather than reused as given: callers
    // form `start` as `root.join(rel)` where `rel` is slash-separated, and the
    // containment checks below compare strings against what the walker
    // reports, which uses the platform separator throughout.
    let scope = scope.map(|rel| {
        rel.components()
            .fold(root.to_owned(), |path, part| path.join(part.as_str()))
    });

    let mut builder = WalkBuilder::new(root);
    builder
        // `max_depth` stays relative to `start`, which is now `offset` levels
        // below the walk root.
        .max_depth(max_depth.map(|depth| depth + offset))
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

    // Decided from the *project root*, not from `start`: a watcher-driven
    // single-directory walk has to reach the same verdict as the full scan it
    // must agree with, or the two would fight over the same files forever.
    let fallback = !root.join(LORE_IGNORE_FILE).exists();

    let data_dir = data_dir.to_owned();
    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy();
        if is_hard_excluded(&name) {
            return false;
        }
        // A `filter_entry` rather than an `Override`: this must reject by
        // directory *name* at any depth, exactly as the old hardcoded list
        // did, and must not be re-includable — an override set would let a
        // nested `.loreignore` argue with a rule that only exists because
        // there is no `.loreignore` to argue with.
        if fallback && matches_any(FALLBACK_EXCLUDES, &name) {
            return false;
        }
        match Utf8Path::from_path(entry.path()) {
            Some(path) => {
                if paths::is_within(&data_dir, path) {
                    return false;
                }
                // Keep `start`'s subtree and the directories on the way down
                // to it; everything else is a sibling branch this call was
                // never asked about.
                match &scope {
                    Some(scope) => paths::is_within(scope, path) || paths::is_within(path, scope),
                    None => true,
                }
            }
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
        walk_from(root, root, None)
    }

    fn walk_from(root: &Utf8Path, start: &Utf8Path, max_depth: Option<usize>) -> Vec<String> {
        let far_away = Utf8PathBuf::from("Z:/nowhere/lore-data");
        let mut files: Vec<String> = walk_files(root, start, max_depth, &far_away, None)
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
        // No root `.loreignore`, so the in-memory fallback is live; none of
        // these names are in it, so it changes nothing here.
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

    /// The safety net: generation failed (or has not run yet), there is no
    /// VCS metadata either, and the build trees still must not be indexed.
    #[test]
    fn without_a_loreignore_the_built_in_exclusions_apply_at_any_depth() {
        let (_dir, root) = fixture(&[
            ("src/main.rs", "fn main() {}"),
            ("target/debug/build.rs", "generated"),
            ("node_modules/pkg/index.js", "vendored"),
            ("game/Library/ScriptAssemblies/Asm.dll.txt", "unity"),
            ("game/deep/nested/obj/Debug/gen.cs", "msbuild"),
            (".idea/workspace.xml", "editor"),
        ]);
        assert!(!root.join(LORE_IGNORE_FILE).exists());
        assert_eq!(walk(&root), ["src/main.rs"]);
    }

    /// The escape hatch, and the reason the fallback keys off *existence*
    /// rather than content: an empty file is a complete statement.
    #[test]
    fn an_empty_loreignore_disables_the_fallback_entirely() {
        let (_dir, root) = fixture(&[
            ("src/main.rs", "fn main() {}"),
            ("node_modules/pkg/index.js", "vendored"),
            ("target/debug/build.rs", "generated"),
            (".loreignore", ""),
        ]);
        assert_eq!(
            walk(&root),
            [
                "node_modules/pkg/index.js",
                "src/main.rs",
                "target/debug/build.rs"
            ]
        );
    }

    /// A nested `.loreignore` is not the project's `.loreignore`: the
    /// fallback is a property of the root, so a rule three directories down
    /// cannot switch it off.
    #[test]
    fn a_nested_loreignore_does_not_disable_the_fallback() {
        let (_dir, root) = fixture(&[
            ("app/src/main.rs", "fn main() {}"),
            ("app/node_modules/pkg/index.js", "vendored"),
            ("app/.loreignore", "*.tmp\n"),
        ]);
        assert_eq!(walk(&root), ["app/src/main.rs"]);
    }

    /// The incremental path lists one directory; it must reach the same
    /// verdict the full scan does, including when the rule that excludes it
    /// names an *ancestor* of that directory rather than the directory
    /// itself.
    #[test]
    fn listing_a_subdirectory_obeys_the_rules_that_apply_to_its_ancestors() {
        let (_dir, root) = fixture(&[
            ("keep.rs", "x"),
            ("build/out/gen.rs", "generated"),
            (".loreignore", "build/\n"),
        ]);
        assert!(walk_from(&root, &root.join("build/out"), Some(1)).is_empty());
        assert_eq!(walk(&root), ["keep.rs"]);
    }

    /// Same, for the fallback: no `.loreignore` at all, and the listing still
    /// must not hand back a build tree.
    #[test]
    fn listing_a_subdirectory_obeys_the_fallback_too() {
        let (_dir, root) = fixture(&[("target/debug/build.rs", "generated")]);
        assert!(walk_from(&root, &root.join("target/debug"), Some(1)).is_empty());
    }

    /// Rooting the walk above `start` must not widen what it returns.
    #[test]
    fn a_depth_limited_listing_returns_only_that_directorys_own_files() {
        let (_dir, root) = fixture(&[
            ("top.rs", "x"),
            ("a/one.rs", "x"),
            ("a/b/two.rs", "x"),
            ("z/other.rs", "x"),
        ]);
        assert_eq!(walk_from(&root, &root.join("a"), Some(1)), ["a/one.rs"]);
        assert_eq!(
            walk_from(&root, &root.join("a"), None),
            ["a/b/two.rs", "a/one.rs"]
        );
    }

    /// `.git` is the one name the user cannot argue with, in either state of
    /// the fallback: the ignore rules are read out of it.
    #[test]
    fn git_metadata_is_never_indexed() {
        let (_dir, root) = fixture(&[
            ("kept.rs", "x"),
            (".git/config", "[core]"),
            (".git/objects/ab/cdef", "blob"),
        ]);
        assert_eq!(walk(&root), ["kept.rs"]);

        // Even when a `.loreignore` disables the fallback and tries to
        // re-include it.
        std::fs::write(root.join(LORE_IGNORE_FILE), "!.git/\n").unwrap();
        assert_eq!(walk(&root), ["kept.rs"]);
    }
}
