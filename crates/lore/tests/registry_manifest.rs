//! `projects.toml` is authoritative and the `projects` table is derived from
//! it (adversarial review S1#7). These tests pin the reconciliation directions,
//! because getting them backwards is silently destructive in both directions:
//! trusting the database would make a hand-edited manifest a no-op, and
//! trusting an absent manifest would drop every registered project on upgrade.

use camino::{Utf8Path, Utf8PathBuf};
use lore::registry::{self, Manifest, ManifestEntry, Reconciliation};
use lore::store::{ProjectSpec, Store};
use lore::types::SourceKind;
use std::collections::BTreeSet;
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    data_dir: Utf8PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir =
            Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8 temp dir");
        let store = Store::open(data_dir.join("lore.db")).expect("open store");
        Self {
            _dir: dir,
            data_dir,
            store,
        }
    }

    fn reconcile(&mut self) -> Reconciliation {
        registry::reconcile(&mut self.store, &self.data_dir).expect("reconcile")
    }

    fn manifest(&self) -> Manifest {
        registry::read(&self.data_dir)
            .expect("read manifest")
            .expect("a manifest should exist")
    }

    /// (name, key, root) of every project in the database, in row order.
    fn rows(&self) -> Vec<(String, String, String)> {
        self.store
            .list_projects()
            .expect("list projects")
            .into_iter()
            .map(|p| (p.name, p.key, p.root.into_string()))
            .collect()
    }
}

fn entry(key: &str, name: &str, root: &str) -> ManifestEntry {
    ManifestEntry {
        key: key.to_string(),
        name: name.to_string(),
        root: Utf8PathBuf::from(root),
        kind: SourceKind::Repo,
    }
}

/// The upgrade path: a database written by a daemon that predates the manifest
/// has projects and no `projects.toml`. Its rows are the only surviving record
/// of the user's roots, so this is the one direction in which the database is
/// trusted.
#[test]
fn a_missing_manifest_is_bootstrapped_from_the_database() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/lore"), "lore")
        .unwrap();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/lexomancy"), "lexomancy")
        .unwrap();
    // Simulate pre-key rows: reconciliation must fill these in.
    std::fs::remove_file(registry::manifest_path(&fixture.data_dir)).ok();

    let outcome = fixture.reconcile();
    assert!(
        matches!(outcome, Reconciliation::Bootstrapped { projects: 2, .. }),
        "{outcome:?}"
    );

    let manifest = fixture.manifest();
    let roots: Vec<&str> = manifest
        .projects
        .iter()
        .map(|entry| entry.root.as_str())
        .collect();
    assert_eq!(roots, ["C:/repos/lore", "C:/repos/lexomancy"]);
    assert!(manifest.projects.iter().all(|entry| !entry.key.is_empty()));
    assert_eq!(manifest.projects[0].key, "lore");
}

/// A first run has neither, and must still leave the file behind so the
/// registry is discoverable rather than conjured on first registration.
#[test]
fn a_fresh_data_dir_gets_an_empty_manifest() {
    let mut fixture = Fixture::new();
    let outcome = fixture.reconcile();
    assert_eq!(
        outcome,
        Reconciliation::Bootstrapped {
            projects: 0,
            keys_assigned: 0
        }
    );
    assert!(fixture.manifest().projects.is_empty());
}

/// Once the manifest exists it wins in both directions: entries it names are
/// inserted, rows it dropped are removed. Editing the file with the daemon down
/// is the supported way to deregister a project.
#[test]
fn the_manifest_inserts_what_it_names_and_removes_what_it_dropped() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/stale"), "stale")
        .unwrap();
    fixture.reconcile();

    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![
                entry("lore", "lore", "C:/repos/lore"),
                entry("lex", "lexomancy", "C:/repos/lexomancy"),
            ],
        },
    )
    .unwrap();

    let outcome = fixture.reconcile();
    assert_eq!(
        outcome,
        Reconciliation::Applied {
            inserted: 2,
            updated: 0,
            removed: 1
        }
    );
    assert_eq!(
        fixture.rows(),
        [
            ("lore".into(), "lore".into(), "C:/repos/lore".into()),
            (
                "lexomancy".into(),
                "lex".into(),
                "C:/repos/lexomancy".into()
            ),
        ]
    );
}

/// The key is the one thing about a project that never moves — that is the
/// entire reason `SearchResult.project_key` is trustworthy where the display
/// name is not (S1#3).
#[test]
fn a_rename_keeps_the_key() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/lore"), "lore")
        .unwrap();
    fixture.reconcile();
    let original = fixture.rows()[0].1.clone();

    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/lore"), "renamed")
        .unwrap();
    let (name, key, _) = fixture.rows().pop().unwrap();
    assert_eq!(name, "renamed");
    assert_eq!(key, original, "a rename must not move the key");
}

/// Two roots whose display names slug identically get distinct keys — the
/// `shared`/`shared` collision the review used as its failure scenario.
#[test]
fn colliding_names_still_get_distinct_keys() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/a/shared"), "shared")
        .unwrap();
    fixture
        .store
        .register_project(Utf8Path::new("D:/work/shared"), "shared")
        .unwrap();

    let keys: Vec<String> = fixture.rows().into_iter().map(|(_, key, _)| key).collect();
    assert_eq!(keys[0], "shared");
    assert_ne!(keys[0], keys[1], "{keys:?}");
    assert!(keys[1].starts_with("shared-"), "{keys:?}");
}

/// A hand-written manifest is a supported input, so it must survive being
/// written by hand: no key, no name, just a root.
#[test]
fn a_minimal_hand_written_entry_is_completed_rather_than_rejected() {
    let mut fixture = Fixture::new();
    std::fs::write(
        registry::manifest_path(&fixture.data_dir),
        "[[project]]\nroot = 'C:/repos/lexomancy'\n",
    )
    .unwrap();

    fixture.reconcile();
    let (name, key, root) = fixture.rows().pop().unwrap();
    assert_eq!(root, "C:/repos/lexomancy");
    assert_eq!(name, "lexomancy", "name derived from the root");
    assert_eq!(key, "lexomancy");
    // …and the completed form is written back, so the next start is a no-op.
    assert_eq!(
        fixture.manifest().projects[0],
        entry("lexomancy", "lexomancy", "C:/repos/lexomancy")
    );
}

/// A manifest we cannot parse is the user's data. Overwriting it would destroy
/// the only external record of their roots, so it is moved aside — loudly —
/// and the database is used to rebuild.
#[test]
fn an_unparseable_manifest_is_quarantined_not_overwritten() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/lore"), "lore")
        .unwrap();
    let garbage = "[[project]\nroot = broken";
    std::fs::write(registry::manifest_path(&fixture.data_dir), garbage).unwrap();

    let outcome = fixture.reconcile();
    assert!(
        matches!(outcome, Reconciliation::Bootstrapped { projects: 1, .. }),
        "{outcome:?}"
    );
    let quarantined =
        std::fs::read_to_string(fixture.data_dir.join(registry::MANIFEST_QUARANTINE)).unwrap();
    assert_eq!(quarantined, garbage, "the original bytes are recoverable");
    assert_eq!(fixture.manifest().projects.len(), 1);
    assert_eq!(fixture.rows().len(), 1, "the project survived");
}

/// The manifest is a hand-editable file, so the ways a human breaks it are
/// part of its contract. Duplicate keys are the likely one — copy an entry,
/// change the root, forget the key — and the key is the handle `expand` uses,
/// so two entries sharing one is not cosmetic. Neither project may be dropped
/// and neither may be left unreachable.
#[test]
fn a_manifest_with_duplicate_keys_reassigns_instead_of_losing_a_project() {
    let mut fixture = Fixture::new();
    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![
                entry("lore", "lore", "C:/repos/lore"),
                entry("lore", "lexomancy", "C:/repos/lexomancy"),
            ],
        },
    )
    .unwrap();

    let outcome = fixture.reconcile();
    assert_eq!(
        outcome,
        Reconciliation::Applied {
            inserted: 2,
            updated: 0,
            removed: 0
        },
        "both projects survive"
    );

    let rows = fixture.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "lore", "the first claimant keeps the key");
    assert_ne!(rows[1].1, "lore", "{rows:?}");
    assert!(!rows[1].1.is_empty(), "{rows:?}");

    // The repair is written back, so the next start is a no-op rather than a
    // second reassignment with a different random suffix.
    let repaired: Vec<String> = fixture
        .manifest()
        .projects
        .iter()
        .map(|entry| entry.key.clone())
        .collect();
    assert_eq!(repaired, vec![rows[0].1.clone(), rows[1].1.clone()]);
    let outcome = fixture.reconcile();
    assert_eq!(
        outcome,
        Reconciliation::Applied {
            inserted: 0,
            updated: 2,
            removed: 0
        }
    );
    assert_eq!(
        fixture
            .rows()
            .into_iter()
            .map(|(_, key, _)| key)
            .collect::<Vec<_>>(),
        repaired,
        "keys are stable across restarts once repaired"
    );
}

/// The same root listed twice (a merge conflict resolved badly) must not
/// produce two rows for one directory, which would double every search hit.
#[test]
fn a_manifest_listing_one_root_twice_registers_it_once() {
    let mut fixture = Fixture::new();
    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![
                entry("lore", "lore", "C:/repos/lore"),
                entry("dup", "lore-again", r"C:\repos\lore\"),
            ],
        },
    )
    .unwrap();

    fixture.reconcile();
    let rows = fixture.rows();
    assert_eq!(
        rows.len(),
        1,
        "separator and case variants are one root: {rows:?}"
    );
    assert_eq!(rows[0].0, "lore", "the first entry wins");
    assert_eq!(fixture.manifest().projects.len(), 1);
}

/// Roots the daemon cannot use: a relative path, and one that does not exist.
///
/// Asserting the *actual* behavior rather than a wish: both are accepted
/// verbatim, because `reconcile` deliberately does not touch the filesystem —
/// a root on a disconnected network share or an unmounted drive must not be
/// silently deregistered on a restart, which is what an existence check at
/// startup would do. The consequences are real but bounded, and belong in the
/// report rather than in a startup failure:
///
/// - a missing root indexes to nothing and shows `files: 0` in `lore status`;
/// - a **relative** root is resolved against the daemon's working directory by
///   everything downstream, so it names a different tree depending on where
///   the daemon was launched. Registration through `POST /v1/projects`
///   canonicalizes and cannot produce one; only a hand-edited manifest can.
#[test]
fn a_relative_or_missing_manifest_root_is_accepted_as_written() {
    let mut fixture = Fixture::new();
    std::fs::write(
        registry::manifest_path(&fixture.data_dir),
        "[[project]]\nkey = 'rel'\nname = 'relative'\nroot = 'design/vault'\n\n\
         [[project]]\nkey = 'gone'\nname = 'missing'\nroot = 'C:/nowhere/at/all'\n",
    )
    .unwrap();

    let outcome = fixture.reconcile();
    assert_eq!(
        outcome,
        Reconciliation::Applied {
            inserted: 2,
            updated: 0,
            removed: 0
        },
        "a root the daemon cannot reach is still a registration, not an error"
    );
    assert_eq!(
        fixture.rows(),
        [
            ("relative".into(), "rel".into(), "design/vault".into()),
            ("missing".into(), "gone".into(), "C:/nowhere/at/all".into()),
        ],
        "roots are stored exactly as written, unresolved and unverified"
    );
}

/// Removing a project takes its index with it; leaving orphaned chunks behind
/// would make a deregistered project keep answering searches.
#[test]
fn dropping_a_project_from_the_manifest_drops_its_index() {
    let mut fixture = Fixture::new();
    let id = fixture
        .store
        .register_project(Utf8Path::new("C:/repos/lore"), "lore")
        .unwrap();
    fixture.reconcile();

    let path = Utf8Path::new("src/lib.rs");
    // Code, so the profile changes nothing about the chunks; `None` keeps the
    // fixture honest about what this test is (registry reconciliation).
    let chunks = lore::chunk::chunk_file(path, b"pub fn alpha() -> u32 {\n    41\n}\n", None);
    fixture
        .store
        .replace_file_chunks(id, path, "h1", chunks.chunks())
        .unwrap();
    assert!(fixture.store.status().unwrap().projects[0].chunks > 0);

    registry::write(&fixture.data_dir, &Manifest::default()).unwrap();
    fixture.reconcile();
    assert!(fixture.store.status().unwrap().projects.is_empty());
    assert!(fixture.store.list_files(id).unwrap().is_empty());
}

/// Two hand-edited entries that *exchange* keys — the one manifest edit that
/// collides with `projects.key`'s uniqueness index while both old values are
/// still in the table.
///
/// This pins the invariants that must hold whichever way the collision is
/// resolved, and they are deliberately weaker than what the daemon now does
/// (see `a_key_exchange_converges_on_the_first_start`): a future resolution is
/// free to reassign rather than swap, and must still not
///
/// - lose a registered project or let two share a key;
/// - make reconciliation fatal — `daemon::run` logs a failure and carries on
///   with the index's own list, so a bad manifest edit cannot make the daemon
///   unstartable.
///
/// Historical note, so the gap is not rediscovered: `apply` used to upsert row
/// by row with no enclosing transaction, so the swap failed on a raw `UNIQUE
/// constraint failed: projects.key` after earlier entries had already been
/// committed — half-applied, and failing identically on every subsequent start
/// instead of converging. `Store::apply_project_set` now does the whole
/// reconciliation in one transaction, releasing the old keys before claiming
/// the new ones.
#[test]
fn a_manifest_that_exchanges_two_keys_never_loses_a_project_or_duplicates_a_key() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/a"), "a")
        .unwrap();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/b"), "b")
        .unwrap();
    fixture.reconcile();

    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![entry("b", "a", "C:/repos/a"), entry("a", "b", "C:/repos/b")],
        },
    )
    .unwrap();

    // Deliberately not `.expect(...)`: the daemon treats a reconciliation
    // failure as recoverable, and so must this test.
    let outcome = registry::reconcile(&mut fixture.store, &fixture.data_dir);
    let rows = fixture.rows();

    let roots: Vec<&str> = rows.iter().map(|(_, _, root)| root.as_str()).collect();
    assert_eq!(
        roots,
        ["C:/repos/a", "C:/repos/b"],
        "both projects survive however the collision is resolved: {outcome:?}"
    );
    let keys: BTreeSet<&str> = rows.iter().map(|(_, key, _)| key.as_str()).collect();
    assert_eq!(keys.len(), 2, "keys stay distinct: {rows:?}");
    assert!(keys.iter().all(|key| !key.is_empty()), "{rows:?}");
}

/// The manifest is authoritative, so a key exchange is not merely survivable —
/// it must *apply*, on the first start, exactly as written. Anything less means
/// the file says one thing and the index answers another, with no error the
/// user ever sees (reconciliation failures are logged and swallowed).
///
/// The same edit also removes a third project and adds a fourth, because the
/// old row-at-a-time apply lost precisely those: it aborted at the colliding
/// upsert, so insertions before it stuck and every removal after it was
/// skipped.
#[test]
fn a_key_exchange_converges_on_the_first_start() {
    let mut fixture = Fixture::new();
    for (root, name) in [
        ("C:/repos/a", "a"),
        ("C:/repos/b", "b"),
        ("C:/repos/gone", "gone"),
    ] {
        fixture
            .store
            .register_project(Utf8Path::new(root), name)
            .unwrap();
    }
    fixture.reconcile();

    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![
                entry("b", "a", "C:/repos/a"),
                entry("a", "b", "C:/repos/b"),
                entry("new", "new", "C:/repos/new"),
            ],
        },
    )
    .unwrap();

    let outcome = registry::reconcile(&mut fixture.store, &fixture.data_dir).expect("reconcile");
    assert_eq!(
        outcome,
        Reconciliation::Applied {
            inserted: 1,
            updated: 2,
            removed: 1
        }
    );
    assert_eq!(
        fixture.rows(),
        [
            ("a".into(), "b".into(), "C:/repos/a".into()),
            ("b".into(), "a".into(), "C:/repos/b".into()),
            ("new".into(), "new".into(), "C:/repos/new".into()),
        ],
        "the database matches the manifest exactly, keys included"
    );

    // Converged: a second start is a no-op that changes nothing and reassigns
    // nothing.
    let outcome = fixture.reconcile();
    assert_eq!(
        outcome,
        Reconciliation::Applied {
            inserted: 0,
            updated: 3,
            removed: 0
        }
    );
    assert_eq!(
        fixture.rows(),
        [
            ("a".into(), "b".into(), "C:/repos/a".into()),
            ("b".into(), "a".into(), "C:/repos/b".into()),
            ("new".into(), "new".into(), "C:/repos/new".into()),
        ]
    );
}

/// A three-way key rotation, which no ordering of one-row-at-a-time writes can
/// perform: whichever row you write first wants a key another still holds. It
/// only works because the transaction releases all three before claiming any.
#[test]
fn a_three_way_key_rotation_applies() {
    let mut fixture = Fixture::new();
    for name in ["a", "b", "c"] {
        fixture
            .store
            .register_project(Utf8Path::new(&format!("C:/repos/{name}")), name)
            .unwrap();
    }
    fixture.reconcile();

    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![
                entry("b", "a", "C:/repos/a"),
                entry("c", "b", "C:/repos/b"),
                entry("a", "c", "C:/repos/c"),
            ],
        },
    )
    .unwrap();

    registry::reconcile(&mut fixture.store, &fixture.data_dir).expect("reconcile");
    assert_eq!(
        fixture
            .rows()
            .into_iter()
            .map(|(name, key, _)| (name, key))
            .collect::<Vec<_>>(),
        [
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "c".to_string()),
            ("c".to_string(), "a".to_string()),
        ]
    );
}

/// A key the manifest hands to a surviving project may still be held by one the
/// same edit *deletes* — so removals have to run before any key is claimed.
/// Handing `b`'s key to `a` while `b` is on its way out is the smallest case.
#[test]
fn a_key_freed_by_a_removal_can_be_claimed_in_the_same_edit() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/a"), "a")
        .unwrap();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/b"), "b")
        .unwrap();
    fixture.reconcile();

    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![entry("b", "a", "C:/repos/a")],
        },
    )
    .unwrap();

    registry::reconcile(&mut fixture.store, &fixture.data_dir).expect("reconcile");
    assert_eq!(
        fixture.rows(),
        [("a".into(), "b".into(), "C:/repos/a".into())]
    );
}

/// The same root spelled differently — backslashes, a trailing separator,
/// different case — is the same project, and `root_key` already says so. It has
/// to reach the database that way too: treating the respelling as a *new* root
/// would insert a rival row still wanting the key the original holds, which is
/// the non-convergence this fix exists to remove. The row is rewritten in
/// place, keeping its id and therefore its index.
#[test]
fn a_respelled_root_updates_the_existing_row_rather_than_rivalling_it() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .register_project(Utf8Path::new("C:/repos/lore"), "lore")
        .unwrap();
    fixture.reconcile();

    registry::write(
        &fixture.data_dir,
        &Manifest {
            projects: vec![entry("lore", "lore", r"C:\repos\lore\")],
        },
    )
    .unwrap();

    let outcome = registry::reconcile(&mut fixture.store, &fixture.data_dir).expect("reconcile");
    assert_eq!(
        outcome,
        Reconciliation::Applied {
            inserted: 0,
            updated: 1,
            removed: 0
        },
        "a respelling is an update, not a registration"
    );
    assert_eq!(
        fixture.rows(),
        [("lore".into(), "lore".into(), r"C:\repos\lore\".into())],
        "one row, respelled as the manifest asked"
    );
}

/// Atomicity, proved through the one illegal input the store cannot repair: a
/// `desired` set that collides with *itself*. `registry::apply` never produces
/// one, so this drives `Store::apply_project_set` directly — the point is that
/// a failure partway through leaves nothing behind, not that this particular
/// input is reachable.
///
/// Without the enclosing transaction the first spec's insert and the removal
/// would both have stuck; with it, the failing third spec rolls the whole
/// reconciliation back to the state the previous start left.
#[test]
fn a_failed_apply_leaves_the_database_untouched() {
    let mut fixture = Fixture::new();
    let stale = fixture
        .store
        .register_project(Utf8Path::new("C:/repos/stale"), "stale")
        .unwrap();
    fixture.reconcile();
    let before = fixture.rows();

    let spec = |key: &str, name: &str, root: &str| ProjectSpec {
        id: None,
        root: Utf8PathBuf::from(root),
        name: name.to_string(),
        key: key.to_string(),
        kind: SourceKind::Repo,
    };
    let err = fixture
        .store
        .apply_project_set(
            &[
                spec("fresh", "fresh", "C:/repos/fresh"),
                spec("dup", "one", "C:/repos/one"),
                spec("dup", "two", "C:/repos/two"),
            ],
            &[stale],
        )
        .expect_err("a self-colliding desired set cannot be applied");
    assert!(
        err.to_string().contains("UNIQUE"),
        "the store reports the real conflict: {err}"
    );

    assert_eq!(
        fixture.rows(),
        before,
        "no insert and no removal survived the rollback"
    );
}
