//! The git-aware manifest basis (D-0017).
//!
//! For a project inside a git work tree, what may enter a manifest is
//! **tracked files plus untracked files not excluded by gitignore** — the
//! `git ls-files --cached --others --exclude-standard` set — intersected with
//! [`super::walk`]'s own rules. Non-git projects are untouched: the walker's
//! verdict is the whole basis, exactly as before.
//!
//! D-0017 amends D-0015's tracked-only clause. Tracked-only made a brand-new
//! file invisible until `git add`, which is a freshness regression in precisely
//! the agent workflow Lore serves. The union restores that while keeping the
//! leak protection that mattered: `.gitignore` is where secrets actually live,
//! and a gitignored `.env` still never reaches a manifest.
//!
//! # Why the git binary, and not the `ignore` crate
//!
//! The walker already evaluates `.gitignore` through the `ignore` crate, so
//! reusing it looks free. It cannot express this basis:
//!
//! - **The tracked half has no gitignore expression at all.** `--cached` lists
//!   a file *because git tracks it*, gitignored or not (`git add -f`), and
//!   there is no index for a gitignore evaluator to consult. This is not
//!   academic: a repository whose `.loreignore` re-includes a gitignored
//!   directory (which the walker honors — `.loreignore` outranks `.gitignore`)
//!   gets exactly two kinds of file in it, and only git's index separates the
//!   deliberately-versioned one from the scratch file beside it.
//! - **`--exclude-standard` includes the user's global excludes**
//!   (`core.excludesFile`), which [`super::walk`] deliberately does *not*
//!   consult (`git_global(false)`) so that what Lore indexes cannot depend on
//!   unrelated machine state. Under a git basis that dependency is git's
//!   answer, faithfully, rather than a second opinion about it.
//! - **`hidden(true)`.** The walker skips every dot-prefixed path; git does
//!   not. Because this module *intersects* rather than replaces, that stays
//!   true — `.github/workflows/` is still unindexed — but it means the two
//!   rule sets were never going to agree, and pretending otherwise would put
//!   the disagreement somewhere nobody could see it.
//!
//! `.git/info/exclude` is the one input both agree on.
//!
//! # No hard dependency on git
//!
//! Everything here degrades. A root with no `.git` anywhere above it never
//! spawns a process. A root that *is* in a work tree but whose git is missing,
//! broken, or refuses to answer falls back to the walker's own verdict — the
//! pre-D-0017 behavior, which is a superset and therefore never deletes
//! anything — and the reason travels back on the [`Basis`] so `lore status`
//! reports a degraded project rather than a pass silently changing meaning.

use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};

use super::walk::is_repo_root;

/// Windows `CREATE_NO_WINDOW`: a daemon that flashes a console window on every
/// index pass is a daemon nobody leaves running.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What git says a project's manifest may be drawn from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    /// Not in a git work tree. The walker's rules are the whole basis and
    /// [`Basis::admits`] refuses nothing (D-0017: non-git projects unchanged).
    Walker,
    /// git answered. Only these project-relative paths may enter a manifest —
    /// still intersected with the walker, never replacing it.
    Git(BTreeSet<Utf8PathBuf>),
    /// A work tree whose git could not answer, with the reason. The walker
    /// stands in, which is the pre-D-0017 basis and a superset of this one, so
    /// a degraded pass over-includes rather than deleting anything.
    Unavailable(String),
}

impl Basis {
    /// Whether the git basis admits `rel`. Always true when there is no answer
    /// to apply: an absent basis must not subtract from the walker's.
    pub fn admits(&self, rel: &Utf8Path) -> bool {
        match self {
            Basis::Walker | Basis::Unavailable(_) => true,
            Basis::Git(paths) => paths.contains(rel),
        }
    }

    /// The degradation to report in `lore status`, if any.
    pub fn unavailable(&self) -> Option<&str> {
        match self {
            Basis::Unavailable(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Is `root` inside a git work tree?
///
/// Walks up rather than testing `root` alone: registering a subdirectory of a
/// repository as a project is legitimate, and such a root has no `.git` of its
/// own while still being governed by one. A few `exists()` calls, and a
/// non-git project never pays for a process spawn.
///
/// "Is this directory a repository root" is [`super::walk::is_repo_root`], the
/// same question the walker asks of every subdirectory it descends into. One
/// definition, because two would eventually disagree about a linked worktree.
pub fn in_work_tree(root: &Utf8Path) -> bool {
    root.ancestors().any(is_repo_root)
}

/// The whole basis for a full observation: one `git ls-files`.
///
/// Run from `root` rather than the repository top level, because git reports
/// paths relative to its working directory — which is exactly the
/// project-relative form a manifest uses, including when the registered root is
/// a subdirectory of the repository.
pub fn observe(root: &Utf8Path) -> Basis {
    if !in_work_tree(root) {
        return Basis::Walker;
    }
    let mut command = git(root);
    command.args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ]);
    match run(command, "ls-files", &[]) {
        Ok(stdout) => Basis::Git(parse_nul_paths(&stdout)),
        Err(reason) => degraded(root, "ls-files", reason),
    }
}

/// The same question for the handful of paths a watcher batch names, without
/// listing the repository.
///
/// One `git check-ignore -z --stdin` per batch: one process, one index read,
/// N paths down a pipe. At the ~20–30s debounce D-0015 settled on that is
/// noise next to the hashing the same batch already does, and it is the only
/// option of the three that stays *correct*. Re-running `ls-files` would list
/// a 300k-file repository to ask about four paths; caching the last full
/// scan's set would answer "not in the basis" for a file created since, which
/// is the exact freshness regression D-0017 exists to repeal.
///
/// `check-ignore` is index-aware by default (that is what its `--no-index`
/// flag turns *off*), so it agrees with `--cached ∪ --others
/// --exclude-standard` path by path: a tracked-but-gitignored file is reported
/// as not ignored, and therefore admitted, exactly as `--cached` admits it.
pub fn observe_paths(root: &Utf8Path, candidates: &BTreeSet<Utf8PathBuf>) -> Basis {
    if candidates.is_empty() || !in_work_tree(root) {
        return Basis::Walker;
    }
    let mut stdin = Vec::new();
    for path in candidates {
        stdin.extend_from_slice(path.as_str().as_bytes());
        stdin.push(0);
    }
    let mut command = git(root);
    command.args(["check-ignore", "-z", "--stdin"]);
    match run(command, "check-ignore", &stdin) {
        Ok(stdout) => {
            let ignored = parse_nul_paths(&stdout);
            Basis::Git(candidates.difference(&ignored).cloned().collect())
        }
        Err(reason) => degraded(root, "check-ignore", reason),
    }
}

/// A work tree whose git would not answer: log once, and carry the reason to
/// `status` rather than failing the pass.
fn degraded(root: &Utf8Path, subcommand: &str, reason: String) -> Basis {
    tracing::warn!(
        root = %root,
        subcommand,
        reason = %reason,
        "git could not supply the manifest basis (D-0017); falling back to the walker's own \
         rules for this pass, which over-includes rather than deleting anything"
    );
    Basis::Unavailable(reason)
}

/// A `git` invocation shaped for a background daemon.
fn git(root: &Utf8Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        // Never block a pass on a human. A credential helper or a `safe.
        // directory` prompt would otherwise hang an index pass forever, and
        // there is nobody attached to answer it.
        .env("GIT_TERMINAL_PROMPT", "0")
        // Do not take the index lock. Both subcommands here are read-only in
        // intent, and a daemon that fights the developer's own `git` for a
        // lock every twenty seconds is a daemon that gets uninstalled.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Run one git subcommand and return its stdout, or a human-readable reason it
/// could not be trusted.
///
/// stdin is written from its own thread. The batches this module sends are
/// small enough that a pipe buffer would swallow them whole, but "small
/// enough" is a property of the caller and deadlocking an index pass is not a
/// failure mode worth leaving reachable.
fn run(mut command: Command, subcommand: &str, stdin: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = command.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => {
            "`git` is not on this daemon's PATH; install it or put it there to get the \
             git-aware manifest basis"
                .to_string()
        }
        _ => format!("could not run `git {subcommand}`: {err}"),
    })?;

    let mut pipe = child.stdin.take();
    let writer = std::thread::spawn({
        let stdin = stdin.to_vec();
        move || {
            if let Some(pipe) = pipe.as_mut() {
                // A subcommand that stops reading (it failed, it saw enough)
                // closes the pipe; that is its answer, not an error of ours.
                let _ = pipe.write_all(&stdin);
            }
            drop(pipe);
        }
    });

    let output = child
        .wait_with_output()
        .map_err(|err| format!("`git {subcommand}` could not be collected: {err}"))?;
    let _ = writer.join();

    // `check-ignore` answers "nothing here is ignored" with exit 1, which is a
    // successful answer and not a failure; 128 and friends are the failures.
    let ok = output.status.success()
        || (subcommand == "check-ignore" && output.status.code() == Some(1));
    if !ok {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr
            .trim()
            .lines()
            .next()
            .unwrap_or("no output")
            .to_string();
        return Err(format!(
            "`git {subcommand}` exited {}: {detail}",
            output
                .status
                .code()
                .map_or_else(|| "abnormally".to_string(), |code| code.to_string())
        ));
    }
    Ok(output.stdout)
}

/// Split `-z` output into project-relative paths.
///
/// `-z` is what makes this safe: without it git quotes unusual names, and a
/// quoted path silently does not match the walker's. A path that is not UTF-8
/// is dropped, because the walker drops it too — chunk ids are derived from the
/// path string.
fn parse_nul_paths(stdout: &[u8]) -> BTreeSet<Utf8PathBuf> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok())
        .map(Utf8PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository whose basis is entirely its own: the developer's global
    /// excludes are neutralized so this test says the same thing on every
    /// machine, which is the only way a git-shaped test can be deterministic.
    fn repo(spec: &[(&str, &str)]) -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        for (path, contents) in spec {
            let abs = root.join(path);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(abs, contents).unwrap();
        }
        run_git(&root, &["init", "-q", "."]);
        // Inside `.git`, which `ls-files` never reports, so neutralizing the
        // machine's global excludes does not itself become a fixture file.
        let empty = root.join(".git").join("lore-empty-excludes");
        std::fs::write(&empty, "").unwrap();
        run_git(
            &root,
            &["config", "--local", "core.excludesFile", empty.as_str()],
        );
        run_git(
            &root,
            &["config", "--local", "user.email", "test@example.invalid"],
        );
        run_git(&root, &["config", "--local", "user.name", "test"]);
        (dir, root)
    }

    fn run_git(root: &Utf8Path, args: &[&str]) {
        let output = git(root).args(args).output().expect("git is on PATH");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn admitted(basis: &Basis) -> Vec<String> {
        match basis {
            Basis::Git(paths) => {
                let mut out: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
                out.sort();
                out
            }
            other => panic!("expected a git basis, got {other:?}"),
        }
    }

    /// The three cases D-0017 turns on, in one repository: a tracked file is
    /// always in, an untracked-un-gitignored file is in *without* `git add`
    /// (the whole point of the amendment), and a gitignored file is out.
    #[test]
    fn the_basis_is_tracked_plus_untracked_minus_gitignored() {
        let (_dir, root) = repo(&[
            (".gitignore", "ignored.log\nsecrets/\n"),
            ("src/tracked.rs", "fn tracked() {}"),
            ("src/fresh.rs", "fn fresh() {}"),
            ("ignored.log", "noise"),
            ("secrets/.env", "TOKEN=1"),
        ]);
        run_git(&root, &["add", ".gitignore", "src/tracked.rs"]);

        let basis = observe(&root);
        assert_eq!(
            admitted(&basis),
            [".gitignore", "src/fresh.rs", "src/tracked.rs"]
        );
        assert!(basis.admits(Utf8Path::new("src/fresh.rs")), "no `git add`");
        assert!(!basis.admits(Utf8Path::new("ignored.log")));
        assert!(!basis.admits(Utf8Path::new("secrets/.env")));
    }

    /// The half no gitignore evaluation can reach: `git add -f` makes a
    /// gitignored file tracked, and a tracked file is in the basis. What that
    /// is worth in a real manifest is
    /// `daemon_observe::the_tracked_half_of_the_basis_separates_two_files_one_rule_admits`,
    /// where lore's own rules admit the file and only git's index can tell it
    /// from its untracked neighbour.
    #[test]
    fn a_tracked_file_stays_in_the_basis_even_when_gitignore_names_it() {
        let (_dir, root) = repo(&[(".gitignore", "build.log\n"), ("build.log", "kept")]);
        run_git(&root, &["add", "-f", "build.log"]);
        assert!(observe(&root).admits(Utf8Path::new("build.log")));
        // …and the per-path form the watcher uses has to agree, or the two
        // paths would fight over the same file forever.
        let batch = BTreeSet::from([Utf8PathBuf::from("build.log")]);
        assert!(observe_paths(&root, &batch).admits(Utf8Path::new("build.log")));
    }

    /// The watcher's form, on the cases it actually sees: a file created since
    /// the last full scan, and one the developer just gitignored.
    #[test]
    fn the_per_path_form_agrees_with_the_full_listing() {
        let (_dir, root) = repo(&[
            (".gitignore", "ignored.log\n"),
            ("src/tracked.rs", "fn tracked() {}"),
            ("ignored.log", "noise"),
        ]);
        run_git(&root, &["add", ".gitignore", "src/tracked.rs"]);
        std::fs::write(root.join("src/brand_new.rs"), "fn new() {}").unwrap();

        let batch = BTreeSet::from([
            Utf8PathBuf::from("src/tracked.rs"),
            Utf8PathBuf::from("src/brand_new.rs"),
            Utf8PathBuf::from("ignored.log"),
        ]);
        assert_eq!(
            admitted(&observe_paths(&root, &batch)),
            ["src/brand_new.rs", "src/tracked.rs"]
        );
    }

    /// A batch where git finds nothing to ignore exits 1, which is an answer.
    /// Reading it as a failure would degrade every ordinary watcher batch.
    #[test]
    fn a_batch_with_nothing_ignored_is_an_answer_not_a_failure() {
        let (_dir, root) = repo(&[("src/main.rs", "fn main() {}")]);
        let batch = BTreeSet::from([Utf8PathBuf::from("src/main.rs")]);
        let basis = observe_paths(&root, &batch);
        assert_eq!(basis.unavailable(), None);
        assert_eq!(admitted(&basis), ["src/main.rs"]);
    }

    /// A project outside any work tree never spawns git and never subtracts.
    #[test]
    fn a_non_git_project_keeps_the_walkers_basis() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        // A tempdir is not inside a repository, so nothing above it counts
        // either.
        assert!(!in_work_tree(&root));
        assert_eq!(observe(&root), Basis::Walker);
        assert!(observe(&root).admits(Utf8Path::new("anything/at/all.rs")));
    }

    /// A subdirectory registered as its own project is still governed by the
    /// repository above it, and git reports paths relative to it.
    #[test]
    fn a_subdirectory_project_gets_paths_relative_to_itself() {
        let (_dir, root) = repo(&[
            (".gitignore", "docs/draft.md\n"),
            ("docs/published.md", "# published"),
            ("docs/draft.md", "# draft"),
            ("src/main.rs", "fn main() {}"),
        ]);
        let docs = root.join("docs");
        assert!(in_work_tree(&docs));
        assert_eq!(admitted(&observe(&docs)), ["published.md"]);
    }
}
