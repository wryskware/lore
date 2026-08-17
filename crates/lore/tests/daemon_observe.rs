//! What the observer decides to send (D-0015 pusher-side, D-0020 layering).
//!
//! These drive [`observe_project`] and [`observe_paths`] directly rather than a
//! full index pass, because the property under test *is* the manifest: what a
//! pusher would put on the wire, before anything indexes, chunks or deletes.
//! The unit tests in `daemon::walk` cover the evaluator itself; these cover the
//! seam — that the full scan and the watcher's micro-manifest reach the *same*
//! verdict, through the one evaluator, with all three rule sources in play.
//!
//! **No git binary anywhere in this file.** D-0020 retired the git-aware basis:
//! `.gitignore` is honoured as text through the same evaluator, so a work tree
//! is not a precondition for anything here and these tests say the same thing on
//! a machine with no git installed.

use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use lore::daemon::snapshot::{Snapshot, observe_paths, observe_project};
use lore::daemon::walk::USER_IGNORE_FILE;
use tokio_util::sync::CancellationToken;

/// A project root, plus the daemon's data directory — which is both outside the
/// project and where the user-level rules live.
struct Project {
    _dir: tempfile::TempDir,
    _data: tempfile::TempDir,
    root: Utf8PathBuf,
    data_dir: Utf8PathBuf,
}

impl Project {
    fn new(spec: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = Self {
            root: Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap(),
            data_dir: Utf8PathBuf::from_path_buf(data.path().to_path_buf()).unwrap(),
            _dir: dir,
            _data: data,
        };
        for (path, contents) in spec {
            project.write(path, contents);
        }
        project
    }

    fn write(&self, path: &str, contents: &str) {
        let abs = self.root.join(path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, contents).unwrap();
    }

    /// The lowest rule source: what this machine excludes from every project.
    ///
    /// Every fixture here installs `.*` at minimum, because that is what the
    /// shipped template starts with and it keeps the ignore files themselves out
    /// of the manifests below.
    fn user_rules(&self, rules: &str) -> &Self {
        std::fs::write(self.data_dir.join(USER_IGNORE_FILE), rules).unwrap();
        self
    }

    fn observe(&self) -> Snapshot {
        observe_project(&self.root, &self.data_dir, &CancellationToken::new())
    }

    /// The watcher's form, over the paths a debounced batch would name.
    fn observe_batch(&self, named: &[&str]) -> Snapshot {
        let requested: BTreeSet<Utf8PathBuf> = named.iter().map(Utf8PathBuf::from).collect();
        observe_paths(
            &self.root,
            &self.data_dir,
            &requested,
            &CancellationToken::new(),
        )
    }
}

fn listed(snapshot: &Snapshot) -> Vec<String> {
    snapshot
        .manifest
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// The stack, through the observer (D-0020)
// ---------------------------------------------------------------------------

/// The amendment in one manifest: an untracked file is observed like any other
/// (no `git add`, no work tree, no subprocess), and a file `.gitignore` names is
/// not.
#[test]
fn a_manifest_is_everything_the_rules_admit_tracked_or_not() {
    let project = Project::new(&[
        (".gitignore", "*.log\nsecrets/\n"),
        ("src/committed.rs", "fn committed() {}"),
        ("src/brand_new.rs", "fn brand_new() {}"),
        ("build.log", "noise"),
        ("secrets/token.txt", "hunter2"),
    ]);
    project.user_rules(".*\n");
    assert_eq!(
        listed(&project.observe()),
        ["src/brand_new.rs", "src/committed.rs"]
    );
}

/// The re-include that the retired git basis used to intersect away. D-0020
/// makes `.loreignore` sovereign, so it now stands — this is the behaviour
/// change, asserted deliberately.
#[test]
fn a_loreignore_reinclusion_of_a_gitignored_path_now_stands() {
    let project = Project::new(&[
        (".gitignore", "data/\n"),
        (".loreignore", "!data/\n!data/notes.md\n"),
        ("data/notes.md", "# notes"),
        ("src/main.rs", "fn main() {}"),
    ]);
    project.user_rules(".*\n");
    assert_eq!(
        listed(&project.observe()),
        ["data/notes.md", "src/main.rs"],
        "the sovereign file wins, whether or not the repo is a work tree"
    );
}

/// A credential is excluded by the *user-level* rules and re-includable by the
/// project — the accepted trade in D-0020, through the whole observer rather
/// than only the walker.
///
/// The trade, stated: a bad ignore file can admit a secret. Hygiene here is
/// best-effort user responsibility, and an encrypted store is the substantive
/// data-protection measure.
#[test]
fn a_credential_is_excluded_by_default_and_re_includable_by_the_project() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        (".env", "API_TOKEN=hunter2"),
        ("certs/server.pem", "-----BEGIN PRIVATE KEY-----"),
    ]);
    project.user_rules(".*\n*.pem\n");
    assert_eq!(listed(&project.observe()), ["src/main.rs"]);

    // And through the watcher, where a file is named directly rather than
    // discovered by a walk.
    let batch = project.observe_batch(&[".env", "certs/server.pem"]);
    assert!(listed(&batch).is_empty(), "{:?}", listed(&batch));

    // Now the project overrules the machine, for exactly one of them.
    project.write(".loreignore", "!certs/server.pem\n");
    assert_eq!(
        listed(&project.observe()),
        ["certs/server.pem", "src/main.rs"]
    );
    assert_eq!(
        listed(&project.observe_batch(&[".env", "certs/server.pem"])),
        ["certs/server.pem"],
        "the watcher has to agree, or the two would fight over the file forever"
    );
}

/// The watcher's micro-manifest applies the same rules. Absence inside its
/// scope is deletion, so a batch that admitted a gitignored file would index
/// something the next full scan deletes — forever.
#[test]
fn a_watcher_batch_applies_the_same_rules_as_a_full_scan() {
    let project = Project::new(&[
        (".gitignore", "*.log\n"),
        ("src/tracked.rs", "fn tracked() {}"),
        ("src/fresh.rs", "fn fresh() {}"),
        ("run.log", "noise"),
    ]);
    project.user_rules(".*\n");

    let batch = project.observe_batch(&["src/tracked.rs", "src/fresh.rs", "run.log"]);
    assert_eq!(listed(&batch), ["src/fresh.rs", "src/tracked.rs"]);
    // All three were *observed* — `run.log` is in scope and absent, which is
    // how a newly-gitignored file leaves the index.
    assert!(batch.covers(Utf8Path::new("run.log")));
}

/// A project's own exclusion is not overridden by anything below it, which is
/// the other half of sovereignty: silence inherits, but a rule decides.
#[test]
fn a_project_exclusion_holds_against_the_lower_sources() {
    let project = Project::new(&[
        (".loreignore", "generated/\n"),
        (".gitignore", "!generated/\n"),
        ("generated/big.rs", "// machine written"),
        ("src/main.rs", "fn main() {}"),
    ]);
    project.user_rules(".*\n");
    assert_eq!(listed(&project.observe()), ["src/main.rs"]);
}

/// `.git` answers to nobody, in the manifest a pusher would send — it holds the
/// remote's credentials and it is the mechanism `.gitignore` is read from.
#[test]
fn git_metadata_never_reaches_a_manifest() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        (".git/config", "[core]\n"),
        (".loreignore", "!.git/\n!.git/*\n"),
    ]);
    project.user_rules(".*\n");
    assert_eq!(listed(&project.observe()), ["src/main.rs"]);

    // Named directly by a watcher event, where no walk pruned anything.
    let batch = project.observe_batch(&[".git/config", ".git"]);
    assert!(listed(&batch).is_empty(), "{:?}", listed(&batch));
}

/// With nothing installed and nothing committed, the observer sends the project
/// whole. **Chosen, not overlooked** (D-0020/D-0021): lore ships no ignore rules
/// of its own, so out of the box a manifest is the repo minus `.git`.
#[test]
fn with_no_rules_anywhere_the_manifest_is_the_whole_project() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        ("target/debug/build.rs", "generated"),
        (".env", "API_TOKEN=hunter2"),
        (".git/config", "[core]\n"),
    ]);
    assert!(!project.data_dir.join(USER_IGNORE_FILE).exists());
    assert_eq!(
        listed(&project.observe()),
        [".env", "src/main.rs", "target/debug/build.rs"]
    );
}
