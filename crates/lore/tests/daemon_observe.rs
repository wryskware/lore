//! What the observer decides to send (D-0015 pusher-side, D-0017 basis).
//!
//! These drive [`observe_project`] and [`observe_paths`] directly rather than a
//! full index pass, because the property under test *is* the manifest: what a
//! pusher would put on the wire, before anything indexes, chunks or deletes.
//! The unit tests in `daemon::basis` and `daemon::walk` cover the two
//! mechanisms in isolation; these cover the layering — which rule wins when
//! `.loreignore`, `.gitignore`, git's index and `.lore.toml` all have an
//! opinion about the same file.

use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use lore::daemon::snapshot::{Snapshot, observe_paths, observe_project};
use tokio_util::sync::CancellationToken;

/// A project root, with the daemon's data directory far away from it.
struct Project {
    _dir: tempfile::TempDir,
    root: Utf8PathBuf,
}

impl Project {
    fn new(spec: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let project = Self { _dir: dir, root };
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

    /// Turn the root into a git work tree whose basis is entirely its own —
    /// the machine's global excludes are neutralized, so this says the same
    /// thing on every machine.
    fn git_init(&self) -> &Self {
        self.git(&["init", "-q", "."]);
        // Inside `.git`, which `ls-files` never reports.
        let empty = self.root.join(".git").join("lore-empty-excludes");
        std::fs::write(&empty, "").unwrap();
        self.git(&["config", "--local", "core.excludesFile", empty.as_str()]);
        self
    }

    fn git(&self, args: &[&str]) -> &Self {
        let output = std::process::Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .expect("git is on PATH");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self
    }

    /// The daemon data dir, deliberately outside the project.
    fn data_dir(&self) -> Utf8PathBuf {
        Utf8PathBuf::from("Z:/nowhere/lore-data")
    }

    fn observe(&self) -> Snapshot {
        observe_project(&self.root, &self.data_dir(), &CancellationToken::new())
    }

    /// The watcher's form, over the paths a debounced batch would name.
    fn observe_batch(&self, named: &[&str]) -> Snapshot {
        let requested: BTreeSet<Utf8PathBuf> = named.iter().map(Utf8PathBuf::from).collect();
        observe_paths(
            &self.root,
            &self.data_dir(),
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
// The git-aware basis (D-0017)
// ---------------------------------------------------------------------------

/// The amendment in one manifest. A tracked file is in; an untracked file that
/// gitignore does not name is in **without `git add`**, which is the entire
/// reason D-0017 exists; a gitignored file is out.
#[test]
fn a_git_projects_manifest_is_tracked_plus_untracked_minus_gitignored() {
    let project = Project::new(&[
        (".gitignore", "*.log\nsecrets/\n"),
        ("src/tracked.rs", "fn tracked() {}"),
        ("build.log", "noise"),
        ("secrets/token.txt", "hunter2"),
    ]);
    project.git_init().git(&["add", "src/tracked.rs"]);
    // Created after the `git add`, and never added: the file an agent just
    // wrote, which tracked-only would have hidden from the index.
    project.write("src/brand_new.rs", "fn brand_new() {}");

    let snapshot = project.observe();
    assert_eq!(listed(&snapshot), ["src/brand_new.rs", "src/tracked.rs"]);
    assert_eq!(snapshot.basis_error, None, "git answered");
}

/// A file `.gitignore` names is out of the basis even when `.loreignore`
/// explicitly re-includes it. The re-include is not being ignored — it is being
/// *intersected*, which is what D-0017 says the basis is.
#[test]
fn the_git_basis_intersects_a_loreignore_reinclusion_away() {
    let project = Project::new(&[
        (".gitignore", "data/\n"),
        (".loreignore", "!data/\n!data/notes.md\n"),
        ("data/notes.md", "# notes"),
        ("src/main.rs", "fn main() {}"),
    ]);
    project.git_init();
    assert_eq!(listed(&project.observe()), ["src/main.rs"]);

    // The same tree with no git metadata is unchanged from before D-0017: the
    // walker's rules are the whole basis, and the re-include stands.
    let plain = Project::new(&[
        (".gitignore", "data/\n"),
        (".loreignore", "!data/\n!data/notes.md\n"),
        ("data/notes.md", "# notes"),
        ("src/main.rs", "fn main() {}"),
    ]);
    assert_eq!(listed(&plain.observe()), ["data/notes.md", "src/main.rs"]);
}

/// The watcher's micro-manifest applies the same basis. Absence inside its
/// scope is deletion, so a batch that admitted a gitignored file would index
/// something the next full scan deletes — forever.
#[test]
fn a_watcher_batch_applies_the_same_basis_as_a_full_scan() {
    let project = Project::new(&[
        (".gitignore", "*.log\n"),
        ("src/tracked.rs", "fn tracked() {}"),
        ("run.log", "noise"),
    ]);
    project.git_init().git(&["add", "src/tracked.rs"]);
    project.write("src/fresh.rs", "fn fresh() {}");

    let batch = project.observe_batch(&["src/tracked.rs", "src/fresh.rs", "run.log"]);
    assert_eq!(listed(&batch), ["src/fresh.rs", "src/tracked.rs"]);
    // All three were *observed* — `run.log` is in scope and absent, which is
    // how a newly-gitignored file leaves the index.
    assert!(batch.covers(Utf8Path::new("run.log")));
}

/// Where the *tracked* half of the basis earns its keep, and the case no
/// gitignore evaluator could answer.
///
/// `.gitignore` excludes `data/`; `.loreignore` re-includes it, so lore's own
/// rules admit both files below. Git then separates them by a fact that exists
/// only in its index: one was `git add -f`'d and one was not. The tracked file
/// survives the intersection; the untracked one does not.
#[test]
fn the_tracked_half_of_the_basis_separates_two_files_one_rule_admits() {
    let project = Project::new(&[
        (".gitignore", "data/\n"),
        (".loreignore", "!data/\n!data/*\n"),
        ("data/tracked.md", "# deliberately versioned"),
        ("data/untracked.md", "# scratch"),
    ]);
    project.git_init().git(&["add", "-f", "data/tracked.md"]);

    assert_eq!(listed(&project.observe()), ["data/tracked.md"]);
    // The watcher's per-path form has to reach the same verdict, or the two
    // would fight over these two files forever.
    assert_eq!(
        listed(&project.observe_batch(&["data/tracked.md", "data/untracked.md"])),
        ["data/tracked.md"]
    );
}

/// The other side of the same coin: lore's ignore rules are an *intersection*
/// partner, not a subordinate. A file git tracks that lore's own rules exclude
/// stays excluded — D-0017 intersects, and a tracked file is not a licence to
/// index a build tree somebody deliberately `.loreignore`d.
#[test]
fn tracking_a_file_does_not_override_lores_own_ignore_rules() {
    let project = Project::new(&[
        (".loreignore", "generated/\n"),
        ("generated/big.rs", "// machine written"),
        ("src/main.rs", "fn main() {}"),
    ]);
    project.git_init().git(&["add", "-A"]);
    assert_eq!(listed(&project.observe()), ["src/main.rs"]);
}

// ---------------------------------------------------------------------------
// Credential hard-excludes (D-0015)
// ---------------------------------------------------------------------------

/// The property that makes the list worth having: no ignore file can argue
/// with it. `.loreignore` outranks `.gitignore` everywhere else in this
/// daemon — that is the documented rule — and here it loses.
#[test]
fn a_credential_never_enters_a_manifest_however_hard_loreignore_argues() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        (".env", "API_TOKEN=hunter2"),
        ("certs/server.pem", "-----BEGIN PRIVATE KEY-----"),
        ("certs/id_rsa", "-----BEGIN OPENSSH PRIVATE KEY-----"),
        ("tls/session.key", "raw key bytes"),
        // Every re-inclusion syntax a user might reach for.
        (
            ".loreignore",
            "!.env\n!certs/\n!certs/*\n!tls/session.key\n",
        ),
    ]);
    assert_eq!(listed(&project.observe()), ["src/main.rs"]);

    // And through the watcher, where a file is named directly rather than
    // discovered by a walk.
    let batch = project.observe_batch(&[".env", "certs/server.pem", "tls/session.key"]);
    assert!(listed(&batch).is_empty(), "{:?}", listed(&batch));
}

/// The one escape hatch (D-0015: "only explicit named configuration"). It
/// admits exactly what it names — the sibling secret beside it stays refused.
#[test]
fn the_named_config_key_admits_exactly_the_paths_it_names() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        ("tests/fixtures/sample.pem", "-----BEGIN CERTIFICATE-----"),
        ("certs/real.pem", "-----BEGIN PRIVATE KEY-----"),
        (
            ".lore.toml",
            "[ingest]\nallow_secret_paths = [\"tests/fixtures/sample.pem\"]\n",
        ),
    ]);
    // `.lore.toml` itself is hidden, so it is not in the manifest either — the
    // key is read from disk by the observer, never indexed as its own content.
    assert_eq!(
        listed(&project.observe()),
        ["src/main.rs", "tests/fixtures/sample.pem"]
    );

    // A hidden file too: the key is an admission, not a re-inclusion, so it
    // does not have to survive `hidden(true)` first.
    project.write(".env.example", "API_TOKEN=<yours here>");
    project.write(
        ".lore.toml",
        "[ingest]\nallow_secret_paths = [\".env.example\"]\n",
    );
    let entries = listed(&project.observe());
    assert!(entries.contains(&".env.example".to_string()), "{entries:?}");
    assert!(
        !entries.contains(&"certs/real.pem".to_string()),
        "{entries:?}"
    );

    // …and the watcher agrees, or the two paths would fight over the file.
    let batch = project.observe_batch(&[".env.example", "certs/real.pem"]);
    assert_eq!(listed(&batch), [".env.example"]);
}

/// `.git` answers to nobody — not to `.loreignore`, and not to the escape
/// hatch either. It holds the remote's credentials and it is the mechanism
/// every other rule here is read out of.
#[test]
fn the_escape_hatch_cannot_reach_into_git_metadata() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        (
            ".lore.toml",
            "[ingest]\nallow_secret_paths = [\".git/config\"]\n",
        ),
    ]);
    project.git_init();
    let entries = listed(&project.observe());
    assert!(
        !entries.iter().any(|p| p.starts_with(".git/")),
        "{entries:?}"
    );
}

/// A project outside any work tree behaves exactly as it did before D-0017 —
/// the walker's rules, plus the credential refusals.
#[test]
fn a_non_git_project_is_unchanged_but_for_the_credential_refusals() {
    let project = Project::new(&[
        ("src/main.rs", "fn main() {}"),
        ("docs/guide.md", "# guide"),
        ("id_ed25519", "-----BEGIN OPENSSH PRIVATE KEY-----"),
    ]);
    let snapshot = project.observe();
    assert_eq!(listed(&snapshot), ["docs/guide.md", "src/main.rs"]);
    assert_eq!(
        snapshot.basis_error, None,
        "a non-git project is not a degraded git one"
    );
}
