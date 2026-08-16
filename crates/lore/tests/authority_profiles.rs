//! Authority is repository-opt-in (D-0012).
//!
//! `daemon_authority.rs` covers what the authority policy *does* once it is
//! switched on. This file covers the switch: what a repository gets when it
//! never committed a `.lore.toml`, what committing one turns on and what that
//! costs, how far each `behavior` is allowed to reach, and what a config Lore
//! cannot use does instead of failing quietly.
//!
//! The failure this whole dimension exists to prevent is a *silent* one — a
//! repository running an authority model its file does not describe — so the
//! assertions are deliberately about absence as much as presence: absent wire
//! fields, absent parsing, absent reordering, and an error that is impossible
//! to miss.

mod daemon_support;

use daemon_support::Fixture;
use lore::daemon::index::full_scan;
use lore::daemon::search;
use lore::repo_config::{Behavior, Profile, REPO_CONFIG_FILE, RepoAuthority};
use lore::store::{NewEmbedding, Project, ProjectStatus, SearchFilter};
use lore::types::ChunkId;
use lore_core::{SearchRequest, SearchResult};
use serde_json::Value;

const LEDGER: &str = "design/0_Canon/DECISIONS.md";
const CANON: &str = "design/1_Architecture/canon.md";
const SCRATCH: &str = "design/99_Scratch/notes.md";

const RANK: &str = "[authority]\nprofile = \"lore-v1\"\nbehavior = \"rank\"\n";
const ANNOTATE: &str = "[authority]\nprofile = \"lore-v1\"\nbehavior = \"annotate\"\n";
/// No `behavior` key at all, which D-0012 says means `annotate`.
const DEFAULTED: &str = "[authority]\nprofile = \"lore-v1\"\n";
const OFF: &str = "[authority]\nprofile = \"lore-v1\"\nbehavior = \"off\"\n";

/// A vault whose two documents make the same claim about themselves and are
/// judged very differently — but only by a repo that asked to be judged.
///
/// The scratch note is written to *win* on pure retrieval (shorter, and the
/// query term repeated), so authority and retrieval disagree about the order.
/// That disagreement is the whole instrument: `annotate` must resolve it the
/// way retrieval did, `rank` the way authority did.
fn populate_vault(fixture: &Fixture) {
    fixture.write(
        LEDGER,
        "# Ledger\n\n## D-0001 — Live canon\n\n\
         - **Status:** Accepted\n- **Supersedes:** None\n",
    );
    fixture.write(
        CANON,
        "---\ndesign_status: decided\ndecision_refs: [D-0001]\n---\n\n\
         # Canon\n\nThe widget lifecycle is owned by the daemon, and by the daemon alone, \
         for as long as the process lives.\n",
    );
    fixture.write(
        SCRATCH,
        "---\ndesign_status: decided\ndecision_refs: [D-0001]\n---\n\n\
         # Scratch\n\nWidget widget widget.\n",
    );
}

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        project: None,
        project_key: None,
        path_prefix: None,
        language: None,
        status: Vec::new(),
        sources: None,
        limit: Some(20),
    }
}

fn search(fixture: &Fixture, query: &str) -> Vec<SearchResult> {
    let request = request(query);
    fixture
        .store
        .blocking(move |store| search::execute(store, &request, None))
        .expect("search")
        .results
}

/// Result paths in ranked order — the only thing an ordering assertion needs.
fn ranked_paths(fixture: &Fixture, query: &str) -> Vec<String> {
    search(fixture, query)
        .into_iter()
        .map(|hit| hit.path)
        .collect()
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
/// path — the reader whose behavior the tier exists to change.
fn tiers_matching(fixture: &Fixture, path: &str, term: &str) -> Vec<u8> {
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

/// The two vault documents both carry the query term, so this covers them.
fn tiers(fixture: &Fixture, path: &str) -> Vec<u8> {
    tiers_matching(fixture, path, "widget")
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

/// Give every chunk a vector, so any later pass that re-created chunk rows
/// instead of updating them would visibly destroy embeddings.
fn embed_everything(fixture: &Fixture) -> usize {
    let ids = chunk_ids(fixture);
    let embeddings: Vec<NewEmbedding> = ids
        .iter()
        .map(|id| NewEmbedding {
            project: fixture.project.id,
            chunk_id: id.clone(),
            vector: vec![0.25, 0.5, 0.25, 0.5],
        })
        .collect();
    fixture
        .store
        .blocking(move |store| store.upsert_embeddings(&embeddings))
        .expect("store embeddings")
}

/// Register a second repository into `host`'s store, so one search response
/// spans two repos that configured themselves differently. `guest` is used
/// only for its directory tree and its `write` helper; its own store stays
/// empty, because the point is a *single* index holding both.
fn co_register(host: &Fixture, guest: &Fixture, name: &str) -> Project {
    let root = guest.root.clone();
    let display = name.to_string();
    let wanted = guest.root.clone();
    host.store
        .blocking(move |store| {
            store.register_project(&root, &display)?;
            store.list_projects()
        })
        .expect("register the second project")
        .into_iter()
        .find(|p| p.root == wanted)
        .expect("the second project is registered")
}

// ---------------------------------------------------------------------------
// 1. No `.lore.toml`: a fully neutral repository
// ---------------------------------------------------------------------------

/// The default has to be *nothing*. A repository that never adopted the vault
/// workflow must not acquire its meanings by accident: no frontmatter
/// parsing, no ledger, no path ceilings, and no authority fields on the wire
/// for a client to render as though a judgement had been made.
#[test]
fn an_unconfigured_repo_carries_no_authority_anywhere() {
    let fixture = Fixture::neutral("neutral");
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    let status = status(&fixture);
    assert_eq!(status.authority, RepoAuthority::default(), "{status:?}");
    assert_eq!(status.authority.profile, None, "no profile to report");
    assert_eq!(
        status.authority.error, None,
        "and nothing to complain about"
    );
    assert_eq!(status.decisions_active, 0, "the ledger was never parsed");
    assert_eq!(status.decisions_total, 0, "not even counted");
    assert_eq!(
        status.authority_violations, 0,
        "there was no declaration to refuse"
    );

    // Every file is unjudged, including the ledger and the scratch note whose
    // path spells `99_Scratch`.
    assert_eq!(tiers(&fixture, CANON), [1], "`decided` was never read");
    assert_eq!(tiers(&fixture, SCRATCH), [1], "and neither was the path");

    let results = search(&fixture, "widget");
    assert!(!results.is_empty(), "the corpus must be searchable");
    for hit in &results {
        let json = serde_json::to_value(hit).expect("a result serializes");
        let object = json.as_object().expect("an object");
        assert!(
            !object.contains_key("effective_authority"),
            "unjudged is not a tier: {json:#}"
        );
        assert!(
            !object.contains_key("authority_note"),
            "and there is no demotion to explain: {json:#}"
        );
        // These two stay on the wire always, so an old client keeps parsing.
        assert_eq!(
            object.get("design_status"),
            Some(&Value::Null),
            "always serialized, and null because nothing was parsed: {json:#}"
        );
        assert_eq!(
            object.get("decision_refs"),
            Some(&Value::Array(Vec::new())),
            "always serialized, and empty for the same reason: {json:#}"
        );
    }
}

/// The consequence of neutrality that a user would actually notice: with
/// nobody judging, a scratch note wins on retrieval alone. If this ever
/// ordered canon first, some part of the vault policy leaked into a repo that
/// never asked for it.
#[test]
fn a_scratch_note_can_outrank_canon_in_an_unconfigured_repo() {
    let fixture = Fixture::neutral("neutral");
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    let paths = ranked_paths(&fixture, "widget");
    assert_eq!(
        paths.first().map(String::as_str),
        Some(SCRATCH),
        "pure retrieval, and retrieval prefers the scratch note: {paths:?}"
    );
    assert!(paths.contains(&CANON.to_string()), "{paths:?}");
}

// ---------------------------------------------------------------------------
// 2. Opting in, settling, and opting back out
// ---------------------------------------------------------------------------

/// Committing the file to an already-indexed repository has to reach back
/// over everything already stored: the Markdown re-chunks under the new
/// profile, the ledger is parsed, and tiers appear on files nobody touched.
///
/// And it must cost only that. The profile changes how Markdown is
/// *interpreted*, not where it splits, so chunk ids are stable across the
/// flip — which is what keeps a flip from silently throwing away every vector
/// in the repo and re-embedding it.
#[test]
fn committing_a_profile_turns_authority_on_without_re_embedding() {
    let fixture = Fixture::neutral("optin");
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(tiers(&fixture, CANON), [1], "neutral to begin with");

    let ids_before = chunk_ids(&fixture);
    let embedded = embed_everything(&fixture);
    assert_eq!(embedded, ids_before.len());

    fixture.write(REPO_CONFIG_FILE, RANK);
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert!(summary.profile_changed, "{summary:?}");
    assert!(
        summary.indexed > 0,
        "the Markdown must be re-chunked under the new profile: {summary:?}"
    );
    assert_eq!(
        (summary.chunks_inserted, summary.chunks_deleted),
        (0, 0),
        "…and re-chunking must land on the same chunk ids: {summary:?}"
    );
    assert!(summary.chunks_kept > 0, "{summary:?}");

    let status = status(&fixture);
    assert_eq!(status.authority.profile, Some(Profile::LoreV1));
    assert_eq!(status.authority.behavior, Behavior::Rank);
    assert_eq!(status.decisions_active, 1, "the ledger is parsed now");
    assert_eq!(status.decisions_total, 1);
    assert_eq!(tiers(&fixture, CANON), [3], "a validated citation");
    assert_eq!(tiers(&fixture, SCRATCH), [0], "and a path ceiling");

    assert_eq!(
        chunk_ids(&fixture),
        ids_before,
        "a profile flip re-chunks; it must not re-identify"
    );
    assert_eq!(
        status.embedded_chunks as usize, embedded,
        "so every vector survives the flip"
    );
}

/// Once the profile has settled, refreshing again must be free. A pass that
/// re-chunked every Markdown file on every scan would make the profile tag a
/// permanent tax rather than a one-time migration.
#[test]
fn a_settled_profile_makes_the_next_refresh_a_no_op() {
    let fixture = Fixture::new("settled");
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    let summary = full_scan(&fixture.context(), &fixture.project);
    assert!(!summary.profile_changed, "{summary:?}");
    assert_eq!(summary.indexed, 0, "nothing was rewritten: {summary:?}");
    assert!(summary.unchanged > 0, "{summary:?}");
    assert_eq!(
        summary.authority_recomputed, 0,
        "and no tier moved: {summary:?}"
    );
}

/// Opting back out is the same switch in the other direction, and the
/// dangerous half: state parsed under a profile that is no longer declared
/// must not linger. A stale active-decision set would keep validating
/// citations in a repo that has stopped having decisions at all.
#[test]
fn removing_the_profile_reverts_the_repo_to_neutral() {
    let fixture = Fixture::new("optout");
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);
    assert_eq!(status(&fixture).decisions_active, 1);
    assert_eq!(tiers(&fixture, CANON), [3]);

    fixture.remove(REPO_CONFIG_FILE);
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert!(summary.profile_changed, "{summary:?}");
    let status = status(&fixture);
    assert_eq!(status.authority, RepoAuthority::default());
    assert_eq!(status.decisions_active, 0, "the parsed set is emptied");
    assert_eq!(status.decisions_total, 0);
    assert_eq!(status.authority_violations, 0);
    assert_eq!(tiers(&fixture, CANON), [1], "every tier flattens back");
    assert_eq!(tiers(&fixture, SCRATCH), [1]);

    let results = search(&fixture, "widget");
    for hit in &results {
        let json = serde_json::to_value(hit).expect("a result serializes");
        assert!(
            !json
                .as_object()
                .unwrap()
                .contains_key("effective_authority"),
            "the wire has to forget too: {json:#}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Behavior modes
// ---------------------------------------------------------------------------

/// The default half of D-0012, and the one whose value is best evidenced:
/// every label is computed and exposed, and *nothing* moves because of them.
///
/// The corpus is built so authority and retrieval disagree, and the check is
/// that `annotate` produces exactly the order an unconfigured repo produces
/// from the same bytes — not merely "some order".
#[test]
fn annotate_labels_every_result_but_leaves_the_order_to_retrieval() {
    let neutral = Fixture::neutral("neutral");
    populate_vault(&neutral);
    full_scan(&neutral.context(), &neutral.project);
    let pure_retrieval = ranked_paths(&neutral, "widget");

    let fixture = Fixture::neutral("annotating");
    fixture.write(REPO_CONFIG_FILE, ANNOTATE);
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    assert_eq!(
        ranked_paths(&fixture, "widget"),
        pure_retrieval,
        "annotate must not touch ordering"
    );

    // …while every judgement is still computed and reported. Including the
    // ledger's pin, which D-0012 keeps under `annotate` deliberately: the
    // label is a claim about what the document *is*, and suspending it would
    // make the annotation lie about the one file that defines canon.
    assert_eq!(tiers(&fixture, CANON), [3]);
    assert_eq!(tiers(&fixture, SCRATCH), [0]);
    assert_eq!(tiers_matching(&fixture, LEDGER, "Accepted"), [3]);
    assert_eq!(status(&fixture).decisions_active, 1);

    let by_path: Vec<(String, Option<String>, Option<String>)> = search(&fixture, "widget")
        .into_iter()
        .map(|hit| (hit.path, hit.effective_authority, hit.authority_note))
        .collect();
    let canon = by_path
        .iter()
        .find(|(path, ..)| path == CANON)
        .expect("canon is in the page");
    assert_eq!(canon.1.as_deref(), Some("decided"), "{by_path:?}");
    let scratch = by_path
        .iter()
        .find(|(path, ..)| path == SCRATCH)
        .expect("the scratch note is in the page");
    assert_eq!(scratch.1.as_deref(), Some("deprecated"), "{by_path:?}");
    assert!(
        scratch
            .2
            .as_deref()
            .is_some_and(|note| note.contains("99_Scratch")),
        "the demotion has to explain itself: {by_path:?}"
    );
}

/// `rank` is the other side of the same corpus: the identical bytes, ordered
/// by authority instead. Without this pairing, "annotate does not reorder"
/// could pass simply because nothing ever reorders.
#[test]
fn rank_reorders_the_same_corpus_that_annotate_leaves_alone() {
    let annotating = Fixture::neutral("annotating");
    annotating.write(REPO_CONFIG_FILE, ANNOTATE);
    populate_vault(&annotating);
    full_scan(&annotating.context(), &annotating.project);

    let ranking = Fixture::neutral("ranking");
    ranking.write(REPO_CONFIG_FILE, RANK);
    populate_vault(&ranking);
    full_scan(&ranking.context(), &ranking.project);

    let annotated = ranked_paths(&annotating, "widget");
    let ranked = ranked_paths(&ranking, "widget");
    assert_ne!(
        annotated, ranked,
        "the corpus must actually distinguish the two behaviors"
    );
    assert_eq!(
        ranked.first().map(String::as_str),
        Some(CANON),
        "rank puts validated canon over a better-matching scratch note: {ranked:?}"
    );
    assert_eq!(
        annotated.first().map(String::as_str),
        Some(SCRATCH),
        "annotate does not: {annotated:?}"
    );
}

/// D-0012 makes the conservative half the default, so an author who declares
/// a profile and stops gets labels — never a re-ranked index they did not ask
/// for. Defaulting the other way would be the expensive mistake.
#[test]
fn an_absent_behavior_key_annotates() {
    let fixture = Fixture::neutral("defaulted");
    fixture.write(REPO_CONFIG_FILE, DEFAULTED);
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    let status = status(&fixture);
    assert_eq!(status.authority.profile, Some(Profile::LoreV1));
    assert_eq!(status.authority.behavior, Behavior::Annotate);
    assert_eq!(tiers(&fixture, CANON), [3], "metadata is computed");
    assert_eq!(
        ranked_paths(&fixture, "widget").first().map(String::as_str),
        Some(SCRATCH),
        "and ordering is untouched"
    );
}

/// `off` is not the same as no file: the declaration stays visible so an
/// author can tell "suspended" from "never configured", which is the whole
/// reason the mode exists rather than asking people to delete the file.
#[test]
fn off_reports_the_profile_while_indexing_like_an_unconfigured_repo() {
    let fixture = Fixture::neutral("suspended");
    fixture.write(REPO_CONFIG_FILE, OFF);
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);

    let status = status(&fixture);
    assert_eq!(
        status.authority.profile,
        Some(Profile::LoreV1),
        "status still shows the declaration"
    );
    assert_eq!(status.authority.behavior, Behavior::Off);
    assert_eq!(status.authority.error, None, "`off` is not a mistake");

    // …and nothing else happened at all.
    assert_eq!(status.decisions_active, 0, "the ledger is not parsed");
    assert_eq!(status.decisions_total, 0);
    assert_eq!(tiers(&fixture, CANON), [1]);
    assert_eq!(tiers(&fixture, SCRATCH), [1]);
    assert_eq!(
        ranked_paths(&fixture, "widget").first().map(String::as_str),
        Some(SCRATCH)
    );
    for hit in search(&fixture, "widget") {
        let json = serde_json::to_value(&hit).expect("a result serializes");
        assert!(
            !json
                .as_object()
                .unwrap()
                .contains_key("effective_authority"),
            "a suspended profile judges nothing: {json:#}"
        );
    }
}

/// Authority weights are per-corpus, so one repository's decision to be
/// re-ranked must not follow its results into a response that also contains
/// somebody else's. A globally applied multiplier would silently impose a
/// vault's conventions on every other repo the user registered.
#[test]
fn one_repos_rank_does_not_re_weight_another_repos_results() {
    let ranking = Fixture::new("ranking");
    populate_vault(&ranking);
    full_scan(&ranking.context(), &ranking.project);

    // A second repo in the *same* index, annotating rather than ranking.
    let annotating = Fixture::neutral("annotating");
    annotating.write(REPO_CONFIG_FILE, ANNOTATE);
    populate_vault(&annotating);
    let guest = co_register(&ranking, &annotating, "annotating");
    full_scan(&ranking.context(), &guest);

    let results = search(&ranking, "widget");
    let order_within = |project: &str| -> Vec<String> {
        results
            .iter()
            .filter(|hit| hit.project == project)
            .map(|hit| hit.path.clone())
            .collect()
    };

    let ranked = order_within("ranking");
    let annotated = order_within("annotating");
    assert_eq!(
        ranked.first().map(String::as_str),
        Some(CANON),
        "the repo that asked to be ranked is: {results:#?}"
    );
    assert_eq!(
        annotated.first().map(String::as_str),
        Some(SCRATCH),
        "the one that did not, is not: {results:#?}"
    );

    // Both repos still label, which is what makes this a weighting difference
    // rather than one of them simply not being judged.
    for hit in &results {
        assert!(
            hit.effective_authority.is_some(),
            "both repos annotate: {hit:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. A config Lore cannot use
// ---------------------------------------------------------------------------

/// Every way of getting the file wrong lands in the same place: the repo
/// indexes exactly as an unconfigured one *and* carries an error a human will
/// see. The banned outcome is the quiet one — a repo running some other
/// authority model, or none, with nothing to say why.
#[test]
fn an_unusable_config_indexes_neutrally_and_says_so() {
    for (label, text, expected) in [
        (
            "an unknown profile",
            "[authority]\nprofile = \"adr\"\n",
            "adr",
        ),
        (
            "a misspelled key",
            "[authority]\nprofile = \"lore-v1\"\nbehaviour = \"rank\"\n",
            "behaviour",
        ),
        (
            "malformed TOML",
            "[authority\nprofile = \"lore-v1\"\n",
            ".lore.toml",
        ),
    ] {
        let fixture = Fixture::neutral("broken");
        fixture.write(REPO_CONFIG_FILE, text);
        populate_vault(&fixture);
        let summary = full_scan(&fixture.context(), &fixture.project);

        assert!(summary.config_error, "{label}: {summary:?}");
        let status = status(&fixture);
        assert_eq!(status.authority.profile, None, "{label} indexes neutrally");
        let error = status
            .authority
            .error
            .as_deref()
            .unwrap_or_else(|| panic!("{label} must be visible in status"));
        assert!(
            error.contains(expected),
            "{label}: the error must name the problem, got {error:?}"
        );

        // Neutral is not a figure of speech: nothing was parsed or capped.
        assert_eq!(status.decisions_active, 0, "{label}");
        assert_eq!(status.decisions_total, 0, "{label}");
        assert_eq!(tiers(&fixture, CANON), [1], "{label}");
        assert_eq!(tiers(&fixture, SCRATCH), [1], "{label}");
        assert_eq!(
            ranked_paths(&fixture, "widget").first().map(String::as_str),
            Some(SCRATCH),
            "{label}: ordering is pure retrieval too"
        );
    }
}

/// The error is a property of the current file, not a scar. Repairing the
/// config has to both clear the complaint and actually apply the profile —
/// otherwise the only remedy for a typo would be re-registering the project.
#[test]
fn repairing_the_config_clears_the_error_and_applies_the_profile() {
    let fixture = Fixture::neutral("repaired");
    fixture.write(REPO_CONFIG_FILE, "[authority]\nprofile = \"lore-v2\"\n");
    populate_vault(&fixture);
    full_scan(&fixture.context(), &fixture.project);
    assert!(status(&fixture).authority.error.is_some());

    fixture.write(REPO_CONFIG_FILE, RANK);
    let summary = full_scan(&fixture.context(), &fixture.project);

    assert!(!summary.config_error, "{summary:?}");
    assert!(
        summary.profile_changed,
        "a repaired config is a profile change: {summary:?}"
    );
    let status = status(&fixture);
    assert_eq!(status.authority.error, None, "the complaint is gone");
    assert_eq!(status.authority.profile, Some(Profile::LoreV1));
    assert_eq!(status.decisions_active, 1, "and the profile took effect");
    assert_eq!(tiers(&fixture, CANON), [3]);
    assert_eq!(
        ranked_paths(&fixture, "widget").first().map(String::as_str),
        Some(CANON)
    );
}

/// A `.lore.toml` in a subdirectory is an ordinary file, not configuration
/// (D-0012 puts the config at the registered root). Honoring a nested one
/// would let any vendored dependency silently reconfigure the repo indexing
/// it — including into an error state.
#[test]
fn a_nested_lore_toml_is_not_configuration() {
    let fixture = Fixture::new("nested");
    populate_vault(&fixture);
    fixture.write("vendor/thing/.lore.toml", "not toml at [all");
    full_scan(&fixture.context(), &fixture.project);

    let status = status(&fixture);
    assert_eq!(status.authority.error, None, "{status:?}");
    assert_eq!(status.authority.profile, Some(Profile::LoreV1));
    assert_eq!(status.authority.behavior, Behavior::Rank);
    assert_eq!(tiers(&fixture, CANON), [3]);
}
