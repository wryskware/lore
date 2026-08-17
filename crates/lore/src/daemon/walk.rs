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
//!    installed by default (`lore setup ignore` writes a commented starting
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
//! ## Where the project stops
//!
//! One thing decides *descent* rather than interestingness, and so is not a
//! fourth rule source: the walk does not enter a subdirectory that is itself a
//! repository root (see [`is_repo_root`]). Everything under such a directory is
//! another repository's content — most concretely an agent tool's `git
//! worktree` of this very project, which is a byte-for-byte second copy of it,
//! chunked twice and competing with itself in the rankings.
//!
//! This does not reopen D-0020's working rule. It is not an opinion about what
//! kind of file is worth indexing — the thing D-0020 said belongs in a file the
//! user can read — but about *whose* project a directory is, a question no
//! ignore file asks and the `ignore` crate does not answer either. And it stays
//! under the sovereign file: a `!` re-include in the project's own
//! `.loreignore` rescues a deliberately vendored repository, so precedence is
//! unchanged.
//!
//! ## Links do not extend the project (D-0021)
//!
//! **Filesystem topology does not define project topology.** Symbolic links
//! are not followed — not into a directory, not through a link to a file
//! (which would place one content at two logical paths in a store keyed
//! `(project, path)`), and emphatically not out of the root, so that no link
//! can turn a project into an index of `~/secrets` or a company mount. On
//! Windows a directory **junction** is a symbolic link throughout: the
//! platform sets the reparse-point attribute with a name-surrogate tag and
//! Rust answers `is_symlink()` for it, so nothing here distinguishes the two
//! and no caller has to either.
//!
//! Like the repository boundary above this decides *descent* and is not a
//! fourth rule source. **Unlike it, there is no override**, and the asymmetry
//! is the point rather than an oversight: a `!` re-include on a vendored
//! repository answers *whose content is this*, and its bytes are already under
//! the root so the owner may answer "mine". A `!` on a link would instead
//! extend the project's *extent* to another part of the filesystem. Same
//! syntax, different act; they do not share a mechanism. There is no
//! `follow_symlinks` option and there is not going to be one — it would
//! arrive with cycles, duplicate aliases, out-of-root escapes, ambiguous rule
//! semantics for content reachable at two paths, and watches to arm on
//! targets, all at once.
//!
//! Creating, deleting or retargeting a link is an ordinary filesystem event
//! and is processed like one, so indexed state that has gone stale reconciles.
//! What is not done is traversing the new target.
//!
//! The one link that *is* resolved is the project root itself, once, at
//! registration ([`paths::canonicalize_root`]) — so what is walked, watched
//! and compared against is always the physical root.
//!
//! What was missing was never the traversal, it was the *account*: a workspace
//! assembled from junctions indexed its loose files and reported nothing, so it
//! read as a project that simply had almost nothing in it — and no edit to its
//! `.loreignore` could change that, because ignore rules are never consulted
//! for a tree the walk does not enter. [`Walk::links`] is that account.
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

/// Is `dir` the root of its own repository?
///
/// Deliberately an *existence* test and never a directory test, and that is the
/// whole subtlety: in an ordinary clone [`GIT_DIR`] is a directory, but in a
/// linked worktree — and in a submodule under git's modern layout — it is a
/// **file** holding a `gitdir:` pointer. A rule that only looked for the
/// directory would miss exactly the case that motivated this one. The same
/// constant the hard floor reads, asked a different question: the floor asks
/// whether a *path component* is git's metadata, this asks whether an entry by
/// that name *exists here*.
///
/// The walker refuses to descend into such a directory when it is *below* the
/// project root, because everything under it belongs to another repository and
/// indexing it duplicates that repository inside this project — every file
/// chunked twice, both copies competing in the rankings, the embedding cost
/// paid twice. The concrete case is an agent tool's `git worktree` living
/// inside the repo it was made from, which is a byte-for-byte second copy.
///
/// The `ignore` crate does **not** do this. It knows about repository
/// boundaries only for deciding which `.gitignore` files apply
/// (`require_git`); it walks straight through a nested clone and a nested
/// worktree alike, under either setting. That is established by
/// `the_ignore_crate_walks_into_a_nested_repository_on_its_own`, which pins the
/// upstream behavior so a crate upgrade that changed it would be noticed rather
/// than silently making this rule redundant.
///
/// This is a *default*, not a floor like [`GIT_DIR`]: vendoring a repository
/// into a project and wanting it indexed is legitimate, and `!vendor/dep/` in
/// the project's own `.loreignore` re-includes it. Under D-0020 the boundary is
/// also the only thing standing between the index and a tool's worktrees under
/// a dot-directory (`.claude/worktrees/`), because `hidden(false)` means
/// nothing prunes those by name any more.
pub fn is_repo_root(dir: &Utf8Path) -> bool {
    dir.join(GIT_DIR).exists()
}

/// What one walk observed.
///
/// Two lists rather than one, because "nothing here" and "something here that
/// lore declined to enter" are different answers and only the second one is a
/// thing a user can act on. A workspace assembled from directory links reports
/// a handful of loose files either way; only [`Self::links`] says why.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Walk {
    /// Indexable files, as project-relative forward-slash paths.
    pub files: Vec<Utf8PathBuf>,
    /// Symbolic links the walk did not follow, project-relative. On Windows
    /// this includes directory **junctions**, which the platform reports as
    /// links like any other (see [`walk_files`]).
    pub links: Vec<Utf8PathBuf>,
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
///
/// Links are **not followed** and are reported instead, in [`Walk::links`]
/// (D-0021 — see the module header for why there is no override). The walker
/// sets `follow_links(false)`, so a link is yielded as an entry whose type is
/// neither file nor directory; without the report it would be dropped on the
/// floor and a project whose whole content sits behind one would index its
/// loose files and say nothing about the rest.
pub fn walk_files(
    root: &Utf8Path,
    start: &Utf8Path,
    max_depth: Option<usize>,
    data_dir: &Utf8Path,
    cancel: Option<&CancellationToken>,
) -> Walk {
    if paths::is_within(data_dir, start) {
        return Walk::default();
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
        // Ignore files *above* the walk root are not consulted. The crate
        // defaults this on for ripgrep, where it is right: a search root is ad
        // hoc (`cd src && rg foo`), so reading the ancestors reconstructs the
        // repository context the user never got to state. A lore root is
        // *declared* — `lore add` names where the project starts — so there is
        // no missing context to reconstruct, and reading past the boundary
        // just ignores what the user said.
        //
        // Left on it is worse here than in ripgrep, because `.loreignore` is
        // registered below as a *custom* ignore filename and the crate's parent
        // scan reads custom names too, walking to the filesystem root: a
        // `.loreignore` in a home directory would silently govern every project
        // beneath it. That is the same objection `git_global(false)` is set for
        // a few lines down, arriving through a different door — and it is what
        // makes each declared root genuinely independent rather than nominally
        // so.
        //
        // The cost, stated: a subdirectory of a monorepo registered as its own
        // project stops inheriting the monorepo root's `.gitignore`. Under
        // D-0020 that project says what it wants in its own file, which is the
        // whole posture.
        .parents(false)
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

    // The one escape hatch for the nested-repository boundary, built from the
    // *project's* own `.loreignore` and nothing else. A nested repository is
    // skipped during descent, so the crate's own matcher stack never gets to
    // weigh in on it — this asks the same question the crate would have asked,
    // over the one file whose author is unambiguously the project owner rather
    // than the vendored repository itself. Deliberately not the user-level file
    // (a machine-wide preference cannot know which of this project's
    // subdirectories is a legitimate vendored copy) and not `.gitignore` (which
    // a vendored repository ships one of).
    //
    // A malformed line is ignored rather than failing the walk; the `ignore`
    // crate reports the same line again when it parses the file for real.
    let reinclude = {
        let mut hatch = ignore::gitignore::GitignoreBuilder::new(root);
        let _ = hatch.add(root.join(LORE_IGNORE_FILE));
        hatch.build().ok()
    };

    let root_owned = root.to_owned();
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
                // Where the project stops. Strictly *below* the root, because
                // the project root is itself a repository root in the
                // overwhelmingly common case — and a linked worktree registered
                // directly as its own project must index normally rather than
                // returning nothing at all.
                //
                // Belt and braces today: this crate version does not run
                // `filter_entry` against the walk root, so the guard is
                // load-bearing only if that changes. It is one string compare,
                // and the alternative is a rule whose most important
                // must-not-break case rests on an undocumented upstream detail.
                if entry.file_type().is_some_and(|kind| kind.is_dir())
                    && paths::relative_to(&root_owned, path).is_some()
                    && is_repo_root(path)
                    && !reinclude
                        .as_ref()
                        .is_some_and(|set| set.matched(path, true).is_whitelist())
                {
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

    let mut walk = Walk::default();
    for entry in builder.build() {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            break;
        }
        match entry {
            Ok(entry) => {
                let kind = entry.file_type();
                let Some(abs) = Utf8Path::from_path(entry.path()) else {
                    tracing::debug!(path = %entry.path().display(), "skipping non-UTF-8 path");
                    continue;
                };
                let Some(rel) = paths::relative_to(root, abs) else {
                    continue;
                };
                // Checked before `is_file`, because with `follow_links(false)`
                // a link is neither a file nor a directory: falling through to
                // the file test would silently discard the one entry a user
                // needs told about.
                if kind.is_some_and(|kind| kind.is_symlink()) {
                    walk.links.push(rel);
                } else if kind.is_some_and(|kind| kind.is_file()) {
                    walk.files.push(rel);
                }
            }
            Err(err) => tracing::debug!(error = %err, "walk error"),
        }
    }
    walk
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
            sorted(walk_files(&self.root, start, max_depth, &self.data_dir, None).files)
        }

        /// The other half of the same walk: what it declined to follow.
        fn links(&self) -> Vec<String> {
            sorted(walk_files(&self.root, &self.root, None, &self.data_dir, None).links)
        }

        /// A walk rooted somewhere other than this fixture's own root — for a
        /// checkout that is registered as a project in its own right. Only the
        /// data directory (and so the user-level rules) is shared.
        fn walk_rooted_at(&self, root: &Utf8Path) -> Vec<String> {
            sorted(walk_files(root, root, None, &self.data_dir, None).files)
        }
    }

    fn sorted(paths: Vec<Utf8PathBuf>) -> Vec<String> {
        let mut out: Vec<String> = paths.into_iter().map(|p| p.to_string()).collect();
        out.sort();
        out
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

    /// **The stack has three rungs and they all live at or below the root.**
    ///
    /// The `ignore` crate defaults to reading ignore files from every ancestor
    /// of the walk root, up to the filesystem root, and — because
    /// `.loreignore` is registered as a *custom* ignore filename — that
    /// includes `.loreignore`. Left on, a file in a home directory would
    /// silently govern every project beneath it: a fourth rung D-0020 never
    /// described, invisible in the project, and unmentioned by `lore status`.
    ///
    /// It is the same objection `git_global(false)` exists for — what lore
    /// indexes must not depend on unrelated machine state — and it is what
    /// makes a declared root actually independent. This test is the whole of
    /// that guarantee; nothing else would notice the crate default coming back
    /// on a version bump.
    ///
    /// Found in the wild rather than by inspection: a stray `/tmp/.gitignore`
    /// on a Linux box excluded `target/` and `node_modules/` from every
    /// tempdir fixture, failing exactly the tests that exist to deny it.
    #[test]
    fn rules_above_the_root_do_not_reach_into_the_project() {
        let outer = tempfile::tempdir().unwrap();
        let outer = Utf8PathBuf::from_path_buf(outer.path().to_path_buf()).unwrap();
        // Two levels up, so this cannot pass by only stopping at the parent.
        std::fs::write(outer.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(outer.join(LORE_IGNORE_FILE), "secret-notes/\n").unwrap();

        let data = tempfile::tempdir().unwrap();
        let data = Utf8PathBuf::from_path_buf(data.path().to_path_buf()).unwrap();
        let root = outer.join("nested").join("project");
        for (rel, body) in [
            ("main.rs", "fn main() {}"),
            ("target/debug/build.rs", "generated"),
            ("secret-notes/plan.md", "mine"),
        ] {
            let abs = root.join(rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(abs, body).unwrap();
        }

        assert_eq!(
            sorted(walk_files(&root, &root, None, &data, None).files),
            ["main.rs", "secret-notes/plan.md", "target/debug/build.rs"],
            "an ancestor's rules — of either kind — are not this project's"
        );
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
    // Where the project stops: nested repository roots
    //
    // Real checkouts throughout, never a hand-built directory shape: the whole
    // question is what `git worktree add` actually puts on disk (a `.git`
    // *file*) versus what a clone does (a `.git` directory), and a mocked
    // fixture would be a restatement of this module's assumptions rather than a
    // test of them. These are the only tests in the crate that need a git
    // binary; D-0020 retired every other use of one.
    // -----------------------------------------------------------------------

    fn git_in(dir: &Utf8Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git is on PATH");
        assert!(
            output.status.success(),
            "git {args:?} in {dir}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository with one commit — `git worktree add` needs a commit to
    /// check out.
    fn init_repo(dir: &Utf8Path) {
        std::fs::create_dir_all(dir).unwrap();
        git_in(dir, &["init", "-q", "."]);
        git_in(
            dir,
            &["config", "--local", "user.email", "t@example.invalid"],
        );
        git_in(dir, &["config", "--local", "user.name", "test"]);
        git_in(dir, &["commit", "-q", "--allow-empty", "-m", "root"]);
    }

    /// Commit whatever the fixture wrote, so there is content for a worktree to
    /// check out a second copy of.
    fn commit_all(root: &Utf8Path) {
        init_repo(root);
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-q", "-m", "content"]);
    }

    /// A project root that is a real repository, with a real linked worktree of
    /// itself nested inside it and a real second repository vendored in.
    fn nested_checkouts() -> Fixture {
        let fixture = Fixture::new(&[("src/main.rs", "fn main() {}"), ("keep.md", "# notes")]);
        commit_all(&fixture.root);
        // The defect, exactly: an agent tool's worktree of this very
        // repository, living inside it. Every committed file appears twice.
        git_in(
            &fixture.root,
            &["worktree", "add", "-q", "wt/agent", "-b", "agent"],
        );
        // And an ordinary nested clone, whose `.git` is a directory.
        let vendored = fixture.root.join("vendor/dep");
        init_repo(&vendored);
        std::fs::write(vendored.join("lib.rs"), "pub fn dep() {}").unwrap();
        fixture
    }

    /// What the `ignore` crate does on its own, pinned. Its repository-boundary
    /// notions (`require_git`) decide which `.gitignore` files apply, **not**
    /// where the walk stops: under either setting it descends into a nested
    /// worktree and a nested clone alike. If a crate upgrade ever changed that,
    /// this failing would be the notice that [`is_repo_root`] had become
    /// redundant.
    #[test]
    fn the_ignore_crate_walks_into_a_nested_repository_on_its_own() {
        let fixture = nested_checkouts();
        for require_git in [false, true] {
            let mut builder = WalkBuilder::new(&fixture.root);
            builder
                .follow_links(false)
                .hidden(false)
                .parents(true)
                .git_ignore(true)
                .git_exclude(false)
                .ignore(false)
                .require_git(require_git)
                .git_global(false);
            let mut files: Vec<String> = builder
                .build()
                .flatten()
                .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
                .filter_map(|entry| {
                    Utf8Path::from_path(entry.path())
                        .and_then(|p| paths::relative_to(&fixture.root, p))
                })
                // `hidden(false)` means the raw crate walks git's own metadata
                // too; it is this module's floor, not the crate's, and it would
                // bury the listing below.
                .filter(|rel| !is_git_metadata(rel))
                .map(|p| p.to_string())
                .collect();
            files.sort();
            assert!(
                files.iter().any(|p| p == "wt/agent/src/main.rs"),
                "require_git={require_git}: the crate stopped at the nested worktree by itself: \
                 {files:#?}"
            );
            assert!(
                files.iter().any(|p| p == "vendor/dep/lib.rs"),
                "require_git={require_git}: the crate stopped at the nested clone by itself: \
                 {files:#?}"
            );
        }
    }

    /// The defect. A worktree of the project inside the project is a
    /// byte-for-byte duplicate of it, and a vendored clone is somebody else's
    /// repository; neither is part of this project's content.
    #[test]
    fn a_nested_repository_is_not_walked_as_part_of_its_parent() {
        let fixture = nested_checkouts();
        assert_eq!(fixture.walk(), ["keep.md", "src/main.rs"]);
    }

    /// The case D-0020 created. With `hidden(false)` nothing prunes a
    /// dot-directory by name any more, so the boundary is the *only* thing
    /// keeping an agent tool's `.claude/worktrees/<task>` — a full second copy
    /// of the project — out of the index. No rules file of any kind is
    /// installed here: that is the point.
    #[test]
    fn a_worktree_under_a_dot_directory_is_excluded_by_the_boundary_alone() {
        let fixture = Fixture::new(&[
            ("src/main.rs", "fn main() {}"),
            (".claude/settings.json", "{}"),
        ]);
        commit_all(&fixture.root);
        git_in(
            &fixture.root,
            &[
                "worktree",
                "add",
                "-q",
                ".claude/worktrees/task",
                "-b",
                "task",
            ],
        );
        assert!(!fixture.data_dir.join(USER_IGNORE_FILE).exists());
        assert!(!fixture.root.join(LORE_IGNORE_FILE).exists());
        assert!(
            fixture
                .root
                .join(".claude/worktrees/task/src/main.rs")
                .is_file()
        );
        assert_eq!(fixture.walk(), [".claude/settings.json", "src/main.rs"]);
    }

    /// The case that must not break: a linked worktree registered *directly* as
    /// its own project (what the benchmark harness does) is a repository root
    /// too, and the rule applies to subdirectories only — a walk that refused
    /// the root it was pointed at would index nothing at all.
    #[test]
    fn a_worktree_registered_as_its_own_project_indexes_normally() {
        let fixture = Fixture::new(&[("src/main.rs", "fn main() {}")]);
        commit_all(&fixture.root);
        let checkout = fixture.root.join("checkout");
        git_in(
            &fixture.root,
            &["worktree", "add", "-q", checkout.as_str(), "-b", "wt"],
        );

        // A `.git` *file*, which is what makes this the case a directory test
        // would have missed.
        let marker = checkout.join(GIT_DIR);
        assert!(marker.is_file(), "expected a gitdir: pointer file");
        assert!(
            std::fs::read_to_string(&marker)
                .unwrap()
                .starts_with("gitdir:"),
            "expected a gitdir: pointer file"
        );
        assert!(is_repo_root(&checkout));
        assert_eq!(fixture.walk_rooted_at(&checkout), ["src/main.rs"]);
    }

    /// The escape hatch, and the reason the boundary is a default rather than a
    /// floor: vendoring a repository and wanting it indexed is legitimate. The
    /// sovereign file, and only it — a machine-wide preference cannot know
    /// which subdirectory of this project is a deliberate vendored copy.
    #[test]
    fn a_loreignore_reinclude_overrides_the_nested_repository_skip() {
        let fixture = nested_checkouts();
        fixture.write(LORE_IGNORE_FILE, "!vendor/dep/\n");
        assert_eq!(
            fixture.walk(),
            [".loreignore", "keep.md", "src/main.rs", "vendor/dep/lib.rs"],
            "the re-include must reach the vendored repo and nothing else"
        );
    }

    /// The incremental path must reach the same verdict as the full scan, or
    /// the watcher indexes a nested repository's files and the next full scan
    /// deletes them, forever. This is the listing `snapshot::observe_paths`
    /// checks a watcher-named file against, so an empty one is the file not
    /// reaching a micro-manifest.
    #[test]
    fn listing_a_directory_inside_a_nested_repository_returns_nothing() {
        let fixture = nested_checkouts();
        assert!(
            fixture
                .walk_from(&fixture.root.join("wt/agent/src"), Some(1))
                .is_empty()
        );
        assert!(
            fixture
                .walk_from(&fixture.root.join("vendor/dep"), Some(1))
                .is_empty()
        );
    }

    // -----------------------------------------------------------------------
    // Links: not followed, but never silent
    //
    // Real links throughout. The whole question is what the *platform* reports
    // for a reparse point, and a fixture that hand-built the shape would be a
    // restatement of this module's assumptions rather than a test of them.
    //
    // The link *construction* is what differs between platforms, so that is the
    // only thing gated: `link_dir`/`link_file` below build the local article and
    // every test that uses one runs everywhere. Gating the tests themselves —
    // which is how they were first written — meant the D-0021 rules were
    // asserted on Windows and nowhere else, so a Linux or macOS regression in
    // the shared `is_symlink` branch above would have shipped unnoticed.
    // -----------------------------------------------------------------------

    /// A directory of content, outside any fixture root, for a link to point at.
    fn elsewhere() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::write(path.join("behind.md"), "content behind the link").unwrap();
        (dir, path)
    }

    /// A link to a directory, in whatever form the platform makes one.
    ///
    /// On Windows that is `mklink /J`, a **junction**, which is what an
    /// assembled workspace actually uses (a directory symlink needs privilege;
    /// a junction does not, so this is the shape real trees have). On POSIX it
    /// is an ordinary symlink, which needs no privilege either. D-0021 gives
    /// the two the identical semantic, so every caller below is one test.
    #[cfg(windows)]
    fn link_dir(link: &Utf8Path, target: &Utf8Path) {
        let out = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J", link.as_str(), target.as_str()])
            .output()
            .expect("mklink is available");
        assert!(out.status.success(), "mklink /J failed: {out:?}");
    }

    #[cfg(unix)]
    fn link_dir(link: &Utf8Path, target: &Utf8Path) {
        std::os::unix::fs::symlink(target, link).expect("a POSIX symlink needs no privilege");
    }

    /// A link to a *file*, or `false` where the platform refused to make one.
    ///
    /// The refusal is Windows-shaped and deliberately not shared: a file
    /// symlink needs `SeCreateSymbolicLinkPrivilege` (developer mode), unlike a
    /// junction, and a test that failed on an ordinary account would be
    /// reporting the account rather than the code. POSIX has no such
    /// privilege, so a failure there is a genuine one and must be loud — a
    /// skip on that side would quietly retire the coverage this helper exists
    /// to extend.
    #[cfg(windows)]
    fn link_file(link: &Utf8Path, target: &Utf8Path) -> bool {
        if std::os::windows::fs::symlink_file(target, link).is_err() {
            eprintln!("skipping: creating a file symlink needs developer mode");
            return false;
        }
        true
    }

    #[cfg(unix)]
    fn link_file(link: &Utf8Path, target: &Utf8Path) -> bool {
        std::os::unix::fs::symlink(target, link).expect("a POSIX symlink needs no privilege");
        true
    }

    /// The defect, exactly: a workspace whose entire corpus is reached through
    /// directory links indexes its loose files and nothing else.
    ///
    /// The concrete case is `Lexomancy-bench` — three junctions over two other
    /// checkouts, 3 loose files, and an index that reported `files: 3` with no
    /// hint that anything had been declined. Editing its `.loreignore` could
    /// not change the outcome, because no ignore rule is ever consulted for a
    /// tree the walk does not enter.
    #[test]
    fn a_directory_link_is_not_followed_but_is_reported() {
        let (_keep, target) = elsewhere();
        let fixture = Fixture::new(&[("loose.md", "# notes")]);
        link_dir(&fixture.root.join("corpus"), &target);

        assert_eq!(fixture.walk(), ["loose.md"], "the link is not followed");
        assert_eq!(fixture.links(), ["corpus"], "and it is not silent either");
    }

    /// The platform detail the report rests on: Windows sets the
    /// reparse-point attribute for a junction with a name-surrogate tag, and
    /// Rust answers `is_symlink()` for it. Pinned because the whole
    /// classification above is one `is_symlink` call — were that ever to
    /// answer `is_dir` instead, junction trees would be walked (and
    /// duplicated) with no other test noticing.
    #[cfg(windows)]
    #[test]
    fn windows_reports_a_junction_as_a_symlink() {
        let (_keep, target) = elsewhere();
        let fixture = Fixture::new(&[]);
        let link = fixture.root.join("corpus");
        link_dir(&link, &target);

        let kind = std::fs::symlink_metadata(&link).unwrap().file_type();
        assert!(kind.is_symlink(), "a junction is a link to std");
        assert!(!kind.is_dir(), "and specifically not a directory");
        // Following it deliberately *does* resolve — the walker's choice not
        // to is policy, not an inability.
        assert!(fixture.root.join("corpus/behind.md").is_file());
    }

    /// The same claim on the other side of the `cfg`, and a separate test
    /// rather than a shared one because the *fact* differs: above, that Windows
    /// answers `is_symlink` for a name-surrogate reparse point; here, that a
    /// POSIX symlink to a directory does not quietly present itself as the
    /// directory. `symlink_metadata` not following the link is the whole reason
    /// the classification holds, and one shared test would have left whichever
    /// platform it was not written on unpinned.
    #[cfg(unix)]
    #[test]
    fn unix_reports_a_symlink_to_a_directory_as_a_symlink() {
        let (_keep, target) = elsewhere();
        let fixture = Fixture::new(&[]);
        let link = fixture.root.join("corpus");
        link_dir(&link, &target);

        let kind = std::fs::symlink_metadata(&link).unwrap().file_type();
        assert!(kind.is_symlink(), "the link itself, unresolved");
        assert!(!kind.is_dir(), "and specifically not a directory");
        // Following it deliberately *does* resolve — the walker's choice not
        // to is policy, not an inability.
        assert!(fixture.root.join("corpus/behind.md").is_file());
    }

    /// An ignore rule still applies to the link itself, so a project that has
    /// already decided a link is noise does not get told about it every pass.
    /// (Which tree the link points at is not something the rules can reach —
    /// that is the part this module cannot fix with a rule.)
    #[test]
    fn an_ignored_link_is_not_reported() {
        let (_keep, target) = elsewhere();
        let fixture = Fixture::new(&[("loose.md", "# notes"), (".loreignore", "corpus\n")]);
        link_dir(&fixture.root.join("corpus"), &target);

        assert_eq!(fixture.links(), Vec::<String>::new());
        assert_eq!(fixture.walk(), [".loreignore", "loose.md"]);
    }

    /// A tree with no links at all reports none — the report is evidence, so
    /// it has to be quiet when there is nothing to say.
    #[test]
    fn a_tree_without_links_reports_none() {
        let fixture = Fixture::new(&[("src/main.rs", "fn main() {}"), ("keep.md", "# notes")]);
        assert_eq!(fixture.links(), Vec::<String>::new());
    }

    /// **The asymmetry, pinned** (D-0021). The nested-repository boundary two
    /// sections up is overridable by a `!` re-include in the sovereign file;
    /// this one is not, and the difference is deliberate rather than an
    /// oversight waiting to be tidied up.
    ///
    /// A `!` on a vendored repository answers *whose content is this* — its
    /// bytes are already under the root and the owner may answer "mine". A `!`
    /// on a link would instead **extend the project's extent to another part
    /// of the filesystem**, which is a different act wearing the same syntax.
    ///
    /// This is also, precisely, what a user tried: `Lexomancy-bench`'s
    /// `.loreignore` carries `!design/`, `!tools/` and `!Lexomancy/` over three
    /// junctions, written in the expectation that they would pull the corpus
    /// in. They are inert, and this test is what keeps them inert.
    #[test]
    fn a_loreignore_reinclude_does_not_rescue_a_link() {
        let (_keep, target) = elsewhere();
        let fixture = Fixture::new(&[
            ("loose.md", "# notes"),
            // Every spelling a hopeful user would reach for.
            (".loreignore", "!corpus\n!corpus/\n!corpus/**\n"),
        ]);
        link_dir(&fixture.root.join("corpus"), &target);

        assert_eq!(
            fixture.walk(),
            [".loreignore", "loose.md"],
            "a re-include must not reach through a link the way it reaches \
             into a vendored repository"
        );
        assert_eq!(fixture.links(), ["corpus"]);
    }

    /// A link to a *file* is not indexed through the link either (D-0021).
    ///
    /// Not a special case of the directory rule, and the reason is storage
    /// rather than traversal: the store is keyed `(project, path)`, so
    /// indexing the target under the link's path would put one content at two
    /// logical paths and the same bytes would compete with themselves in the
    /// rankings.
    ///
    /// Where the privilege to make one is absent, [`link_file`] skips rather
    /// than fails — on Windows only, and see that helper for why.
    #[test]
    fn a_link_to_a_file_is_not_indexed_through_the_link() {
        let fixture = Fixture::new(&[("real.md", "# the genuine article")]);
        let link = fixture.root.join("alias.md");
        if !link_file(&link, &fixture.root.join("real.md")) {
            return;
        }
        assert!(
            link.is_file(),
            "it does resolve; not following it is policy"
        );

        assert_eq!(fixture.walk(), ["real.md"], "indexed once, under one path");
        assert_eq!(fixture.links(), ["alias.md"]);
    }

    /// **Every boundary is relative to the walk root, not to "the project".**
    ///
    /// This is what makes a declared source root (D-0022) work: a mount is
    /// walked as its own root, and everything — the repository boundary, the
    /// `!` escape hatch, the whole ignore stack — is then evaluated against
    /// *it*. Two halves, and only the first was pinned before:
    ///
    /// 1. the walk root is exempt from the repository boundary, so a mount
    ///    that is itself a checkout indexes rather than returning nothing
    ///    (`a_worktree_registered_as_its_own_project_indexes_normally`);
    /// 2. a repository nested *inside* that root is still refused, and the
    ///    hatch consulted is that root's own `.loreignore` — not the one
    ///    belonging to whatever project happened to declare the mount.
    ///
    /// The second half is the one a mount depends on, and it is what "rules
    /// travel down a root and never between roots" means in the walker.
    #[test]
    fn every_boundary_is_relative_to_the_walk_root() {
        // A repository, with a second one vendored inside it, standing in for
        // a mounted tree that is somebody else's checkout.
        let fixture = Fixture::new(&[("keep.md", "# notes")]);
        commit_all(&fixture.root);
        let vendored = fixture.root.join("vendor/dep");
        init_repo(&vendored);
        std::fs::write(vendored.join("lib.rs"), "pub fn dep() {}").unwrap();

        // Walked as its own root: exempt itself, still stopping at the nested
        // repository below it.
        assert_eq!(
            fixture.walk_rooted_at(&fixture.root.clone()),
            ["keep.md"],
            "the root is exempt; the repository inside it is not"
        );

        // And the hatch is *this* root's file. Written here rather than in
        // some declaring project, because that is the file whose author owns
        // this tree.
        fixture.write(LORE_IGNORE_FILE, "!vendor/dep/\n");
        assert_eq!(
            fixture.walk_rooted_at(&fixture.root.clone()),
            [".loreignore", "keep.md", "vendor/dep/lib.rs"],
            "the re-include is read from the walk root, not from anywhere above it"
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

        assert_eq!(
            sorted(walk_files(&root, &root, None, &data_dir, None).files),
            ["keep.rs"]
        );
        assert_eq!(
            walk_files(&root, &data_dir, None, &data_dir, None),
            Walk::default()
        );
    }
}
