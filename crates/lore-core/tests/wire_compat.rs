//! Wire compatibility across the package-3 (authority + provenance) change.
//!
//! `lore` ships as one binary that is both daemon and client, but the two are
//! not upgraded atomically: a long-running daemon serves a freshly built CLI,
//! and `lore-mcp` is a separate process an editor may have started days ago.
//! The design note therefore specifies every new field as **additive**.
//!
//! "Additive" is asserted here in both directions, because each direction
//! breaks differently:
//!
//! - **old daemon → new client.** JSON with the new fields simply absent must
//!   still deserialize. Every added field carries `#[serde(default)]`; a
//!   missing `#[serde(default)]` is a hard parse failure, not a degradation,
//!   and would make the new CLI unable to talk to a running old daemon at all.
//! - **new daemon → old client.** Nothing pre-existing may be removed or
//!   renamed. That cannot be checked against the old code (it is gone), so the
//!   pre-change field set is pinned as a golden list, and the old response
//!   shapes are re-declared locally and parsed from new-daemon JSON. A rename
//!   passes a "is the new field there?" test and fails both of these.
//!
//! The golden lists are the serialized shapes as of commit `a128f07` (the
//! branch base), field for field.

use lore_core::{
    ExpandRequest, ProjectInfo, ProjectStatus, SearchRequest, SearchResult, WatchState,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Field names of a serialized struct, sorted.
fn keys<T: serde::Serialize>(value: &T) -> Vec<String> {
    match serde_json::to_value(value).expect("serializes") {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            keys
        }
        other => panic!("expected a JSON object, got {other}"),
    }
}

fn populated_search_result() -> SearchResult {
    SearchResult {
        chunk_id: "9f3a1c2b".into(),
        project: "lore".into(),
        project_key: "lore".into(),
        path: "design/9_Scratch/notes.md".into(),
        line_start: 15,
        line_end: 17,
        language: Some("markdown".into()),
        symbol_path: None,
        heading_path: Some(vec!["Notes".into()]),
        design_status: Some("decided".into()),
        effective_authority: Some("deprecated".into()),
        authority_note: Some("9_Scratch path cap".into()),
        decision_refs: vec!["D-0007".into()],
        score: 0.8741,
        excerpt: "body".into(),
        excerpt_truncated: false,
    }
}

fn populated_project_status() -> ProjectStatus {
    ProjectStatus {
        id: 1,
        name: "lore".into(),
        key: "lore".into(),
        root: r"C:\repos\lore".into(),
        kind: "repo".into(),
        files: 12,
        chunks: 40,
        embedded_chunks: 40,
        authority_violations: 2,
        authority_violation_paths: vec!["design/a.md".into(), "design/b.md".into()],
        authority_profile: Some("lore-v1".into()),
        authority_behavior: Some("rank".into()),
        // Populated even though a real project would not report a profile and
        // a config error at once: this fixture exists to pin the *maximal*
        // serialized shape, and a field left `None` is a field a
        // `skip_serializing_if` would hide from the golden list.
        authority_config_error: Some("unknown authority profile `adr`".into()),
        decisions_active: 11,
        decisions_total: 13,
        decision_violations: vec![
            "design/0_Canon/decisions/D-0004-a.md: duplicate decision id D-0004".into(),
        ],
        watch: WatchState::Armed,
    }
}

/// One wire type's serialized field set, against what it looked like before.
struct Shape {
    /// Type name, for the failure message.
    name: &'static str,
    /// Field names actually serialized today.
    actual: Vec<String>,
    /// Field names as of the branch base (`a128f07`).
    before: &'static [&'static str],
    /// Fields package 3 is specified to add, and nothing else.
    added: &'static [&'static str],
}

/// Nothing that existed before package 3 may have been removed or renamed, and
/// the additions are exactly the ones the design note specifies — no more.
#[test]
fn every_pre_existing_wire_field_survives_and_the_additions_are_the_specified_ones() {
    let cases = [
        Shape {
            name: "SearchResult",
            actual: keys(&populated_search_result()),
            before: &[
                "chunk_id",
                "decision_refs",
                "design_status",
                "excerpt",
                "excerpt_truncated",
                "heading_path",
                "language",
                "line_end",
                "line_start",
                "path",
                "project",
                "score",
                "symbol_path",
            ],
            added: &["authority_note", "effective_authority", "project_key"],
        },
        Shape {
            name: "ProjectStatus",
            actual: keys(&populated_project_status()),
            before: &[
                "chunks",
                "embedded_chunks",
                "files",
                "id",
                "name",
                "root",
                "watch",
            ],
            added: &[
                // Package 3 (authority + provenance).
                "authority_violation_paths",
                "authority_violations",
                "key",
                "kind",
                // D-0012/D-0013: which profile a repository opted into, how
                // healthy its config and its decision corpus are.
                "authority_behavior",
                "authority_config_error",
                "authority_profile",
                "decision_violations",
                "decisions_active",
                "decisions_total",
            ],
        },
        Shape {
            name: "ProjectInfo",
            actual: keys(&ProjectInfo {
                id: 1,
                name: "lore".into(),
                key: "lore".into(),
                root: r"C:\repos\lore".into(),
                kind: "repo".into(),
            }),
            before: &["id", "name", "root"],
            added: &["key", "kind"],
        },
        Shape {
            name: "ExpandRequest",
            actual: keys(&ExpandRequest {
                project: "lore".into(),
                project_key: Some("lore".into()),
                chunk_id: "9f3a1c2b".into(),
                context_lines: Some(10),
            }),
            before: &["chunk_id", "context_lines", "project"],
            added: &["project_key"],
        },
        Shape {
            name: "SearchRequest",
            actual: keys(&SearchRequest {
                query: "ranking".into(),
                project: Some("lore".into()),
                project_key: Some("lore".into()),
                path_prefix: None,
                language: None,
                status: vec!["decided".into()],
                sources: Some(vec!["repo".into()]),
                limit: Some(20),
            }),
            before: &[
                "language",
                "limit",
                "path_prefix",
                "project",
                "query",
                "status",
            ],
            // `project_key` mirrors `ExpandRequest`'s: additive in shape, even
            // though the *semantics* of `project` tightened at the same time
            // (a request naming neither is now refused). That tightening is
            // not expressible as a field, which is why it is a self-describing
            // 400 rather than an API_VERSION bump.
            added: &["project_key", "sources"],
        },
    ];

    for case in cases {
        let Shape {
            name,
            actual,
            before,
            added,
        } = case;
        for field in before {
            assert!(
                actual.iter().any(|key| key == field),
                "{name}.{field} was removed or renamed; clients built against \
                 the previous release read it: {actual:?}"
            );
        }
        let mut expected: Vec<&str> = before.iter().chain(added).copied().collect();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "{name} grew a field this test does not know about; if it is \
             intentional, add it to the additive list"
        );
    }
}

/// An old daemon's response has none of the new fields. The new client must
/// still parse it — degraded (empty key, empty effective authority), never
/// failed.
#[test]
fn a_response_from_a_daemon_that_predates_this_change_still_deserializes() {
    let old_result = json!({
        "chunk_id": "9f3a1c2b",
        "project": "lore",
        "path": "design/1.1.md",
        "line_start": 1,
        "line_end": 4,
        "language": "markdown",
        "symbol_path": null,
        "heading_path": ["Overview"],
        "design_status": "decided",
        "decision_refs": ["D-0007"],
        "score": 0.87,
        "excerpt": "body",
        "excerpt_truncated": false
    });
    let parsed: SearchResult = serde_json::from_value(old_result).expect("old results still parse");
    assert_eq!(parsed.chunk_id, "9f3a1c2b");
    assert_eq!(parsed.design_status.as_deref(), Some("decided"));
    assert_eq!(parsed.project_key, "", "no key from a daemon without keys");
    assert_eq!(parsed.effective_authority, None);
    assert_eq!(parsed.authority_note, None);

    let old_status = json!({
        "id": 1,
        "name": "lore",
        "root": "C:\\repos\\lore",
        "files": 12,
        "chunks": 40,
        "embedded_chunks": 40
    });
    let parsed: ProjectStatus =
        serde_json::from_value(old_status).expect("old status still parses");
    assert_eq!(parsed.key, "");
    assert_eq!(parsed.kind, "");
    assert_eq!(parsed.authority_violations, 0);
    assert!(parsed.authority_violation_paths.is_empty());
    assert_eq!(parsed.watch, WatchState::Unknown);
    // A daemon that predates profiles reports none — which reads exactly like
    // a repository that opted out, and is the honest degradation: the old
    // daemon genuinely cannot say which profile is in force.
    assert_eq!(parsed.authority_profile, None);
    assert_eq!(parsed.authority_behavior, None);
    assert_eq!(parsed.authority_config_error, None);
    assert_eq!((parsed.decisions_active, parsed.decisions_total), (0, 0));
    assert!(parsed.decision_violations.is_empty());

    // The request direction: an old client sends no `sources` and no
    // `project_key`, and a new daemon must accept both bodies.
    let old_request: SearchRequest =
        serde_json::from_value(json!({ "query": "ranking" })).expect("old request parses");
    assert_eq!(old_request.sources, None, "absent means every kind");
    assert_eq!(old_request.project_key, None);
    assert!(old_request.status.is_empty());

    let old_expand: ExpandRequest =
        serde_json::from_value(json!({ "project": "lore", "chunk_id": "x" }))
            .expect("old expand parses");
    assert_eq!(old_expand.project_key, None);
    assert_eq!(old_expand.context_lines, None);
}

// The response shapes exactly as they were before package 3, re-declared so a
// removal or rename shows up as a parse failure or a wrong value rather than
// as a field that merely happens to still exist under a new name.

#[derive(Debug, Deserialize)]
struct OldSearchResult {
    chunk_id: String,
    project: String,
    path: String,
    line_start: u32,
    line_end: u32,
    language: Option<String>,
    symbol_path: Option<String>,
    heading_path: Option<Vec<String>>,
    design_status: Option<String>,
    decision_refs: Vec<String>,
    score: f64,
    excerpt: String,
    excerpt_truncated: bool,
}

#[derive(Debug, Deserialize)]
struct OldProjectStatus {
    id: i64,
    name: String,
    root: String,
    files: u64,
    chunks: u64,
    embedded_chunks: u64,
    #[serde(default)]
    watch: WatchState,
}

/// A client built before this change, parsing a new daemon's response.
#[test]
fn a_client_that_predates_this_change_still_reads_a_new_daemons_response() {
    let json = serde_json::to_value(populated_search_result()).unwrap();
    let old: OldSearchResult = serde_json::from_value(json).expect("new results parse as old ones");
    assert_eq!(old.chunk_id, "9f3a1c2b");
    assert_eq!(old.project, "lore");
    assert_eq!(old.path, "design/9_Scratch/notes.md");
    assert_eq!((old.line_start, old.line_end), (15, 17));
    assert_eq!(old.language.as_deref(), Some("markdown"));
    assert_eq!(old.symbol_path, None);
    assert_eq!(
        old.heading_path.as_deref(),
        Some(["Notes".to_string()].as_slice())
    );
    // Still the *declared* status, unchanged in meaning for an old client:
    // the effective reading arrived in a new field rather than by redefining
    // this one, which is what keeps the old client honest instead of wrong.
    assert_eq!(old.design_status.as_deref(), Some("decided"));
    assert_eq!(old.decision_refs, ["D-0007"]);
    assert!((old.score - 0.8741).abs() < 1e-9);
    assert_eq!(old.excerpt, "body");
    assert!(!old.excerpt_truncated);

    let json = serde_json::to_value(populated_project_status()).unwrap();
    let old: OldProjectStatus = serde_json::from_value(json).expect("new status parses as old");
    assert_eq!(
        (old.id, old.files, old.chunks, old.embedded_chunks),
        (1, 12, 40, 40)
    );
    assert_eq!(old.name, "lore");
    assert_eq!(old.root, r"C:\repos\lore");
    assert_eq!(old.watch, WatchState::Armed);
}
