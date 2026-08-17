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

mod daemon_support;

use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use daemon_support::link_dir;
use lore::daemon::snapshot::{Snapshot, observe_paths, observe_project};
use lore::daemon::walk::USER_IGNORE_FILE;
use lore::sources::Sources;
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
            // Canonical, exactly as `lore add` stores it — the source table
            // compares declared paths against this.
            root: lore::daemon::paths::canonicalize_root(dir.path()).unwrap(),
            data_dir: lore::daemon::paths::canonicalize_root(data.path()).unwrap(),
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

    /// Read from the fixture's own `.lore.toml`, so a test that declares
    /// `[[sources]]` gets them and one that does not gets the project root —
    /// exactly what the daemon does at the top of every pass.
    fn sources(&self) -> Sources {
        Sources::load(&self.root)
    }

    fn observe(&self) -> Snapshot {
        observe_project(&self.sources(), &self.data_dir, &CancellationToken::new())
    }

    /// The watcher's form, over the paths a debounced batch would name.
    fn observe_batch(&self, named: &[&str]) -> Snapshot {
        let requested: BTreeSet<Utf8PathBuf> = named.iter().map(Utf8PathBuf::from).collect();
        observe_paths(
            &self.sources(),
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

/// The nested-repository boundary reaches the micro-manifest too. A watcher
/// event names a path directly, so if only the full scan stopped at the
/// boundary a single edit inside a worktree would index the whole file — and
/// the next full scan would delete it, forever.
///
/// The `gitdir:` pointer is written by hand, keeping this file's no-git-binary
/// property (the same way `git_metadata_never_reaches_a_manifest` writes
/// `.git/config`). That the pointer file is genuinely what `git worktree add`
/// puts on disk is established with a real checkout in `daemon::walk`'s unit
/// tests; what is under test here is only that the observer routes both of its
/// forms through that one evaluator.
#[test]
fn a_watcher_event_inside_a_nested_repository_reaches_no_manifest() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        ("wt/agent/src/main.rs", "fn main() {}"),
        ("wt/agent/.git", "gitdir: ../../.git/worktrees/agent\n"),
        ("vendor/dep/.git/HEAD", "ref: refs/heads/main\n"),
        ("vendor/dep/lib.rs", "pub fn dep() {}"),
    ]);
    project.user_rules(".*\n");
    assert_eq!(listed(&project.observe()), ["src/main.rs"]);

    // The file event, and the directory event a dropped-in tree produces.
    let batch = project.observe_batch(&["wt/agent/src/main.rs", "vendor/dep"]);
    assert!(listed(&batch).is_empty(), "{:?}", listed(&batch));

    // And the escape hatch holds on this path as well, or the two would
    // disagree about a deliberately vendored repository.
    project.write(".loreignore", "!vendor/dep/\n");
    assert_eq!(
        listed(&project.observe_batch(&["vendor/dep/lib.rs"])),
        ["vendor/dep/lib.rs"]
    );
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
/// whole. **Chosen, not overlooked** (D-0020): lore ships no ignore rules
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

// ---------------------------------------------------------------------------
// Links the observer declined to follow
// ---------------------------------------------------------------------------

/// A link is not followed, and the snapshot says so — in *both* observation
/// shapes, because a watcher batch that named the link has to reach the same
/// verdict the full scan does.
///
/// The shape that motivated this is a directory **junction**
/// (`Lexomancy-bench`: three junctions over other checkouts, three loose files,
/// and a `files: 3` index with nothing saying why), so the fixture builds one
/// on Windows and a plain symlink elsewhere — see [`daemon_support::link_dir`].
/// D-0021 gives the two the identical semantic, and the assertions below are
/// the semantic, so there is one test rather than a Windows one and a hole.
#[test]
fn a_linked_corpus_is_absent_from_the_manifest_and_named_in_the_snapshot() {
    let corpus = tempfile::tempdir().unwrap();
    let corpus = Utf8Path::from_path(corpus.path()).unwrap();
    std::fs::write(corpus.join("behind.md"), "# the actual content").unwrap();

    let project = Project::new(&[("loose.md", "# the only real file")]);
    link_dir(&project.root.join("corpus"), corpus);
    // The content really is reachable through the link; not walking it is a
    // choice, and the test would be vacuous if the fixture were broken.
    assert!(project.root.join("corpus/behind.md").is_file());

    let full = project.observe();
    assert_eq!(listed(&full), ["loose.md"]);
    assert_eq!(
        full.links.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
        ["corpus"],
        "the manifest omits it, so something else has to account for it"
    );

    // The watcher's shape, over the link itself and a path inside it.
    let batch = project.observe_batch(&["corpus", "corpus/behind.md"]);
    assert!(listed(&batch).is_empty(), "{:?}", listed(&batch));
}

/// The report is evidence, so it stays quiet when there is nothing to report.
#[test]
fn a_project_without_links_reports_none() {
    let project = Project::new(&[("src/main.rs", "fn main() {}")]);
    assert!(project.observe().links.is_empty());
}

// ---------------------------------------------------------------------------
// Declared extent: [[sources]] (D-0021's replacement for following links)
// ---------------------------------------------------------------------------

/// A mounted tree is observed, and every path it contributes carries the
/// mount — which is what keeps one file at one logical address in a store
/// keyed `(project, path)`.
///
/// The shape is the one that started all of this: a project whose real corpus
/// lives in another directory entirely. Under D-0021 a junction to it stays
/// unwalked; declaring it is how you say you meant it.
#[test]
fn a_declared_mount_is_observed_under_its_prefix() {
    let outside = tempfile::tempdir().unwrap();
    let outside = Utf8PathBuf::from_path_buf(outside.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(outside.join("render")).unwrap();
    std::fs::write(outside.join("render/pass.rs"), "pub fn pass() {}").unwrap();
    std::fs::write(outside.join("lib.rs"), "pub mod render;").unwrap();

    let project = Project::new(&[("src/main.rs", "fn main() {}")]);
    project.user_rules(
        ".*
",
    );
    project.write(
        ".lore.toml",
        &format!(
            "[[sources]]
path = \".\"

[[sources]]
path = \"{}\"
mount = \"engine\"
",
            declared_path(&project, &outside)
        ),
    );

    assert_eq!(
        listed(&project.observe()),
        ["engine/lib.rs", "engine/render/pass.rs", "src/main.rs"],
        "both roots contribute, and only the mount is prefixed"
    );
}

/// Rules travel down a root and never between roots.
///
/// The mount's own `.loreignore` governs the mount; the project's governs the
/// project; neither reaches the other. A mounted tree is somebody else's
/// directory that this project happens to name, and reaching into it would be
/// crossing a boundary the declaring project does not own.
#[test]
fn ignore_rules_do_not_cross_between_source_roots() {
    let outside = tempfile::tempdir().unwrap();
    let outside = Utf8PathBuf::from_path_buf(outside.path().to_path_buf()).unwrap();
    std::fs::write(outside.join("keep.rs"), "pub fn keep() {}").unwrap();
    std::fs::write(outside.join("drop.rs"), "pub fn drop_me() {}").unwrap();
    // The mount excludes its own file, and says nothing about the project's.
    std::fs::write(
        outside.join(".loreignore"),
        "drop.rs
",
    )
    .unwrap();

    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        ("src/notes.rs", "// notes"),
        // The project excludes one of its own, and names a path that exists
        // only inside the mount. That line must not reach across.
        (
            ".loreignore",
            "notes.rs
keep.rs
",
        ),
    ]);
    project.user_rules(
        ".*
",
    );
    project.write(
        ".lore.toml",
        &format!(
            "[[sources]]
path = \".\"

[[sources]]
path = \"{}\"
mount = \"engine\"
",
            declared_path(&project, &outside)
        ),
    );

    assert_eq!(
        listed(&project.observe()),
        ["engine/keep.rs", "src/main.rs"],
        "each root applied its own rules and neither applied the other's"
    );
}

/// The watcher's shape has to agree with the full scan across a mount too, or
/// a batch would index what the next full scan deletes.
#[test]
fn a_watcher_batch_reaches_the_same_verdict_inside_a_mount() {
    let outside = tempfile::tempdir().unwrap();
    let outside = Utf8PathBuf::from_path_buf(outside.path().to_path_buf()).unwrap();
    std::fs::write(outside.join("keep.rs"), "pub fn keep() {}").unwrap();
    std::fs::write(outside.join("drop.rs"), "pub fn drop_me() {}").unwrap();
    std::fs::write(
        outside.join(".loreignore"),
        "drop.rs
",
    )
    .unwrap();

    let project = Project::new(&[("src/main.rs", "fn main() {}")]);
    project.user_rules(
        ".*
",
    );
    project.write(
        ".lore.toml",
        &format!(
            "[[sources]]
path = \".\"

[[sources]]
path = \"{}\"
mount = \"engine\"
",
            declared_path(&project, &outside)
        ),
    );

    let batch = project.observe_batch(&["engine/keep.rs", "engine/drop.rs"]);
    assert_eq!(listed(&batch), ["engine/keep.rs"]);
    // Both were observed, so the excluded one is in scope and absent — which
    // is how it leaves the index rather than lingering as an orphan.
    assert!(batch.covers(Utf8Path::new("engine/drop.rs")));
}

/// A path under a mount that `.lore.toml` no longer declares resolves to
/// nothing, so it is absent from the manifest and — being in scope — deleted.
/// Removing a mount has to retract its content, not strand it.
#[test]
fn a_path_under_an_undeclared_mount_reaches_no_manifest() {
    let project = Project::new(&[("src/main.rs", "fn main() {}")]);
    project.user_rules(
        ".*
",
    );

    let batch = project.observe_batch(&["engine/gone.rs"]);
    assert!(listed(&batch).is_empty());
    assert!(batch.covers(Utf8Path::new("engine/gone.rs")));
}

/// The mount path as a committed `.lore.toml` would spell it: **relative to
/// the project root**, which is the only form `[[sources]]` accepts — an
/// absolute path works on exactly one machine and this file travels.
///
/// Both directories come from `tempfile::tempdir()`, so they are siblings and
/// `../<name>` is the honest spelling rather than a contrivance.
fn declared_path(project: &Project, outside: &Utf8Path) -> String {
    assert_eq!(
        outside.parent(),
        project.root.parent(),
        "the fixture assumes both temp dirs share a parent"
    );
    format!("../{}", outside.file_name().expect("a temp dir has a name"))
}
