//! Indexer behaviour, driven synchronously against a real store and a real
//! temp directory — no runtime, no watcher, no timing.

mod daemon_support;

use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use daemon_support::{Fixture, STANDARD_TREE_INDEXED, populate_standard_tree};
use lore::daemon::index::{full_scan, index_paths};

fn paths(items: &[&str]) -> BTreeSet<Utf8PathBuf> {
    items.iter().map(Utf8PathBuf::from).collect()
}

/// A function big enough to survive the chunker's "merge tiny siblings" rule
/// (3.1, `TINY_CHUNK_BYTES`), so a two-function file really does produce two
/// chunks and per-chunk identity is observable.
fn rust_fn(name: &str, value: u32) -> String {
    let padding = "    // padding so this declaration is not a tiny sibling\n".repeat(6);
    format!("pub fn {name}() -> u32 {{\n{padding}    {value}\n}}\n")
}

// ---------------------------------------------------------------------------
// Full scan
// ---------------------------------------------------------------------------

#[test]
fn initial_scan_indexes_source_and_docs_and_nothing_else() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);

    let summary = full_scan(&fixture.context(), &fixture.project);

    assert_eq!(fixture.indexed_paths(), STANDARD_TREE_INDEXED);
    assert_eq!(summary.indexed, 3);
    assert_eq!(summary.unchanged, 0);
    assert_eq!(summary.removed, 0);
    assert_eq!(summary.errors, 0);
    assert!(summary.chunks_inserted >= 3, "{summary:?}");
    assert_eq!(summary.chunks_kept, 0);
    assert!(fixture.chunk_count() > 0);
}

/// Each exclusion category gets its own assertion so a regression names the
/// rule that broke rather than just "the set changed".
#[test]
fn every_exclusion_category_is_enforced() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    full_scan(&fixture.context(), &fixture.project);
    let indexed = fixture.indexed_paths();

    let excluded = [
        ("ignored/secret.txt", "gitignored directory"),
        ("noisy.log", "gitignored glob"),
        ("target/debug/build.rs", "hard-excluded build output"),
        ("node_modules/pkg/index.js", "hard-excluded dependency tree"),
        (
            "Library/ScriptAssemblies/Asm.cs",
            "hard-excluded Unity tree",
        ),
        (".vs/state.txt", "hard-excluded editor state"),
        (".hidden/notes.txt", "hidden directory"),
        (".gitignore", "hidden file"),
    ];
    for (path, why) in excluded {
        assert!(
            !indexed.contains(&path.to_string()),
            "{path} must not be indexed ({why}); indexed = {indexed:?}"
        );
    }
}

/// The gitignore rules apply without `git init` — a project is usually
/// registered with Lore before anyone thinks about its VCS state.
#[test]
fn gitignore_applies_in_a_directory_that_is_not_a_git_repository() {
    let fixture = Fixture::new("demo");
    fixture.write(".gitignore", "build/\n");
    fixture.write("build/artifact.rs", "fn artifact() {}\n");
    fixture.write("keep.rs", "fn keep() {}\n");
    assert!(!fixture.root.join(".git").exists());

    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(fixture.indexed_paths(), ["keep.rs"]);
}

#[test]
fn nested_gitignore_files_are_honoured() {
    let fixture = Fixture::new("demo");
    fixture.write("sub/.gitignore", "*.tmp\n");
    fixture.write("sub/keep.rs", "fn keep() {}\n");
    fixture.write("sub/scratch.tmp", "scratch\n");

    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(fixture.indexed_paths(), ["sub/keep.rs"]);
}

/// The content-hash short circuit: a rescan of an untouched tree must not
/// write anything at all, and an edit must cost exactly one file.
#[test]
fn rescan_reindexes_only_what_changed() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    fixture.write(
        "src/lib.rs",
        format!("{}\n{}", rust_fn("alpha", 41), rust_fn("beta", 1)),
    );
    full_scan(&fixture.context(), &fixture.project);

    let quiet = full_scan(&fixture.context(), &fixture.project);
    assert_eq!(quiet.indexed, 0, "nothing changed, nothing written");
    assert_eq!(quiet.unchanged, 3);
    assert_eq!(quiet.chunks_inserted + quiet.chunks_kept, 0);
    assert_eq!(quiet.removed, 0);

    fixture.write(
        "src/lib.rs",
        format!("{}\n{}", rust_fn("alpha", 41), rust_fn("beta", 100)),
    );
    let after_edit = full_scan(&fixture.context(), &fixture.project);
    assert_eq!(after_edit.indexed, 1, "only the edited file");
    assert_eq!(after_edit.unchanged, 2);
    // Content-addressed ids: the untouched symbol in the edited file keeps
    // its chunk (and therefore its future embedding); only the changed one
    // is rewritten. This is the property the whole id scheme exists for.
    assert!(
        after_edit.chunks_kept > 0,
        "unchanged symbols must survive an edit: {after_edit:?}"
    );
    assert!(after_edit.chunks_inserted > 0, "{after_edit:?}");
    assert!(after_edit.chunks_deleted > 0, "{after_edit:?}");
}

#[test]
fn rewriting_a_file_with_identical_bytes_is_a_no_op() {
    let fixture = Fixture::new("demo");
    let body = "pub fn same() {}\n";
    fixture.write("src/lib.rs", body);
    full_scan(&fixture.context(), &fixture.project);

    fixture.write("src/lib.rs", body); // new mtime, same content
    let summary = full_scan(&fixture.context(), &fixture.project);
    assert_eq!(
        (summary.indexed, summary.unchanged),
        (0, 1),
        "change detection is by content, not mtime"
    );
}

#[test]
fn deleted_files_are_pruned_by_the_next_scan() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    fixture.remove("README.md");
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert_eq!(summary.removed, 1);
    assert_eq!(fixture.indexed_paths(), ["docs/design.md", "src/lib.rs"]);
}

/// A source file replaced by a binary blob (a build step clobbering it, a bad
/// merge) must lose its chunks — otherwise the index keeps serving text that
/// no longer exists anywhere on disk.
#[test]
fn a_previously_indexed_file_that_becomes_binary_is_removed() {
    let fixture = Fixture::new("demo");
    fixture.write("data.txt", "readable text\n");
    fixture.write("src/lib.rs", "pub fn keep() {}\n");
    full_scan(&fixture.context(), &fixture.project);
    assert!(fixture.indexed_paths().contains(&"data.txt".to_string()));

    fixture.write("data.txt", b"\x00\x01\x02binary now\x00");
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.removed, 1);
    assert_eq!(fixture.indexed_paths(), ["src/lib.rs"]);
}

#[test]
fn a_binary_file_that_was_never_indexed_is_simply_skipped() {
    let fixture = Fixture::new("demo");
    fixture.write("blob.bin", b"\x00\xff\x00\xff");
    fixture.write("src/lib.rs", "pub fn keep() {}\n");

    let summary = full_scan(&fixture.context(), &fixture.project);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.removed, 0, "nothing was there to remove");
    assert_eq!(fixture.indexed_paths(), ["src/lib.rs"]);
}

#[test]
fn every_completed_pass_moves_the_generation() {
    let fixture = Fixture::new("demo");
    fixture.write("src/lib.rs", "pub fn a() {}\n");

    let before = fixture.generation();
    full_scan(&fixture.context(), &fixture.project);
    let after_first = fixture.generation();
    assert!(after_first > before);

    // Even a pass that changed nothing: clients poll this to learn that the
    // reindex they asked for has finished.
    full_scan(&fixture.context(), &fixture.project);
    assert!(fixture.generation() > after_first);
}

#[test]
fn the_daemons_own_data_directory_is_never_indexed() {
    let fixture = Fixture::new("demo");
    fixture.write("src/lib.rs", "pub fn a() {}\n");
    // Pathological but legal: the data dir sits inside the project root.
    let nested = fixture.root.join("nested-data");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("lore.db"), "pretend database").unwrap();
    std::fs::write(nested.join("config.toml"), "[embeddings]\n").unwrap();

    let mut context = fixture.context();
    context.data_dir = nested;
    full_scan(&context, &fixture.project);

    assert_eq!(fixture.indexed_paths(), ["src/lib.rs"]);
}

#[test]
fn a_cancelled_pass_stops_early_and_does_not_bump_the_generation() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    let context = fixture.context();
    context.cancel.cancel();

    let before = fixture.generation();
    let summary = full_scan(&context, &fixture.project);

    assert!(summary.cancelled);
    assert_eq!(summary.indexed, 0);
    assert_eq!(
        fixture.generation(),
        before,
        "an aborted pass is not a pass"
    );
}

// ---------------------------------------------------------------------------
// Incremental passes (what a watcher batch turns into)
// ---------------------------------------------------------------------------

#[test]
fn incremental_pass_indexes_exactly_the_named_paths() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    fixture.write("src/lib.rs", "pub fn alpha() -> u32 {\n    0\n}\n");
    fixture.write("src/extra.rs", "pub fn extra() {}\n");

    let summary = index_paths(
        &fixture.context(),
        &fixture.project,
        &paths(&["src/lib.rs", "src/extra.rs", "README.md"]),
    );

    assert_eq!(summary.indexed, 2, "README.md was untouched: {summary:?}");
    assert_eq!(summary.unchanged, 1);
    let indexed = fixture.indexed_paths();
    assert!(indexed.contains(&"src/extra.rs".to_string()));
}

#[test]
fn incremental_pass_prunes_a_path_that_vanished() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    fixture.remove("README.md");
    let summary = index_paths(&fixture.context(), &fixture.project, &paths(&["README.md"]));

    assert_eq!(summary.removed, 1);
    assert!(!fixture.indexed_paths().contains(&"README.md".to_string()));
}

/// Deleting a directory produces one event naming the directory — the files
/// under it are never mentioned again, so the prune has to be by prefix.
#[test]
fn removing_a_directory_prunes_everything_under_it() {
    let fixture = Fixture::new("demo");
    fixture.write("src/a.rs", "pub fn a() {}\n");
    fixture.write("src/deep/b.rs", "pub fn b() {}\n");
    fixture.write("README.md", "# demo\n");
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(fixture.indexed_paths().len(), 3);

    fixture.remove_dir("src");
    let summary = index_paths(&fixture.context(), &fixture.project, &paths(&["src"]));

    assert_eq!(summary.removed, 2, "both files under src/: {summary:?}");
    assert_eq!(fixture.indexed_paths(), ["README.md"]);
}

/// A whole tree appearing at once (branch checkout, unzip) arrives as a
/// directory event; the indexer expands it rather than losing the contents.
#[test]
fn a_new_directory_is_expanded_into_its_files() {
    let fixture = Fixture::new("demo");
    fixture.write("README.md", "# demo\n");
    full_scan(&fixture.context(), &fixture.project);

    fixture.write("added/one.rs", "pub fn one() {}\n");
    fixture.write("added/deep/two.rs", "pub fn two() {}\n");
    let summary = index_paths(&fixture.context(), &fixture.project, &paths(&["added"]));

    assert_eq!(summary.indexed, 2, "{summary:?}");
    assert_eq!(
        fixture.indexed_paths(),
        ["README.md", "added/deep/two.rs", "added/one.rs"]
    );
}

/// A file that is still on disk but has become ignored (new `.gitignore`
/// rule, or moved under an excluded directory) must leave the index —
/// otherwise the incremental path and a full rescan would disagree forever.
#[test]
fn a_file_that_became_ignored_is_dropped_from_the_index() {
    let fixture = Fixture::new("demo");
    fixture.write("secret.key", "shhh\n");
    fixture.write("README.md", "# demo\n");
    full_scan(&fixture.context(), &fixture.project);
    assert!(fixture.indexed_paths().contains(&"secret.key".to_string()));

    fixture.write(".gitignore", "*.key\n");
    let summary = index_paths(
        &fixture.context(),
        &fixture.project,
        &paths(&["secret.key"]),
    );

    assert_eq!(summary.removed, 1, "{summary:?}");
    assert_eq!(fixture.indexed_paths(), ["README.md"]);
}

#[test]
fn incremental_pass_ignores_excluded_paths_without_touching_disk() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    let summary = index_paths(
        &fixture.context(),
        &fixture.project,
        &paths(&[
            "target/debug/build.rs",
            "node_modules/pkg/index.js",
            ".vs/state.txt",
        ]),
    );

    assert_eq!(summary.seen, 0);
    assert_eq!(summary.indexed, 0);
    assert_eq!(summary.removed, 0);
    assert_eq!(fixture.indexed_paths(), STANDARD_TREE_INDEXED);
}
