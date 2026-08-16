//! Per-file decision records (D-0013): `**/0_Canon/decisions/D-NNNN-<slug>.md`.
//!
//! `lore-v1` recognizes two shapes of the same thing, and the interesting
//! cases are all at the seam between them — a record superseding a ledger
//! entry, a ledger entry superseding a record, and the two disagreeing about
//! who owns an id. Identity is the load-bearing part: a record whose id is
//! ambiguous makes both `Supersedes: D-NNNN` and every document's citation of
//! it mean two things at once, so the rule is to believe neither and say so
//! rather than to guess.
//!
//! Everything here runs under a `lore-v1` profile, because without one there
//! are no decision records at all — the last test checks exactly that.

mod daemon_support;

use daemon_support::Fixture;
use lore::daemon::index::full_scan;
use lore::repo_config::REPO_CONFIG_FILE;
use lore::store::{ProjectStatus, SearchFilter};
use std::collections::BTreeSet;

const LEDGER: &str = "design/0_Canon/DECISIONS.md";
const RECORDS: &str = "design/0_Canon/decisions";

/// One per-file decision record, written where `lore-v1` looks for them.
fn record(fixture: &Fixture, file: &str, body: &str) -> String {
    let path = format!("{RECORDS}/{file}");
    fixture.write(&path, body);
    path
}

/// A document that declares `decided` and cites `refs`.
fn citing(fixture: &Fixture, path: &str, refs: &str) {
    fixture.write(
        path,
        format!(
            "---\ndesign_status: decided\ndecision_refs: [{refs}]\n---\n\n\
             # Citing\n\nA body that mentions the topic.\n"
        ),
    );
}

fn active(fixture: &Fixture) -> Vec<String> {
    let id = fixture.project.id;
    let set: BTreeSet<String> = fixture
        .store
        .blocking(move |store| store.active_decisions(id))
        .expect("active decisions");
    set.into_iter().collect()
}

fn status(fixture: &Fixture) -> ProjectStatus {
    fixture
        .store
        .blocking(|store| store.status())
        .expect("status")
        .projects
        .into_iter()
        .find(|p| p.project == fixture.project.id)
        .expect("the project is in status")
}

/// Distinct effective tiers stored for a file, read back through the search
/// path — the reader the tier exists to steer.
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

fn violation_paths(status: &ProjectStatus) -> Vec<String> {
    status
        .decision_violations
        .iter()
        .map(|v| v.path.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// The format itself
// ---------------------------------------------------------------------------

/// The base case: a vault with no mono ledger at all, only records. If this
/// did not work, D-0013's promise — that a vault can migrate at its own pace,
/// or start out per-file — would be empty.
#[test]
fn a_per_file_record_validates_a_document_that_cites_it() {
    let fixture = Fixture::new("records");
    record(
        &fixture,
        "D-0001-authority-model.md",
        "# D-0001 — Authority model\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    record(
        &fixture,
        "D-0002-deferred.md",
        "# D-0002 — Deferred\n\n- **Status:** Proposed\n- **Supersedes:** None\n",
    );
    citing(&fixture, "design/1_Architecture/honored.md", "D-0001");
    citing(&fixture, "design/1_Architecture/refused.md", "D-0002");
    full_scan(&fixture.context(), &fixture.project);

    assert_eq!(active(&fixture), ["D-0001"], "accepted, and not retired");
    let status = status(&fixture);
    assert_eq!(status.decisions_active, 1);
    assert_eq!(status.decisions_total, 2, "a proposed record still exists");
    assert!(status.decision_violations.is_empty(), "{status:?}");

    assert_eq!(
        tiers(&fixture, "design/1_Architecture/honored.md", "body"),
        [3]
    );
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/refused.md", "body"),
        [1],
        "citing a record that is only proposed is not citing canon"
    );
    assert_eq!(
        status.authority_violations, 1,
        "and the refused declaration is reported"
    );

    // The record is canon in a different file layout, so it gets canon's pin.
    assert_eq!(
        tiers(
            &fixture,
            &format!("{RECORDS}/D-0001-authority-model.md"),
            "Accepted"
        ),
        [3],
        "a per-file record carries the pinned ledger tier"
    );
}

/// The two formats are one corpus, not two, so supersession has to cross the
/// boundary in both directions. A vault half-migrated to per-file records
/// would otherwise resolve into two disjoint sets that each believe stale
/// canon is live.
#[test]
fn supersession_crosses_between_the_ledger_and_the_records() {
    let fixture = Fixture::new("crossing");
    fixture.write(
        LEDGER,
        "# Ledger\n\n\
         ## D-0001 — Old, retired by a record\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
         ## D-0004 — New, retires a record\n\n- **Status:** Accepted\n- **Supersedes:** D-0003\n",
    );
    record(
        &fixture,
        "D-0002-replacement.md",
        "# D-0002 — Replacement\n\n- **Status:** Accepted\n- **Supersedes:** D-0001\n",
    );
    record(
        &fixture,
        "D-0003-retired-by-the-ledger.md",
        "# D-0003 — Retired by the ledger\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    full_scan(&fixture.context(), &fixture.project);

    assert_eq!(
        active(&fixture),
        ["D-0002", "D-0004"],
        "a record retired a ledger entry, and a ledger entry retired a record"
    );
    let status = status(&fixture);
    assert_eq!(status.decisions_total, 4, "all four still exist");
    assert!(status.decision_violations.is_empty(), "{status:?}");
}

/// The filename is the identity (D-0013), so a heading that names a different
/// decision is a defect rather than a second opinion — and the failure has to
/// be *visible*, because the alternative is a record that quietly stops
/// backing every document citing it.
#[test]
fn a_record_whose_heading_names_another_decision_is_excluded_and_surfaced() {
    let fixture = Fixture::new("mismatch");
    let path = record(
        &fixture,
        "D-0003-copy-paste.md",
        "# D-0009 — Pasted from another record\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    citing(&fixture, "design/1_Architecture/orphan.md", "D-0003");
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert!(
        active(&fixture).is_empty(),
        "neither the filename's id nor the heading's is believed"
    );
    assert_eq!(summary.decision_violations, 1, "{summary:?}");

    let status = status(&fixture);
    assert_eq!(violation_paths(&status), [path], "{status:?}");
    let detail = &status.decision_violations[0].detail;
    assert!(
        detail.contains("D-0009") && detail.contains("D-0003"),
        "the report must name both ids so the fix is obvious: {detail:?}"
    );
    assert_eq!(
        status.decisions_total, 0,
        "an excluded record is a violation, not a decision"
    );
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/orphan.md", "body"),
        [1],
        "and the document that cited it is not validated"
    );
}

/// Only the *first* heading identifies a record. A record that quotes another
/// decision's heading further down is discussing it, not claiming to be it,
/// and flagging that would make ordinary prose a formatting trap — in the one
/// document format whose whole job is to discuss other decisions.
#[test]
fn a_quoted_decision_heading_deeper_in_a_record_is_not_a_mismatch() {
    let fixture = Fixture::new("quoting");
    record(
        &fixture,
        "D-0007-discusses-others.md",
        "# D-0007 — Discusses others\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
         ## Context\n\nThe entry we are replacing reads:\n\n\
         ## D-0008 — An older decision\n\nquoted verbatim for the record.\n",
    );
    // The same shape with a title-only first heading: the filename already
    // said which decision this is, so the heading scopes nothing and a later
    // `D-NNNN` heading is still just prose.
    record(
        &fixture,
        "D-0011-titled-by-subject.md",
        "# Retention policy\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
         ## D-0012 — Quoted in passing\n\nnot a claim of identity.\n",
    );
    full_scan(&fixture.context(), &fixture.project);

    assert_eq!(active(&fixture), ["D-0007", "D-0011"]);
    let status = status(&fixture);
    assert!(status.decision_violations.is_empty(), "{status:?}");
    assert_eq!(status.decisions_total, 2);
}

/// The `-<slug>` is required. Accepting the bare form would make
/// `D-0006.md` and `D-0006-authority.md` two records for one id purely by
/// accident — so a bare file is an ordinary document, not a broken record,
/// and must not be reported as one.
#[test]
fn a_bare_d_nnnn_file_is_not_a_decision_record() {
    let fixture = Fixture::new("bare");
    fixture.write(
        &format!("{RECORDS}/D-0006.md"),
        "# D-0006 — Looks like a record\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    full_scan(&fixture.context(), &fixture.project);

    assert!(
        active(&fixture).is_empty(),
        "a bare `D-NNNN.md` contributes no decision"
    );
    let status = status(&fixture);
    assert_eq!(status.decisions_total, 0);
    assert!(
        status.decision_violations.is_empty(),
        "and it is not a defect either, just a file: {status:?}"
    );
    assert_eq!(
        tiers(&fixture, &format!("{RECORDS}/D-0006.md"), "Accepted"),
        [1],
        "so it gets no pin — it is not canon"
    );
}

// ---------------------------------------------------------------------------
// Duplicate ids
// ---------------------------------------------------------------------------

/// The asymmetric half of the duplicate rule: the mono ledger wins and stays
/// active. Deciding in favour of the existing corpus is what keeps a
/// half-finished migration from *deactivating* live canon — the failure that
/// would silently demote every document in the vault.
///
/// The colliding record is written so that believing it would have a visible
/// consequence (it retires an unrelated, live decision). If D-0005 is still
/// active at the end, the record really was excluded rather than merged.
#[test]
fn a_record_colliding_with_a_ledger_entry_loses_to_the_ledger() {
    let fixture = Fixture::new("collision");
    fixture.write(
        LEDGER,
        "# Ledger\n\n\
         ## D-0001 — Held by the ledger\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
         ## D-0005 — Unrelated and live\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    let intruder = record(
        &fixture,
        "D-0001-second-claim.md",
        "# D-0001 — Second claim\n\n- **Status:** Accepted\n- **Supersedes:** D-0005\n",
    );
    citing(&fixture, "design/1_Architecture/cites.md", "D-0001");
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert_eq!(
        active(&fixture),
        ["D-0001", "D-0005"],
        "the ledger entry holds D-0001, and the excluded record retires nothing"
    );
    assert_eq!(summary.decision_violations, 1, "{summary:?}");

    let status = status(&fixture);
    assert_eq!(violation_paths(&status), [intruder], "{status:?}");
    assert!(
        status.decision_violations[0].detail.contains("D-0001"),
        "{status:?}"
    );
    assert_eq!(
        status.decisions_total, 2,
        "the excluded record is not a third decision"
    );
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/cites.md", "body"),
        [3],
        "the surviving ledger entry still validates its citations"
    );
}

/// The symmetric half: two records, no ledger entry, no principled winner.
/// Both are excluded and both are named — picking one by file order would
/// make canon depend on a directory listing.
#[test]
fn two_records_claiming_one_id_are_both_excluded() {
    let fixture = Fixture::new("twins");
    fixture.write(
        LEDGER,
        "# Ledger\n\n## D-0004 — Unrelated and live\n\n\
         - **Status:** Accepted\n- **Supersedes:** None\n",
    );
    let first = record(
        &fixture,
        "D-0005-first-claim.md",
        "# D-0005 — First claim\n\n- **Status:** Accepted\n- **Supersedes:** D-0004\n",
    );
    let second = record(
        &fixture,
        "D-0005-second-claim.md",
        "# D-0005 — Second claim\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    citing(&fixture, "design/1_Architecture/cites.md", "D-0005");
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert_eq!(
        active(&fixture),
        ["D-0004"],
        "neither twin is believed, so neither retires anything"
    );
    assert_eq!(summary.decision_violations, 2, "{summary:?}");

    let status = status(&fixture);
    let mut paths = violation_paths(&status);
    paths.sort();
    let mut expected = [first, second];
    expected.sort();
    assert_eq!(paths, expected, "both are named: {status:?}");
    assert_eq!(status.decisions_total, 1);
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/cites.md", "body"),
        [1],
        "a document citing an ambiguous id is not validated"
    );
}

// ---------------------------------------------------------------------------
// D-0010 grammar, on the new shape
// ---------------------------------------------------------------------------

/// D-0013 says records use the *identical* field grammar, which includes
/// D-0010: only a bare ID list retires anything. Qualified prose is a partial
/// supersession and the named entry stays live.
///
/// The failure directions are asymmetric (D-0010): under-retiring leaves
/// stale canon visible, over-retiring silently demotes valid canon and every
/// document citing it. So the four prose shapes that must retire *nothing*
/// are checked together with the one that must retire.
#[test]
fn d_0010_supersession_grammar_applies_to_per_file_records() {
    let fixture = Fixture::new("grammar");
    for id in ["D-0010", "D-0015", "D-0016", "D-0017"] {
        record(
            &fixture,
            &format!("{id}-target.md"),
            &format!("# {id} — Target\n\n- **Status:** Accepted\n- **Supersedes:** None\n"),
        );
    }
    // A bare ID list, decorated as a real ledger writes it. This one retires.
    record(
        &fixture,
        "D-0020-bare-list.md",
        "# D-0020 — Bare list\n\n- **Status:** Accepted\n- **Supersedes:** D-0010.\n",
    );
    // Negation, possessive, qualifier: partial supersessions, all inert.
    record(
        &fixture,
        "D-0021-negation.md",
        "# D-0021 — Negation\n\n- **Status:** Accepted\n\
         - **Supersedes:** None (extends D-0015)\n",
    );
    record(
        &fixture,
        "D-0022-possessive.md",
        "# D-0022 — Possessive\n\n- **Status:** Accepted\n\
         - **Supersedes:** D-0016's schema clause only\n",
    );
    record(
        &fixture,
        "D-0023-qualifier.md",
        "# D-0023 — Qualifier\n\n- **Status:** Accepted\n\
         - **Supersedes:** D-0017 in part\n",
    );
    full_scan(&fixture.context(), &fixture.project);

    assert_eq!(
        active(&fixture),
        [
            "D-0015", "D-0016", "D-0017", "D-0020", "D-0021", "D-0022", "D-0023"
        ],
        "only the bare list retired its target (D-0010)"
    );
    assert!(
        status(&fixture).decision_violations.is_empty(),
        "partial prose is legal, not a defect"
    );
}

/// A record is only a record under a profile that recognizes the format
/// (D-0012). Without one, `0_Canon/decisions/D-0001-slug.md` is a Markdown
/// file that happens to be named after a decision — it must not be parsed, it
/// must not be pinned, and it must not put anything in the decision set.
#[test]
fn per_file_records_are_inert_in_a_repo_with_no_profile() {
    let fixture = Fixture::new("inert");
    record(
        &fixture,
        "D-0001-authority-model.md",
        "# D-0001 — Authority model\n\n- **Status:** Accepted\n- **Supersedes:** None\n",
    );
    citing(&fixture, "design/1_Architecture/honored.md", "D-0001");
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(active(&fixture), ["D-0001"], "with the profile, it counts");

    fixture.remove(REPO_CONFIG_FILE);
    full_scan(&fixture.context(), &fixture.project);

    assert!(active(&fixture).is_empty(), "without it, nothing is parsed");
    let status = status(&fixture);
    assert_eq!(status.decisions_total, 0);
    assert!(status.decision_violations.is_empty(), "{status:?}");
    assert_eq!(
        tiers(
            &fixture,
            &format!("{RECORDS}/D-0001-authority-model.md"),
            "Accepted"
        ),
        [1],
        "no pin for a repo that never asked for one"
    );
    assert_eq!(
        tiers(&fixture, "design/1_Architecture/honored.md", "body"),
        [1]
    );
}
