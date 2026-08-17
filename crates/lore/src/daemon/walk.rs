//! What counts as an indexable file.
//!
//! One function decides this for the whole daemon, and both the full scan and
//! the incremental (watcher-driven) path go through it. That is the point:
//! if "is this file indexed?" had two implementations, a watcher event could
//! index something a rescan would then delete, forever.
//!
//! ## One evaluator, three sources (D-0020)
//!
//! Exactly one ignore evaluation decides what is observed — the `ignore`
//! crate's, with rule sources stacked lowest to highest:
//!
//! 1. the user's own [`USER_IGNORE_FILE`], beside `config.toml` in the daemon's
//!    data directory, applying to every project this machine indexes. Not
//!    installed by default (`lore setup loreignore` writes a commented starting
//!    point); absent is simply an empty source;
//! 2. the repo's own **`.gitignore`**, through the same evaluator as a courtesy
//!    to the user — no git subprocess, no tracked/untracked distinction, no
//!    `core.excludesFile`, no global gitignore, and untracked files are
//!    observed like any other. A repository's own declaration outranks a
//!    machine-wide preference;
//! 3. the project's **`.loreignore`**, registered as a *custom* ignore file,
//!    which the crate ranks above every other source. It is sovereign: it
//!    inherits rungs 1 and 2 by staying silent, and its `!` re-includes beat
//!    both.
//!
//! **Lore ships no compiled-in ignore rules at all.** Out of the box a project
//! is observed whole, minus the two mechanical exclusions below; hidden files
//! included. That is deliberate: a rule nobody can see is a rule nobody can
//! fix, and every exclusion lore applies is now a line in a file somebody can
//! read, at a precedence somebody can argue with.
//!
//! Uniform precedence is the whole substance of D-0020, which retired five
//! interacting rule systems (hard excludes, non-overridable credential
//! excludes, a git-aware basis taken by subprocess, `.loreignore`, and an
//! exact-path override key in `.lore.toml`) — each pair with its own quirk.
//! **Working rule for this repo: one evaluator, uniform precedence. A new rule
//! system needs a decision, not a code path.**
//!
//! ## What is outside the stack
//!
//! [`lore_core::snapshot::GIT_DIR`] — `.git` — is a hard floor, pruned by name
//! at any depth. It is not an ecosystem opinion but the mechanism rule source 2
//! is read out of, and it holds the remote's credentials. The receiver enforces
//! the same floor structurally, from the same constant.
//!
//! Plus two exclusions that are not rules about the project at all: the
//! daemon's own data directory (its SQLite WAL is a busy file living outside
//! every project root, but a project root could still be an ancestor of it),
//! and paths that are not UTF-8 (chunk ids are derived from the path string, so
//! they cannot be stored).

use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use lore_core::snapshot::GIT_DIR;
use tokio_util::sync::CancellationToken;

use super::paths;

/// Per-directory ignore file, gitignore syntax. Named for lore rather than
/// reusing `.ignore` (ripgrep's convention) so that a rule meant for search
/// tools and a rule meant for the index can differ — which is also why the
/// walker below leaves `.ignore` files switched off.
pub const LORE_IGNORE_FILE: &str = ".loreignore";

/// The user-level ignore file: `<data-dir>/loreignore`, beside `config.toml`
/// (on Windows, `%LOCALAPPDATA%\lore\loreignore`).
///
/// Undotted like `config.toml` beside it — the data directory is lore's own,
/// not a repository, and nothing there is hiding. Deliberately *not* a
/// machine-global file outside the data directory: `LORE_DATA_DIR` already
/// scopes every other piece of daemon state, and a rules file that ignored it
/// would make what lore indexes depend on state a test cannot control. A
/// system-level (`/etc`-style) source is out of scope.
pub const USER_IGNORE_FILE: &str = "loreignore";

/// Whether some component of `rel` is [`GIT_DIR`] — the one exclusion no rule
/// source can argue with.
///
/// Case-insensitively, because a Windows volume makes `.GIT` the same directory
/// as `.git` and a floor a rename defeats is not a floor.
///
/// Answers about a whole project-relative path rather than one walk component,
/// because the watcher names paths that no walk produced: an event may name a
/// file that no longer exists, so there is nothing left to walk and no ignore
/// rules can be evaluated for it either.
///
/// Deliberately the *only* thing checked this way. Under D-0020 every other
/// exclusion is an overridable rule, so a name-level shortcut for (say)
/// dot-files would quietly outrank a `.loreignore` that re-included one — the
/// exact layering D-0020 deleted.
pub fn is_git_metadata(rel: &Utf8Path) -> bool {
    rel.components()
        .any(|component| component.as_str().eq_ignore_ascii_case(GIT_DIR))
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
        // Hidden-ness is not lore's opinion to hold (D-0020). It is a rule like
        // any other, and the file that holds it is one of the three sources —
        // where a project can re-include a dot-file it wants indexed. This flag
        // is the same policy at a precedence nothing can argue with.
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        // Not among the three decided sources: `.git/info/exclude` lives inside
        // the hard floor, and `.ignore` is configuration for search tools
        // rather than for the index (see `LORE_IGNORE_FILE`).
        .git_exclude(false)
        .ignore(false)
        // A `.gitignore` expresses intent whether or not the directory has
        // been `git init`ed yet; the default (`require_git(true)`) would
        // silently index everything in a not-yet-initialized project.
        .require_git(false)
        // The developer's *global* gitignore is deliberately not consulted
        // (D-0020 names it): it would make what Lore indexes depend on
        // unrelated machine state. `USER_IGNORE_FILE` is the sanctioned
        // machine-wide source, and it lives where the rest of lore's state does.
        .git_global(false)
        // Must be set *before* `add_ignore` below, and must be the project
        // root: `add_ignore` roots the rules it reads at the builder's current
        // directory, and the walker matches absolute paths. Left to the process
        // CWD, a user-level `.*` would be tested against a path whose own
        // prefix may contain a dot component — and the walk would ignore the
        // entire project.
        .current_dir(root);
    // Registered after the builder chain because these return `&mut` rather
    // than the builder.
    //
    // The crate's precedence, highest first: custom ignore files, `.ignore`,
    // `.gitignore`, `.git/info/exclude`, the global gitignore, then explicit
    // ignore files ("lower precedence than all other sources", in its own
    // words). So the project's file as a *custom* one and the user's as an
    // *explicit* one put `.gitignore` between them — exactly the D-0020 stack.
    builder.add_custom_ignore_filename(LORE_IGNORE_FILE);
    let user_rules = data_dir.join(USER_IGNORE_FILE);
    // Existence checked here rather than left to `add_ignore`, because not
    // having one is the default state and not a condition to report.
    if user_rules.is_file() {
        // Partial failure is possible (one unparseable glob, the rest applied),
        // so this reports rather than bails: some of the user's rules are better
        // than none, and the line names the file to fix.
        if let Some(err) = builder.add_ignore(&user_rules) {
            tracing::warn!(path = %user_rules, error = %err, "user-level ignore rules did not fully load");
        }
    }

    let data_dir = data_dir.to_owned();
    builder.filter_entry(move |entry| {
        // A `filter_entry` rather than a rule: this is the one exclusion that
        // must not be re-includable, because the rules are read out of it.
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(GIT_DIR)
        {
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

    /// A fixture project plus the data directory the user-level rules live in.
    ///
    /// Both are temporary and the data directory is outside the project, which
    /// is what the daemon guarantees in practice.
    struct Fixture {
        _project: tempfile::TempDir,
        _data: tempfile::TempDir,
        root: Utf8PathBuf,
        data_dir: Utf8PathBuf,
    }

    impl Fixture {
        fn new(spec: &[(&str, &str)]) -> Self {
            let project = tempfile::tempdir().unwrap();
            let data = tempfile::tempdir().unwrap();
            let fixture = Self {
                root: Utf8PathBuf::from_path_buf(project.path().to_path_buf()).unwrap(),
                data_dir: Utf8PathBuf::from_path_buf(data.path().to_path_buf()).unwrap(),
                _project: project,
                _data: data,
            };
            for (path, contents) in spec {
                fixture.write(path, contents);
            }
            fixture
        }

        fn write(&self, path: &str, contents: &str) {
            let abs = self.root.join(path);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(abs, contents).unwrap();
        }

        /// Install user-level rules — rung 1, the lowest source.
        fn user_rules(&self, rules: &str) -> &Self {
            std::fs::write(self.data_dir.join(USER_IGNORE_FILE), rules).unwrap();
            self
        }

        fn walk(&self) -> Vec<String> {
            self.walk_from(&self.root.clone(), None)
        }

        fn walk_from(&self, start: &Utf8Path, max_depth: Option<usize>) -> Vec<String> {
            let mut files: Vec<String> =
                walk_files(&self.root, start, max_depth, &self.data_dir, None)
                    .into_iter()
                    .map(|p| p.to_string())
                    .collect();
            files.sort();
            files
        }
    }

    // -----------------------------------------------------------------------
    // Out of the box
    // -----------------------------------------------------------------------

    /// **Chosen, not overlooked** (D-0020): lore ships no ignore rules of
    /// its own. With no user-level file, no `.gitignore` and no `.loreignore`, a
    /// project is observed whole — build output, dot-files and a plaintext
    /// credential included. Every exclusion lore applies is a line in a file
    /// somebody can read and argue with, and the cost of that is this test.
    #[test]
    fn with_no_rules_anywhere_everything_is_observed() {
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            ("target/debug/build.rs", "generated"),
            ("node_modules/pkg/index.js", "vendored"),
            (".env", "API_TOKEN=hunter2"),
            (".github/workflows/ci.yml", "on: push"),
        ]);
        assert!(!fixture.data_dir.join(USER_IGNORE_FILE).exists());
        assert_eq!(
            fixture.walk(),
            [
                ".env",
                ".github/workflows/ci.yml",
                "node_modules/pkg/index.js",
                "src/main.rs",
                "target/debug/build.rs",
            ]
        );
    }

    // -----------------------------------------------------------------------
    // The three sources, and which one wins (D-0020)
    // -----------------------------------------------------------------------

    /// Rung 1 applies wherever the higher sources are silent — which is what
    /// makes a user-level file worth installing at all.
    #[test]
    fn user_level_rules_apply_when_the_project_is_silent() {
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            ("target/debug/build.rs", "generated"),
            (".env", "API_TOKEN=hunter2"),
            ("game/Library/cache.dat", "unity"),
        ]);
        fixture.user_rules(".*\n[Tt]arget/\n[Ll]ibrary/\n");
        assert_eq!(fixture.walk(), ["src/main.rs"]);
    }

    /// Rung 2 over rung 1: a repository's own declaration outranks a
    /// machine-wide preference, in both directions.
    #[test]
    fn gitignore_outranks_the_user_level_file() {
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            ("logs/keep.log", "wanted here"),
            ("scratch/notes.txt", "n"),
        ]);
        fixture.user_rules("*.log\n");
        // The repo re-includes what the user excluded…
        fixture.write(".gitignore", "!*.log\nscratch/\n");
        // …and its own exclusion applies with no git binary in sight.
        //
        // `.gitignore` is in the listing because nothing excludes it: lore has
        // no dot-file rule of its own now. See
        // `the_ignore_file_itself_is_excluded_by_the_dot_file_rule`.
        assert_eq!(
            fixture.walk(),
            [".gitignore", "logs/keep.log", "src/main.rs"]
        );
    }

    /// Rung 3 over rung 2: the sovereign file's re-include beats `.gitignore`.
    #[test]
    fn a_loreignore_reinclusion_beats_gitignore() {
        let fixture = Fixture::new(&[
            ("data/model.onnx", "bin"),
            ("data/readme.md", "docs"),
            (".gitignore", "data/\n"),
            (".loreignore", "!data/\n!data/readme.md\ndata/*.onnx\n"),
        ]);
        assert_eq!(
            fixture.walk(),
            [".gitignore", ".loreignore", "data/readme.md"]
        );
    }

    /// Rung 3 over rung 1, on the line that matters most: a credential rule is
    /// an ordinary rule, and a committed `!` beats it.
    ///
    /// This is the accepted trade stated in D-0020 — a bad ignore file can admit
    /// a secret; hygiene is best-effort user responsibility and an encrypted
    /// store is the substantive measure. Asserted rather than merely allowed,
    /// because the previous model (D-0015/D-0017) refused it and the change is
    /// the point.
    #[test]
    fn a_loreignore_reinclusion_beats_a_user_level_credential_rule() {
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            (".env.example", "API_TOKEN=<yours here>"),
            (".env", "API_TOKEN=hunter2"),
            (".loreignore", "!.env.example\n"),
        ]);
        fixture.user_rules(".*\n.env\n.env.*\n");
        // Exactly what was named: the real `.env` beside it still loses.
        assert_eq!(fixture.walk(), [".env.example", "src/main.rs"]);
    }

    /// Sovereignty is *layering*, not replacement: a project's file inherits
    /// every user-level rule it does not mention. A `.loreignore` that exists
    /// but says nothing about `target/` still gets `target/`.
    #[test]
    fn a_project_file_inherits_the_user_level_rules_it_is_silent_about() {
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            ("target/debug/build.rs", "generated"),
            ("notes/scratch.md", "n"),
            (".loreignore", "notes/\n"),
        ]);
        // `.*` as well, so the project's own ignore file stays out of the
        // listings below and the inheritance is the only thing on show.
        fixture.user_rules(".*\n[Tt]arget/\n");
        assert_eq!(fixture.walk(), ["src/main.rs"]);

        // Including when it is empty — a file that says nothing overrides
        // nothing, and saying it takes a `!` line like every other override.
        fixture.write(".loreignore", "");
        assert_eq!(fixture.walk(), ["notes/scratch.md", "src/main.rs"]);
        fixture.write(".loreignore", "![Tt]arget/\n");
        assert_eq!(
            fixture.walk(),
            ["notes/scratch.md", "src/main.rs", "target/debug/build.rs"]
        );
    }

    /// Untracked files are observed like any other (D-0020 retires the
    /// tracked/untracked distinction), and `.gitignore` is honoured whether or
    /// not the directory was ever `git init`ed.
    #[test]
    fn gitignore_applies_without_git_and_untracked_files_are_observed() {
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            ("src/brand_new.rs", "just written by an agent"),
            ("build.log", "noise"),
            (".gitignore", "*.log\n"),
        ]);
        assert!(!fixture.root.join(".git").exists());
        assert_eq!(
            fixture.walk(),
            [".gitignore", "src/brand_new.rs", "src/main.rs"]
        );
    }

    #[test]
    fn loreignore_excludes_without_any_git_metadata() {
        // A VCS-less workspace: no .git anywhere, so gitignore semantics
        // alone would index the telemetry.
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            ("telemetry/run1.jsonl", "{\"tick\":1}"),
            ("assets/big.asset", "yaml: 1"),
            (".loreignore", "telemetry/\n*.asset\n"),
        ]);
        assert_eq!(fixture.walk(), [".loreignore", "src/main.rs"]);
    }

    #[test]
    fn loreignore_nests_like_gitignore() {
        let fixture = Fixture::new(&[
            ("a/keep.txt", "k"),
            ("a/logs/noise.txt", "n"),
            ("a/.loreignore", "logs/\n"),
            ("b/logs/kept.txt", "k"),
        ]);
        // Only `a`'s logs are ignored; `b` has no rule.
        assert_eq!(
            fixture.walk(),
            ["a/.loreignore", "a/keep.txt", "b/logs/kept.txt"]
        );
    }

    /// The ignore files are policy rather than content, but nothing special
    /// keeps them out of the index — a user-level `.*`, or the project's own
    /// rule, is all that does.
    #[test]
    fn the_ignore_file_itself_is_excluded_by_the_dot_file_rule() {
        let fixture = Fixture::new(&[("kept.rs", "x"), (".loreignore", "")]);
        assert_eq!(
            fixture.walk(),
            [".loreignore", "kept.rs"],
            "with no rule saying otherwise, it is just a file"
        );
        fixture.user_rules(".*\n");
        assert_eq!(fixture.walk(), ["kept.rs"]);
    }

    // -----------------------------------------------------------------------
    // Scoped listings — the incremental path must agree with the full scan
    // -----------------------------------------------------------------------

    /// The incremental path lists one directory; it must reach the same
    /// verdict the full scan does, including when the rule that excludes it
    /// names an *ancestor* of that directory rather than the directory
    /// itself.
    #[test]
    fn listing_a_subdirectory_obeys_the_rules_that_apply_to_its_ancestors() {
        let fixture = Fixture::new(&[
            ("keep.rs", "x"),
            ("build/out/gen.rs", "generated"),
            (".loreignore", "build/\n"),
        ]);
        assert!(
            fixture
                .walk_from(&fixture.root.join("build/out"), Some(1))
                .is_empty()
        );
        assert_eq!(fixture.walk(), [".loreignore", "keep.rs"]);
    }

    /// Same, for rung 1: the lowest source has to reach a scoped listing too,
    /// or a watcher event would index what the next full scan deletes.
    #[test]
    fn listing_a_subdirectory_obeys_the_user_level_rules_too() {
        let fixture = Fixture::new(&[
            ("target/debug/build.rs", "generated"),
            (".env", "API_TOKEN=hunter2"),
        ]);
        fixture.user_rules("[Tt]arget/\n.env\n");
        assert!(
            fixture
                .walk_from(&fixture.root.join("target/debug"), Some(1))
                .is_empty()
        );
        assert!(fixture.walk_from(&fixture.root.clone(), Some(1)).is_empty());
    }

    /// Rooting the walk above `start` must not widen what it returns.
    #[test]
    fn a_depth_limited_listing_returns_only_that_directorys_own_files() {
        let fixture = Fixture::new(&[
            ("top.rs", "x"),
            ("a/one.rs", "x"),
            ("a/b/two.rs", "x"),
            ("z/other.rs", "x"),
        ]);
        assert_eq!(
            fixture.walk_from(&fixture.root.join("a"), Some(1)),
            ["a/one.rs"]
        );
        assert_eq!(
            fixture.walk_from(&fixture.root.join("a"), None),
            ["a/b/two.rs", "a/one.rs"]
        );
    }

    // -----------------------------------------------------------------------
    // The hard floor
    // -----------------------------------------------------------------------

    /// `.git` is the one name the user cannot argue with: the ignore rules are
    /// read out of it, and it holds the remote's credentials.
    #[test]
    fn git_metadata_is_never_indexed() {
        let fixture = Fixture::new(&[
            ("kept.rs", "x"),
            (".git/config", "[core]"),
            (".git/objects/ab/cdef", "blob"),
        ]);
        assert_eq!(fixture.walk(), ["kept.rs"]);

        // Even when the sovereign file tries to re-include it, which every
        // other rule here would obey.
        fixture.write(".loreignore", "!.git/\n!.git/*\n");
        assert_eq!(fixture.walk(), [".loreignore", "kept.rs"]);
    }

    /// The path-level form, for the watcher: it names paths no walk produced.
    #[test]
    fn the_floor_answers_about_a_whole_path_at_any_depth_and_case() {
        for path in [".git/config", "vendor/dep/.git/HEAD", ".GIT/config"] {
            assert!(is_git_metadata(Utf8Path::new(path)), "{path}");
        }
        // Named for git without being git's directory — and, crucially, nothing
        // else is checked this way, because everything else is overridable.
        for path in [".gitignore", "src/.gitkeep", ".env", "src/main.rs"] {
            assert!(!is_git_metadata(Utf8Path::new(path)), "{path}");
        }
    }

    /// The daemon's own data directory is not a rule either — and a project
    /// root can be an ancestor of it.
    #[test]
    fn the_daemons_data_directory_is_never_walked() {
        let project = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(project.path().to_path_buf()).unwrap();
        // Joined component by component: the containment check compares strings
        // against what the walker reports, which uses the platform separator.
        let data_dir = root.join("state").join("lore");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("index.sqlite3"), "db").unwrap();
        std::fs::write(root.join("keep.rs"), "x").unwrap();

        let mut files: Vec<String> = walk_files(&root, &root, None, &data_dir, None)
            .into_iter()
            .map(|p| p.to_string())
            .collect();
        files.sort();
        assert_eq!(files, ["keep.rs"]);
        assert!(walk_files(&root, &data_dir, None, &data_dir, None).is_empty());
    }
}
