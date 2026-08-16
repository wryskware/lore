//! Authority laundering, adversarially, through the **whole** query path
//! (adversarial review S1#2).
//!
//! The policy function in `lore::authority` has its own unit tests, and the
//! store has tests for what it stamps. Neither proves the thing the review
//! actually complained about, which is a property of `search`: that a document
//! cannot talk its way into the top ranking multiplier. So everything here
//! goes disk → indexer → store → `daemon::search::execute`, and asserts on the
//! ranked wire results a real agent would receive.
//!
//! Two laundering vectors are covered, because they are different mechanisms:
//!
//! 1. **Declaring** `decided` in frontmatter with citations that are missing,
//!    invented, unaccepted or retired. Canon says `decided` "must cite at
//!    least one active decision ID" (`0_Canon/README.md`), so each of these is
//!    a claim Lore must refuse — visibly, with a note, not silently.
//! 2. **Quoting** decision numbers in the body of scratch material. The review
//!    corpus was itself the exploit: session notes quote `D-NNNN` constantly,
//!    and used to be lifted to the `leaning` multiplier for doing so. The
//!    adversarial form of that test is a scratch note that is *lexically
//!    better* than an ordinary document and must still lose.

mod daemon_support;

use daemon_support::Fixture;
use lore::daemon::index::full_scan;
use lore::daemon::search::execute;
use lore::store::SearchFilter;
use lore_core::{SearchRequest, SearchResult};

const LEDGER: &str = "design/0_Canon/DECISIONS.md";

/// D-0001 and D-0004 active; D-0002 retired by D-0004; D-0003 merely proposed.
fn ledger_body() -> &'static str {
    "# Lore Decision Ledger\n\n\
     ## D-0001 — Vault authority model\n\n\
     - **Status:** Accepted\n- **Supersedes:** None\n\n\
     ## D-0002 — Retired by D-0004\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
     ## D-0003 — Proposed only\n\n- **Status:** Proposed\n- **Supersedes:** None\n\n\
     ## D-0004 — Replaces D-0002\n\n- **Status:** Accepted\n- **Supersedes:** D-0002\n"
}

fn doc(status: Option<&str>, refs: Option<&str>, body: &str) -> String {
    let mut front = String::from("---\n");
    if let Some(status) = status {
        front.push_str(&format!("design_status: {status}\n"));
    }
    if let Some(refs) = refs {
        front.push_str(&format!("decision_refs: [{refs}]\n"));
    }
    front.push_str("---\n\n# Ranking\n\n");
    front.push_str(body);
    front.push('\n');
    front
}

/// Run a real search through the daemon's own executor, lexical-only.
fn search(fixture: &Fixture, query: &str) -> Vec<SearchResult> {
    let request = SearchRequest {
        query: query.to_string(),
        project: Some(fixture.project.name.clone()),
        limit: Some(50),
        ..SearchRequest::default()
    };
    fixture
        .store
        .blocking(move |store| execute(store, &request, None))
        .expect("search executes")
        .results
}

fn find<'a>(results: &'a [SearchResult], path: &str) -> &'a SearchResult {
    results
        .iter()
        .find(|result| result.path == path)
        .unwrap_or_else(|| {
            let seen: Vec<&str> = results.iter().map(|r| r.path.as_str()).collect();
            panic!("{path} is missing from the results: {seen:?}")
        })
}

fn rank_of(results: &[SearchResult], path: &str) -> usize {
    results
        .iter()
        .position(|result| result.path == path)
        .unwrap_or_else(|| panic!("{path} is missing from the results"))
}

/// Every way of declaring `decided` without an active citation, side by side,
/// judged through the search path rather than the policy function.
///
/// The assertion that matters is not "the tier is 1" — it is that no forged
/// declaration ends up carrying the same `effective_authority` as the one
/// honest document, and that each one says *why* on the wire. An agent that
/// cannot see the refusal will believe the declaration.
#[test]
fn a_decided_declaration_cannot_reach_the_top_tier_without_an_active_citation() {
    let fixture = Fixture::new("laundering");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/1_Architecture/1.1.md",
        doc(Some("decided"), Some("D-0001"), "Ranking is defined here."),
    );
    fixture.write(
        "design/invented.md",
        doc(
            Some("decided"),
            Some("D-9999"),
            "Ranking, allegedly decided.",
        ),
    );
    fixture.write(
        "design/retired.md",
        doc(
            Some("decided"),
            Some("D-0002"),
            "Ranking, by a dead decision.",
        ),
    );
    fixture.write(
        "design/unpromoted.md",
        doc(
            Some("decided"),
            Some("D-0003"),
            "Ranking, by a draft entry.",
        ),
    );
    fixture.write(
        "design/bare.md",
        doc(Some("decided"), None, "Ranking, citing nothing at all."),
    );
    // Citing an active decision it does not *need* — an unclassified document
    // quoting canon buys nothing either.
    fixture.write(
        "design/quoter.md",
        doc(None, Some("D-0001"), "Ranking, quoting D-0001 in passing."),
    );

    full_scan(&fixture.context(), &fixture.project);
    let results = search(&fixture, "ranking");

    let honest = find(&results, "design/1_Architecture/1.1.md");
    assert_eq!(honest.effective_authority.as_deref(), Some("decided"));
    assert_eq!(honest.authority_note, None);

    for (path, why) in [
        ("design/invented.md", "the id does not exist"),
        ("design/retired.md", "the id was superseded"),
        ("design/unpromoted.md", "the entry is only proposed"),
        ("design/bare.md", "nothing is cited"),
    ] {
        let forged = find(&results, path);
        assert_eq!(
            forged.design_status.as_deref(),
            Some("decided"),
            "{path}: Lore never edits the declaration"
        );
        assert_eq!(
            forged.effective_authority.as_deref(),
            Some("neutral"),
            "{path}: {why}, so the claim must not be honored"
        );
        assert_eq!(
            forged.authority_note.as_deref(),
            Some("decided declared but cites no active decision"),
            "{path}: the refusal has to be visible on the wire"
        );
        assert!(
            forged.score < honest.score,
            "{path} ({}) must rank below validated canon ({})",
            forged.score,
            honest.score
        );
    }

    // Quoting canon is metadata, never authority.
    let quoter = find(&results, "design/quoter.md");
    assert_eq!(quoter.effective_authority.as_deref(), Some("neutral"));
    assert_eq!(
        quoter.authority_note, None,
        "nothing was declared to refuse"
    );
    assert_eq!(quoter.decision_refs, ["D-0001"], "still reported");
    assert!(quoter.score < honest.score);
}

/// The review's own exploit, reproduced: a `99_Scratch` session note that
/// quotes decision numbers densely *and* matches the query harder than the
/// ordinary document it is competing with.
///
/// It is deliberately arranged so the lexical arm prefers the scratch note —
/// asserted directly, so this test cannot pass for the boring reason that the
/// note was simply a worse match. Authority is what has to move it, and the
/// gap between adjacent RRF ranks is far smaller than the `99_Scratch`
/// multiplier, so a scratch note cannot buy back its cap with relevance.
#[test]
fn a_scratch_note_quoting_decisions_ranks_below_a_plainer_document_it_outmatches() {
    let fixture = Fixture::new("laundering");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/99_Scratch/session-notes.md",
        doc(
            None,
            None,
            "Ranking, ranking, ranking, ranking, ranking. Argued against D-0001, \
             D-0002, D-0003, D-0004 and D-0007 at length.",
        ),
    );
    fixture.write(
        "design/3_Retrieval/3.1.md",
        doc(Some("exploration"), None, "Ranking is discussed here."),
    );

    full_scan(&fixture.context(), &fixture.project);

    // Lexically, the scratch note wins — that is the premise of the exploit.
    let filter = SearchFilter::project(fixture.project.id);
    let lexical: Vec<String> = fixture
        .store
        .blocking(move |store| store.lexical_search("ranking", &filter, 50))
        .expect("lexical search")
        .into_iter()
        .map(|hit| hit.chunk.path.into_string())
        .collect();
    assert_eq!(
        lexical.first().map(String::as_str),
        Some("design/99_Scratch/session-notes.md"),
        "premise: BM25 prefers the scratch note ({lexical:?})"
    );

    // Ranked, it loses anyway.
    let results = search(&fixture, "ranking");
    assert!(
        rank_of(&results, "design/99_Scratch/session-notes.md")
            > rank_of(&results, "design/3_Retrieval/3.1.md"),
        "scratch material must not outrank ordinary exploration: {:?}",
        results
            .iter()
            .map(|r| (r.path.as_str(), r.score))
            .collect::<Vec<_>>()
    );

    let scratch = find(&results, "design/99_Scratch/session-notes.md");
    assert_eq!(scratch.effective_authority.as_deref(), Some("deprecated"));
    assert_eq!(
        scratch.authority_note.as_deref(),
        Some("99_Scratch path cap")
    );
    // The quoted numbers survive as metadata; they simply earn nothing.
    assert!(
        scratch.decision_refs.contains(&"D-0001".to_string()),
        "{:?}",
        scratch.decision_refs
    );
    // …and it is still findable. Demotion is not censorship.
    assert!(scratch.score > 0.0);
}

/// A `99_Scratch` note that *also* declares `decided` and cites real canon —
/// the maximal claim a scratch file can make — still lands at the bottom, and
/// reports the actionable half of the story rather than the path cap.
#[test]
fn a_scratch_note_declaring_validated_canon_is_still_capped_and_still_reported() {
    let fixture = Fixture::new("laundering");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/99_Scratch/claim.md",
        doc(
            Some("decided"),
            Some("D-0001"),
            "Ranking, self-declared canon.",
        ),
    );
    fixture.write(
        "design/99_Scratch/overclaim.md",
        doc(Some("decided"), None, "Ranking, self-declared and uncited."),
    );
    full_scan(&fixture.context(), &fixture.project);

    let results = search(&fixture, "ranking");

    let valid = find(&results, "design/99_Scratch/claim.md");
    assert_eq!(valid.effective_authority.as_deref(), Some("deprecated"));
    assert_eq!(
        valid.authority_note.as_deref(),
        Some("99_Scratch path cap"),
        "a valid citation does not lift scratch material"
    );

    // Two demotions apply at once. The tier takes the lower ceiling; the note
    // takes the one the author can act on.
    let over = find(&results, "design/99_Scratch/overclaim.md");
    assert_eq!(over.effective_authority.as_deref(), Some("deprecated"));
    assert_eq!(
        over.authority_note.as_deref(),
        Some("decided declared but cites no active decision")
    );

    // Only the false claim is a violation `status` should nag about.
    let status = fixture
        .store
        .blocking(|store| store.status())
        .expect("status");
    assert_eq!(status.projects[0].authority_violations, 1);
    assert_eq!(
        status.projects[0]
            .authority_violation_paths
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        ["design/99_Scratch/overclaim.md"]
    );
}

/// The ledger has to win against a document that quotes it, or "search for the
/// decision" returns the argument about the decision.
#[test]
fn the_ledger_outranks_the_scratch_note_arguing_about_it() {
    let fixture = Fixture::new("laundering");
    fixture.write(LEDGER, ledger_body());
    fixture.write(
        "design/99_Scratch/argument.md",
        doc(
            Some("decided"),
            Some("D-0004"),
            "Supersedes Supersedes Supersedes: why D-0004 replaces D-0002.",
        ),
    );
    full_scan(&fixture.context(), &fixture.project);

    let results = search(&fixture, "supersedes");
    assert!(
        rank_of(&results, LEDGER) < rank_of(&results, "design/99_Scratch/argument.md"),
        "{:?}",
        results
            .iter()
            .map(|r| (r.path.as_str(), r.score))
            .collect::<Vec<_>>()
    );
    let ledger = find(&results, LEDGER);
    assert_eq!(ledger.effective_authority.as_deref(), Some("decided"));
    assert_eq!(ledger.authority_note, None);
}
