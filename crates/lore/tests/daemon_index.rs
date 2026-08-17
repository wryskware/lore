//! Indexer behaviour, driven synchronously against a real store and a real
//! temp directory — no runtime, no watcher, no timing.

mod daemon_support;

use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use daemon_support::{Fixture, STANDARD_TREE_INDEXED, link_dir, populate_standard_tree};
use lore::daemon::index::{ApplyOptions, full_scan, full_scan_with, index_paths};
use lore::store::SearchFilter;

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
        ("target/debug/build.rs", ".loreignore build output"),
        ("node_modules/pkg/index.js", ".loreignore dependency tree"),
        ("Library/ScriptAssemblies/Asm.cs", ".loreignore Unity tree"),
        (".vs/state.txt", "hidden editor state"),
        (".hidden/notes.txt", "hidden directory"),
        (".gitignore", "hidden file"),
        (".loreignore", "hidden file"),
    ];
    for (path, why) in excluded {
        assert!(
            !indexed.contains(&path.to_string()),
            "{path} must not be indexed ({why}); indexed = {indexed:?}"
        );
    }
}

/// A scan writes **nothing** into the project it is scanning.
///
/// This replaces the pair of tests that pinned `.loreignore` generation (that a
/// first scan wrote one, and that later scans never rewrote it). D-0020 retired
/// generation outright, so the property worth pinning inverted: a project that
/// has no ignore file still has none afterwards, and a build tree is indexed
/// until a human says otherwise.
#[test]
fn a_full_scan_writes_nothing_into_the_project() {
    let fixture = Fixture::new("demo");
    fixture.write("Cargo.toml", "[package]\nname = \"demo\"\n");
    fixture.write("src/lib.rs", "pub fn a() {}\n");
    fixture.write("target/debug/build.rs", "fn generated() {}\n");
    let ignore_file = fixture.root.join(".loreignore");

    full_scan(&fixture.context(), &fixture.project);

    assert!(!ignore_file.exists(), "the scan must not generate one");
    assert!(
        fixture
            .indexed_paths()
            .contains(&"target/debug/build.rs".to_string()),
        "nothing excludes it yet, and lore ships no rule that would: {:?}",
        fixture.indexed_paths()
    );

    // And a hand-written one is obeyed on the next pass, with no generation
    // step to fight it.
    std::fs::write(&ignore_file, "target/\n").unwrap();
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(std::fs::read_to_string(&ignore_file).unwrap(), "target/\n");
    assert!(
        !fixture
            .indexed_paths()
            .contains(&"target/debug/build.rs".to_string()),
        "{:?}",
        fixture.indexed_paths()
    );
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
    let chunks_before = fixture.chunk_count();

    fixture.remove("README.md");
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert_eq!(summary.removed, 1);
    assert_eq!(fixture.indexed_paths(), ["docs/design.md", "src/lib.rs"]);
    // Issue #9: a prune deletes chunks, and the summary used to report zero of
    // them because only `replace_file_chunks` was counted. The index has to
    // have shrunk by exactly what the pass says it deleted.
    assert!(summary.chunks_deleted > 0, "{summary:?}");
    assert_eq!(
        fixture.chunk_count() + summary.chunks_deleted as u64,
        chunks_before,
        "{summary:?}"
    );
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
    // The user-level rules live beside `config.toml`, so moving the data
    // directory moves them: this pass reads them from `nested`.
    std::fs::write(nested.join(lore::daemon::walk::USER_IGNORE_FILE), ".*\n").unwrap();

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

/// End-to-end proof that a container heading's short introduction reaches
/// FTS. A one-sentence rule under a parent heading is exactly the prose an
/// agent asks for, and it used to exist in no chunk and no FTS row at all.
#[test]
fn a_short_markdown_parent_introduction_reaches_the_index() {
    let fixture = Fixture::new("demo");
    fixture.write(
        "docs/safety.md",
        "# Safety\n\nNever upload.\n\n## Details\n\nEverything stays on the local machine.\n",
    );
    full_scan(&fixture.context(), &fixture.project);

    let hits = fixture
        .store
        .blocking(|store| store.lexical_search("upload", &SearchFilter::default(), 10))
        .expect("lexical search");
    assert!(
        hits.iter().any(|h| h.chunk.text.contains("Never upload.")),
        "short parent intro is not searchable; hits = {:?}",
        hits.iter()
            .map(|h| h.chunk.text.as_str())
            .collect::<Vec<_>>()
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
///
/// The file is deliberately *not* credential-shaped: the credential
/// hard-excludes (D-0015) refuse those before any ignore rule is consulted, so
/// a `*.key` fixture here would pass for the wrong reason.
#[test]
fn a_file_that_became_ignored_is_dropped_from_the_index() {
    let fixture = Fixture::new("demo");
    fixture.write("notes.txt", "shhh\n");
    fixture.write("README.md", "# demo\n");
    full_scan(&fixture.context(), &fixture.project);
    assert!(fixture.indexed_paths().contains(&"notes.txt".to_string()));

    fixture.write(".gitignore", "*.txt\n");
    let summary = index_paths(&fixture.context(), &fixture.project, &paths(&["notes.txt"]));

    assert_eq!(summary.removed, 1, "{summary:?}");
    assert_eq!(fixture.indexed_paths(), ["README.md"]);
}

/// Two layers, and where the line between them sits matters — it moved with
/// D-0020. Only `.git` is rejected on the name alone now, because it is the one
/// exclusion no rule can argue with. A hidden path is an *ordinary* ignored
/// path: it costs a directory listing, exactly as a build tree does, and it is
/// re-includable by a rule the user can read and change.
#[test]
fn an_incremental_pass_indexes_neither_hidden_nor_ignored_paths() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    let stat_free = index_paths(
        &fixture.context(),
        &fixture.project,
        &paths(&[".git/config"]),
    );
    assert_eq!(
        stat_free.seen, 0,
        "the disk is never consulted for the one hard floor: {stat_free:?}"
    );

    let hidden = index_paths(
        &fixture.context(),
        &fixture.project,
        &paths(&[".vs/state.txt"]),
    );
    assert_eq!(
        (hidden.indexed, hidden.removed),
        (0, 0),
        "observed and then excluded by a rule, rather than dropped on its name: {hidden:?}"
    );

    let ignored = index_paths(
        &fixture.context(),
        &fixture.project,
        &paths(&["target/debug/build.rs", "node_modules/pkg/index.js"]),
    );
    assert_eq!(ignored.indexed, 0, "{ignored:?}");
    assert_eq!(
        ignored.removed, 0,
        "they were never indexed, so there is nothing to remove: {ignored:?}"
    );
    assert_eq!(fixture.indexed_paths(), STANDARD_TREE_INDEXED);
}

// ---------------------------------------------------------------------------
// The mass-delete guard (D-0015)
// ---------------------------------------------------------------------------

/// The guard, and the only way past it. Both halves in one test on purpose: a
/// refusal nobody can override is as broken as an override nobody needs.
#[test]
fn a_snapshot_that_would_delete_most_of_a_project_is_refused_until_overridden() {
    let fixture = Fixture::new("demo");
    for i in 0..200 {
        fixture.write(&format!("src/f{i}.rs"), rust_fn("f", i));
    }
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(fixture.indexed_paths().len(), 200);

    // Over half, and over a hundred: both conditions, as D-0015 states them.
    for i in 0..150 {
        fixture.remove(&format!("src/f{i}.rs"));
    }
    let refused = full_scan(&fixture.context(), &fixture.project);
    assert_eq!(
        refused
            .mass_delete_blocked
            .map(|trip| (trip.deletes, trip.stored)),
        Some((150, 200)),
        "{refused:?}"
    );
    assert_eq!(
        fixture.indexed_paths().len(),
        200,
        "a refused apply writes nothing at all: {refused:?}"
    );

    let forced = full_scan_with(
        &fixture.context(),
        &fixture.project,
        ApplyOptions {
            allow_mass_delete: true,
        },
    );
    assert!(forced.mass_delete_blocked.is_none(), "{forced:?}");
    assert_eq!(fixture.indexed_paths().len(), 50);
}

/// A trip is *remembered*, not merely returned to whoever ran the pass.
///
/// D-0015 requires a tripped guard to be visible in `status`, and what `status`
/// reports is this record. It has to survive every subsequent refused pass —
/// the watcher keeps re-observing the same shrunken tree — and clear only when
/// an apply actually succeeds, which for a deletion the human meant is
/// `lore index --allow-mass-delete`.
#[test]
fn a_tripped_guard_is_remembered_until_a_pass_succeeds() {
    let fixture = Fixture::new("demo");
    // One context for every pass here: the daemon has one, and a record that
    // did not outlive the pass that made it would report nothing.
    let ctx = fixture.context();
    for i in 0..120 {
        fixture.write(&format!("src/f{i}.rs"), rust_fn("f", i));
    }
    full_scan(&ctx, &fixture.project);
    assert_eq!(
        ctx.guard.of(fixture.project.id),
        None,
        "a project whose index is tracking it reports no trip"
    );

    for i in 0..110 {
        fixture.remove(&format!("src/f{i}.rs"));
    }
    full_scan(&ctx, &fixture.project);
    assert_eq!(
        ctx.guard
            .of(fixture.project.id)
            .map(|trip| (trip.deletes, trip.stored)),
        Some((110, 120)),
        "the numbers that refused the apply are the ones `status` shows"
    );

    // The tree is still shrunken, so the next pass refuses identically. A
    // record that reset itself here would make a stuck index look healthy
    // between passes.
    full_scan(&ctx, &fixture.project);
    assert!(ctx.guard.of(fixture.project.id).is_some());

    full_scan_with(
        &ctx,
        &fixture.project,
        ApplyOptions {
            allow_mass_delete: true,
        },
    );
    assert_eq!(
        ctx.guard.of(fixture.project.id),
        None,
        "the deletion the human authorized clears the trip"
    );
    assert_eq!(fixture.indexed_paths().len(), 10);
}

/// A pass accounts for what it declined to walk.
///
/// The failure this closes is not a wrong index — it is a silent one. A
/// workspace whose corpus sits behind directory junctions indexes its loose
/// files and, in every number the summary reports, is indistinguishable from a
/// project that genuinely has three files in it. `links_skipped` is the
/// difference, and it is what makes an unchanged `.loreignore` edit explicable
/// instead of mysterious.
#[test]
fn a_pass_reports_the_links_it_did_not_follow() {
    let corpus = tempfile::tempdir().unwrap();
    let corpus = camino::Utf8Path::from_path(corpus.path()).unwrap();
    std::fs::write(corpus.join("behind.md"), "# not indexed").unwrap();

    let fixture = Fixture::new("demo");
    fixture.write("loose.md", "# the only real file");
    link_dir(&fixture.root.join("corpus"), corpus);

    let summary = full_scan(&fixture.context(), &fixture.project);

    assert_eq!(summary.links_skipped, 1, "{summary:?}");
    assert_eq!(summary.seen, 1, "only the loose file was ever considered");
    assert_eq!(fixture.indexed_paths(), ["loose.md"]);
}

/// And reports none when there are none — a count that is always non-zero is
/// not a signal.
#[test]
fn a_pass_over_a_tree_without_links_reports_none() {
    let fixture = Fixture::new("demo");
    populate_standard_tree(&fixture);
    let summary = full_scan(&fixture.context(), &fixture.project);
    assert_eq!(summary.links_skipped, 0, "{summary:?}");
}

// ---------------------------------------------------------------------------
// Links: the incremental path must agree with the full scan (D-0021)
//
// The nasty case this seam exists to kill. The watcher is deliberately dumb —
// it names paths no walk produced and does no IO of its own — so a path
// *behind* a link reaches the indexer looking like any other path, and
// `std::fs::metadata` resolves it perfectly happily. If the incremental path
// indexed it, the next full scan would delete it, and the pair would fight
// forever.
// ---------------------------------------------------------------------------

/// The named case: a watcher batch naming paths behind a link indexes nothing.
///
/// Both shapes a debounced batch can produce — the link itself (a directory
/// event) and a file underneath it — because the watcher cannot tell them
/// apart and neither may get through.
#[test]
fn a_watcher_batch_naming_a_path_behind_a_link_indexes_nothing() {
    let corpus = tempfile::tempdir().unwrap();
    let corpus = camino::Utf8Path::from_path(corpus.path()).unwrap();
    std::fs::write(corpus.join("behind.md"), "# would be a duplicate").unwrap();

    let fixture = Fixture::new("demo");
    fixture.write("loose.md", "# the only real file");
    link_dir(&fixture.root.join("corpus"), corpus);
    // The fixture is not vacuous: the path really does resolve to content.
    assert!(fixture.root.join("corpus/behind.md").is_file());

    let summary = index_paths(
        &fixture.context(),
        &fixture.project,
        &paths(&["corpus", "corpus/behind.md"]),
    );

    assert_eq!(summary.indexed, 0, "{summary:?}");
    assert_eq!(summary.errors, 0, "refusing is not an error condition");
    assert!(
        fixture.indexed_paths().is_empty(),
        "{:?}",
        fixture.indexed_paths()
    );

    // And the full scan agrees, which is the whole property: one evaluator,
    // two entry points, one verdict.
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(fixture.indexed_paths(), ["loose.md"]);
}

/// A link appearing where real content used to be reconciles the stale rows
/// rather than orphaning them.
///
/// This is the other half of "process the event, do not traverse the target":
/// the batch is handled normally, the walker refuses the new link, the paths
/// are absent from the micro-manifest — and being in its scope, they are
/// deleted. An index that merely *stopped updating* those rows would keep
/// serving content that is no longer part of the project.
#[test]
fn a_link_dropped_over_indexed_content_prunes_it() {
    let elsewhere = tempfile::tempdir().unwrap();
    let elsewhere = camino::Utf8Path::from_path(elsewhere.path()).unwrap();
    std::fs::write(elsewhere.join("other.md"), "# somebody else's tree").unwrap();

    let fixture = Fixture::new("demo");
    fixture.write("loose.md", "# stays");
    fixture.write("corpus/real.md", "# genuinely part of the project");
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(fixture.indexed_paths(), ["corpus/real.md", "loose.md"]);

    // Swap the real directory for a junction of the same name.
    fixture.remove_dir("corpus");
    link_dir(&fixture.root.join("corpus"), elsewhere);

    let summary = index_paths(&fixture.context(), &fixture.project, &paths(&["corpus"]));

    assert_eq!(summary.removed, 1, "{summary:?}");
    assert_eq!(summary.indexed, 0, "and nothing from the new target");
    assert_eq!(fixture.indexed_paths(), ["loose.md"]);
}
