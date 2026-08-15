//! Effective authority end to end: what the indexer stamps, when the ledger
//! re-triggers a recompute, and what that recompute is allowed to disturb.
//!
//! The last point is the load-bearing one. Effective tier is derived entirely
//! from persisted state, which is what makes a ledger edit a metadata pass
//! rather than a re-index — so the tests assert not only that the tiers move,
//! but that chunk ids and embeddings do *not*.

mod daemon_support;

use daemon_support::Fixture;
use lore::daemon::index::{full_scan, index_paths};
use lore::store::{NewEmbedding, SearchFilter, StatusFilter};
use lore::types::{ChunkId, DesignStatus};
use std::collections::BTreeSet;

const LEDGER: &str = "design/0_Canon/DECISIONS.md";

/// A ledger with D-0001 accepted, D-0002 superseded by D-0004, D-0003 merely
/// proposed — the three states that decide what `decided` may cite.
fn ledger_body() -> &'static str {
    "# Lore Decision Ledger\n\n\
     ## D-0001 — Vault authority model\n\n\
     - **Status:** Accepted\n- **Supersedes:** None\n\n\
     ## D-0002 — Retired\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
     ## D-0003 — Proposed only\n\n- **Status:** Proposed\n- **Supersedes:** None\n\n\
     ## D-0004 — Replaces D-0002\n\n- **Status:** Accepted\n- **Supersedes:** D-0002\n"
}

fn doc(status: &str, refs: &str, body: &str) -> String {
    format!("---\ndesign_status: {status}\n{refs}---\n\n# Heading\n\n{body}\n")
}

/// Effective tier stored for every chunk of a file (deduplicated). Read back
/// through the search path on purpose: that is the reader whose behavior the
/// tier is supposed to change.
fn tiers(fixture: &Fixture, path: &str, term: &str) -> Vec<u8> {
    let filter = SearchFilter {
        project: Some(fixture.project.id),
        path_prefix: Some(path.to_string()),
        ..SearchFilter::default()
    };
    let query = term.to_string();
    let mut found: Vec<u8> = fixture
        .store
        .blocking(move |store| store.lexical_search(&query, &filter, 100))
        .expect("search")
        .into_iter()
        .map(|hit| hit.authority.tier)
        .collect();
    assert!(!found.is_empty(), "no chunk of {path} matched `{term}`");
    found.sort_unstable();
    found.dedup();
    found
}

fn chunk_ids(fixture: &Fixture) -> Vec<ChunkId> {
    let id = fixture.project.id;
    let files = fixture
        .store
        .blocking(move |store| store.list_files(id))
        .expect("list files");
    let mut ids: Vec<ChunkId> = Vec::new();
    for record in files {
        let path = record.path.clone();
        ids.extend(
            fixture
                .store
                .blocking(move |store| store.get_file_chunks(id, &path))
                .expect("file chunks")
                .into_iter()
                .map(|chunk| chunk.id),
        );
    }
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    ids
}

fn active(fixture: &Fixture) -> BTreeSet<String> {
    let id = fixture.project.id;
    fixture
        .store
        .blocking(move |store| store.active_decisions(id))
        .expect("active decisions")
}

/// The whole policy, through the real indexer rather than the pure function.
#[test]
fn a_full_scan_stamps_the_policy_onto_every_file() {
    let fixture = Fixture::new("authority");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/1_Architecture/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "Validated body"),
    );
    fixture.write(
        "design/1_Architecture/1.2.md",
        doc("decided", "decision_refs: [D-0002]\n", "Superseded body"),
    );
    fixture.write(
        "design/1_Architecture/1.3.md",
        doc("decided", "", "Bare body"),
    );
    fixture.write(
        "design/9_Scratch/notes.md",
        doc(
            "decided",
            "decision_refs: [D-0001]\n",
            "Scratch body about D-0001",
        ),
    );
    fixture.write(
        "design/7_Research/report.md",
        doc("decided", "decision_refs: [D-0001]\n", "Research body"),
    );
    fixture.write(
        "design/3_Retrieval/3.1.md",
        doc("leaning", "", "Leaning body"),
    );

    full_scan(&fixture.context(), &fixture.project);

    assert_eq!(
        active(&fixture)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["D-0001", "D-0004"],
        "accepted and not superseded"
    );
    assert_eq!(
        tiers(&fixture, LEDGER, "Status"),
        [3],
        "the ledger is pinned"
    );
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/1.1.md", "Heading"),
        [3]
    );
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/1.2.md", "Heading"),
        [1],
        "citing a superseded decision is not citing an active one"
    );
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/1.3.md", "Heading"),
        [1]
    );
    assert_eq!(
        tiers(&fixture, "design/9_Scratch/notes.md", "Heading"),
        [0],
        "a valid citation does not lift scratch material"
    );
    assert_eq!(
        tiers(&fixture, "design/7_Research/report.md", "Heading"),
        [1]
    );
    assert_eq!(tiers(&fixture, "design/3_Retrieval/3.1.md", "Heading"), [2]);
}

/// `status` is where a human learns their `decided` documents were refused.
#[test]
fn status_counts_and_names_the_refused_declarations() {
    let fixture = Fixture::new("authority");
    fixture.write(LEDGER, ledger_body());
    fixture.write("design/a.md", doc("decided", "", "A body"));
    fixture.write("design/b.md", doc("decided", "", "B body"));
    fixture.write(
        "design/ok.md",
        doc("decided", "decision_refs: [D-0004]\n", "Fine body"),
    );
    // Capped by path, not a false claim: policy, so not a violation.
    fixture.write("design/9_Scratch/c.md", doc("leaning", "", "C body"));

    full_scan(&fixture.context(), &fixture.project);

    let status = fixture
        .store
        .blocking(|store| store.status())
        .expect("status");
    let project = &status.projects[0];
    assert_eq!(project.authority_violations, 2);
    assert_eq!(
        project
            .authority_violation_paths
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        ["design/a.md", "design/b.md"]
    );
}

/// Editing the ledger changes what `decided` is allowed to mean across the
/// whole project, including in files that did not change. An incremental pass
/// naming only the ledger must therefore re-derive everything — and must not
/// pay for a re-index to do it.
#[test]
fn a_ledger_edit_recomputes_the_project_without_disturbing_chunk_ids() {
    let fixture = Fixture::new("authority");
    fixture.write(
        LEDGER,
        "# Ledger\n\n## D-0001 — First\n\n- **Status:** Proposed\n",
    );
    fixture.write(
        "design/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "Body of one"),
    );
    full_scan(&fixture.context(), &fixture.project);
    assert!(active(&fixture).is_empty(), "nothing accepted yet");
    assert_eq!(tiers(&fixture, "design/1.1.md", "Heading"), [1]);

    let ids_before = chunk_ids(&fixture);
    // Give every chunk a vector, so a recompute that re-wrote rows instead of
    // updating them would visibly destroy embeddings.
    let embeddings: Vec<NewEmbedding> = ids_before
        .iter()
        .map(|id| NewEmbedding {
            project: fixture.project.id,
            chunk_id: id.clone(),
            vector: vec![0.5, 0.5, 0.5, 0.5],
        })
        .collect();
    let stored = fixture
        .store
        .blocking(move |store| store.upsert_embeddings(&embeddings))
        .expect("store embeddings");
    assert_eq!(stored, ids_before.len());

    // The ledger accepts D-0001. Only the ledger file changed on disk.
    fixture.write(
        LEDGER,
        "# Ledger\n\n## D-0001 — First\n\n- **Status:** Accepted\n",
    );
    let batch: BTreeSet<camino::Utf8PathBuf> =
        [camino::Utf8PathBuf::from(LEDGER)].into_iter().collect();
    let summary = index_paths(&fixture.context(), &fixture.project, &batch);

    assert_eq!(
        active(&fixture)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["D-0001"]
    );
    assert_eq!(
        tiers(&fixture, "design/1.1.md", "Heading"),
        [3],
        "an untouched file is re-derived from the new active set"
    );
    assert!(summary.authority_recomputed > 0, "{summary:?}");
    assert_eq!(summary.authority_violations, 0, "{summary:?}");

    // The ledger's own chunks changed (its bytes did); every other id, and
    // every vector, survived.
    let ids_after = chunk_ids(&fixture);
    let survived: Vec<&ChunkId> = ids_before
        .iter()
        .filter(|id| ids_after.contains(id))
        .collect();
    assert!(
        !survived.is_empty(),
        "the design document's chunks must be untouched"
    );
    let status = fixture
        .store
        .blocking(|store| store.status())
        .expect("status");
    assert_eq!(
        status.projects[0].embedded_chunks as usize,
        survived.len(),
        "a metadata recompute must not evict vectors"
    );
}

/// An incremental batch that does not name a ledger must not pay for a
/// project-wide re-derivation; new files still get the right tier because the
/// store stamps it on write.
#[test]
fn an_ordinary_incremental_pass_does_not_recompute() {
    let fixture = Fixture::new("authority");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "One"),
    );
    full_scan(&fixture.context(), &fixture.project);

    fixture.write("design/1.2.md", doc("decided", "", "Two"));
    let batch: BTreeSet<camino::Utf8PathBuf> = [camino::Utf8PathBuf::from("design/1.2.md")]
        .into_iter()
        .collect();
    let summary = index_paths(&fixture.context(), &fixture.project, &batch);

    assert_eq!(summary.authority_recomputed, 0, "{summary:?}");
    assert_eq!(
        summary.authority_violations, 0,
        "the recompute never ran, so it counted nothing"
    );
    // …but the new file was still classified correctly on write.
    assert_eq!(tiers(&fixture, "design/1.2.md", "Heading"), [1]);
    assert_eq!(tiers(&fixture, "design/1.1.md", "Heading"), [3]);
    let status = fixture
        .store
        .blocking(|store| store.status())
        .expect("status");
    assert_eq!(
        status.projects[0].authority_violations, 1,
        "and `status` sees it regardless of which pass wrote it"
    );
}

/// Deleting the ledger empties the active set, which demotes every `decided`
/// document that depended on it. Silently keeping the old set would leave the
/// index asserting canon that no longer exists.
#[test]
fn removing_the_ledger_demotes_everything_that_cited_it() {
    let fixture = Fixture::new("authority");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "One"),
    );
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(tiers(&fixture, "design/1.1.md", "Heading"), [3]);

    fixture.remove(LEDGER);
    let batch: BTreeSet<camino::Utf8PathBuf> =
        [camino::Utf8PathBuf::from(LEDGER)].into_iter().collect();
    index_paths(&fixture.context(), &fixture.project, &batch);

    assert!(active(&fixture).is_empty());
    assert_eq!(tiers(&fixture, "design/1.1.md", "Heading"), [1]);
}

/// The `min_authority` filter is a ranking-side floor, so it must read the
/// effective tier: filtering on the declaration would let scratch material
/// through exactly the door it was demoted away from.
#[test]
fn min_authority_filters_on_the_effective_tier() {
    let fixture = Fixture::new("authority");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/9_Scratch/notes.md",
        doc("decided", "decision_refs: [D-0001]\n", "Scratch body"),
    );
    fixture.write(
        "design/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "Real body"),
    );
    full_scan(&fixture.context(), &fixture.project);

    let filter = SearchFilter {
        project: Some(fixture.project.id),
        min_authority: Some(1),
        ..SearchFilter::default()
    };
    let paths: Vec<String> = fixture
        .store
        .blocking(move |store| store.lexical_search("body", &filter, 50))
        .expect("search")
        .into_iter()
        .map(|hit| hit.chunk.path.into_string())
        .collect();
    assert!(paths.contains(&"design/1.1.md".to_string()), "{paths:?}");
    assert!(
        !paths.iter().any(|path| path.contains("9_Scratch")),
        "{paths:?}"
    );
}

/// Paths for a search restricted to one file, by declared status.
fn paths_for(fixture: &Fixture, filter: SearchFilter, term: &str) -> Vec<String> {
    let term = term.to_string();
    let mut paths: Vec<String> = fixture
        .store
        .blocking(move |store| store.lexical_search(&term, &filter, 50))
        .expect("search")
        .into_iter()
        .map(|hit| hit.chunk.path.into_string())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The two filters answer two different questions and must not be conflated:
/// `min_authority` asks what Lore ranks a document as, `status` asks what the
/// document says about itself. A demoted `decided` file has to fall out of the
/// first and stay in the second — otherwise there is no way to *find* the
/// documents whose declarations were refused, which is exactly the audit an
/// author needs after `lore status` tells them there are some.
#[test]
fn min_authority_reads_the_effective_tier_while_status_still_reads_the_declaration() {
    let fixture = Fixture::new("authority");
    fixture.write(LEDGER, ledger_body());
    // Declares `decided`, cites a decision that exists but was superseded.
    fixture.write(
        "design/refused.md",
        doc("decided", "decision_refs: [D-0002]\n", "Shared body"),
    );
    fixture.write(
        "design/honored.md",
        doc("decided", "decision_refs: [D-0001]\n", "Shared body"),
    );
    full_scan(&fixture.context(), &fixture.project);

    let by_authority = paths_for(
        &fixture,
        SearchFilter {
            project: Some(fixture.project.id),
            min_authority: Some(3),
            ..SearchFilter::default()
        },
        "shared",
    );
    assert_eq!(
        by_authority,
        ["design/honored.md"],
        "min_authority=3 admits validated canon only"
    );

    let by_status = paths_for(
        &fixture,
        SearchFilter {
            project: Some(fixture.project.id),
            statuses: Some(vec![StatusFilter::Status(DesignStatus::Decided)]),
            ..SearchFilter::default()
        },
        "shared",
    );
    assert_eq!(
        by_status,
        ["design/honored.md", "design/refused.md"],
        "the refused document still declares `decided` and must stay findable"
    );

    // The two are composable, and composing them is the audit query: "what
    // claims canon but is not ranked as canon?".
    let refused_only = paths_for(
        &fixture,
        SearchFilter {
            project: Some(fixture.project.id),
            statuses: Some(vec![StatusFilter::Status(DesignStatus::Decided)]),
            min_authority: Some(0),
            ..SearchFilter::default()
        },
        "shared",
    );
    assert_eq!(refused_only, ["design/honored.md", "design/refused.md"]);
}

/// The ordering the daemon cannot control: documents are indexed first and the
/// ledger arrives later (a vault under construction, or a `git pull` the
/// watcher delivers as two batches). The pass that indexes the ledger owns
/// promoting everything that was waiting on it — otherwise a whole vault stays
/// demoted until someone forces a full rescan.
#[test]
fn a_ledger_that_appears_after_its_documents_promotes_them_on_that_pass() {
    let fixture = Fixture::new("authority");
    fixture.write(
        "design/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "Body of one"),
    );
    full_scan(&fixture.context(), &fixture.project);
    assert!(active(&fixture).is_empty(), "there is no ledger yet");
    assert_eq!(
        tiers(&fixture, "design/1.1.md", "Heading"),
        [1],
        "a citation with no ledger to validate it is not validated"
    );

    // The ledger arrives on its own, as its own watcher batch.
    fixture.write(LEDGER, ledger_body());
    let batch: BTreeSet<camino::Utf8PathBuf> =
        [camino::Utf8PathBuf::from(LEDGER)].into_iter().collect();
    let summary = index_paths(&fixture.context(), &fixture.project, &batch);

    assert_eq!(
        active(&fixture)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["D-0001", "D-0004"]
    );
    assert_eq!(
        tiers(&fixture, "design/1.1.md", "Heading"),
        [3],
        "the pass that indexed the ledger must promote what was waiting on it"
    );
    assert!(summary.authority_recomputed > 0, "{summary:?}");
    assert_eq!(tiers(&fixture, LEDGER, "Status"), [3], "and pin itself");
}

/// Supersession is how an append-only ledger retires canon, so it is also how
/// a document silently stops being backed by one. The citing document is not
/// edited and its declaration is untouched — only what Lore ranks it as moves.
#[test]
fn superseding_a_decision_demotes_the_documents_that_cited_it() {
    let fixture = Fixture::new("authority");
    fixture.write(
        LEDGER,
        "# Ledger\n\n## D-0001 — Original\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    fixture.write(
        "design/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "Body of one"),
    );
    fixture.write(
        "design/1.2.md",
        doc(
            "decided",
            "decision_refs: [D-0001, D-0005]\n",
            "Body of two",
        ),
    );
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(tiers(&fixture, "design/1.1.md", "Heading"), [3]);
    assert_eq!(tiers(&fixture, "design/1.2.md", "Heading"), [3]);

    // A new accepted entry retires D-0001 and introduces D-0005, which 1.2
    // already cited speculatively. One edit, opposite verdicts.
    fixture.write(
        LEDGER,
        "# Ledger\n\n## D-0001 — Original\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
         ## D-0005 — Replacement\n\n- **Status:** Accepted\n- **Supersedes:** D-0001\n",
    );
    let batch: BTreeSet<camino::Utf8PathBuf> =
        [camino::Utf8PathBuf::from(LEDGER)].into_iter().collect();
    let summary = index_paths(&fixture.context(), &fixture.project, &batch);

    assert_eq!(
        active(&fixture)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["D-0005"],
        "D-0001 is retired by an accepted successor"
    );
    assert_eq!(
        tiers(&fixture, "design/1.1.md", "Heading"),
        [1],
        "its only citation is no longer active"
    );
    assert_eq!(
        tiers(&fixture, "design/1.2.md", "Heading"),
        [3],
        "one surviving active citation is enough"
    );
    assert_eq!(summary.authority_violations, 1, "{summary:?}");

    // And a *proposed* successor cannot do the same thing — canon invites
    // agents to leave draft entries in the ledger.
    fixture.write(
        LEDGER,
        "# Ledger\n\n## D-0001 — Original\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
         ## D-0005 — Replacement\n\n- **Status:** Proposed\n- **Supersedes:** D-0001\n",
    );
    index_paths(&fixture.context(), &fixture.project, &batch);
    assert_eq!(
        active(&fixture)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["D-0001"]
    );
    assert_eq!(
        tiers(&fixture, "design/1.1.md", "Heading"),
        [3],
        "a draft may not deactivate live canon"
    );
}

/// Ledger recognition is by path convention, and this is a Windows-native tool
/// (D-0003): a vault checked out as `0_canon/decisions.md` — which Windows
/// treats as the same file — must not silently stop being the ledger, because
/// the failure mode is the entire vault quietly losing its canon.
#[cfg(windows)]
#[test]
fn a_case_varied_ledger_path_is_still_the_ledger_on_windows() {
    let fixture = Fixture::new("authority");
    fixture.write("design/0_canon/decisions.md", ledger_body());
    fixture.write(
        "design/1.1.md",
        doc("decided", "decision_refs: [D-0001]\n", "Body of one"),
    );
    full_scan(&fixture.context(), &fixture.project);

    assert_eq!(
        active(&fixture)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["D-0001", "D-0004"],
        "the ledger was parsed despite its casing"
    );
    assert_eq!(
        tiers(&fixture, "design/0_canon/decisions.md", "Status"),
        [3]
    );
    assert_eq!(tiers(&fixture, "design/1.1.md", "Heading"), [3]);

    // …and it re-triggers a recompute incrementally under that casing too.
    fixture.write(
        "design/0_canon/decisions.md",
        "# Ledger\n\n## D-0001 — Retired\n\n- **Status:** Proposed\n",
    );
    let batch: BTreeSet<camino::Utf8PathBuf> =
        [camino::Utf8PathBuf::from("design/0_canon/decisions.md")]
            .into_iter()
            .collect();
    index_paths(&fixture.context(), &fixture.project, &batch);
    assert!(active(&fixture).is_empty());
    assert_eq!(tiers(&fixture, "design/1.1.md", "Heading"), [1]);
}
