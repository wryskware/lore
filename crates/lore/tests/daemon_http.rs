//! The `/v1` HTTP surface, driven through `tower::ServiceExt::oneshot` — the
//! real router, the real handlers, the real store, but no port and no
//! background tasks. Anything that binds a socket is testing tokio, not Lore.

mod daemon_support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use daemon_support::{Fixture, populate_standard_tree};
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::full_scan;
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch::{self, WatchCommand, WatchReceiver};
use lore::embed::Embedder;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

struct Harness {
    fixture: Fixture,
    router: Router,
    queue: IndexQueue,
    watch: WatchReceiver,
}

fn harness() -> Harness {
    harness_from(Fixture::new("demo"))
}

/// The same harness over a caller-built project, for the tests that care
/// about what the repo committed in its `.lore.toml` (D-0012).
fn harness_from(fixture: Fixture) -> Harness {
    let queue = IndexQueue::new();
    let (watch_tx, watch) = watch::channel();
    let state = AppState {
        store: fixture.store.clone(),
        queue: queue.clone(),
        watch: watch_tx,
        // No watcher pump in this file, so every project reports `unknown`;
        // the real per-project states are covered in `daemon_watch.rs`.
        watch_status: watch::WatchStatus::new(),
        config: Arc::new(Config::default()),
        // No embedding endpoint: this file covers the lexical-only daemon.
        // Hybrid ranking and health transitions live in `embed_search.rs`.
        embeddings: Embedder::disabled(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        data_dir: fixture.data_dir.clone(),
    };
    Harness {
        router: router(state),
        fixture,
        queue,
        watch,
    }
}

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .expect("router never fails");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            panic!(
                "body was not JSON ({err}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, json)
}

async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
    call(router, "GET", uri, None).await
}

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    call(router, "POST", uri, Some(body)).await
}

// ---------------------------------------------------------------------------
// status / projects
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_reports_the_contract_fields() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["api_version"], lore_core::API_VERSION);
    assert_eq!(body["daemon_version"], env!("CARGO_PKG_VERSION"));
    assert!(body["generation"].as_u64().unwrap() >= 1);
    // No endpoint in config ⇒ the degradation is visible, not silent (D-0007).
    assert_eq!(body["embeddings"]["state"], "unconfigured");

    let project = &body["projects"][0];
    assert_eq!(project["name"], "demo");
    assert_eq!(project["root"], h.fixture.root.as_str());
    assert_eq!(project["files"], 3);
    assert!(project["chunks"].as_u64().unwrap() >= 3);
    assert_eq!(project["embedded_chunks"], 0);
    // Additive field, present on every project entry. No pump runs here, so
    // the honest answer is "no opinion" rather than a claim either way.
    assert_eq!(project["watch"], "unknown");
}

#[tokio::test]
async fn registering_a_project_lists_it_watches_it_and_queues_a_scan() {
    let mut h = harness();
    let other = tempfile::tempdir().unwrap();
    let root = other.path().to_string_lossy().to_string();

    let (status, body) = post(&h.router, "/v1/projects", json!({ "root": root })).await;
    assert_eq!(status, StatusCode::OK);
    let id = body["id"].as_i64().unwrap();
    assert!(!body["name"].as_str().unwrap().is_empty());

    let (status, body) = get(&h.router, "/v1/projects").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"demo"), "{names:?}");
    assert_eq!(names.len(), 2);

    // Registration is not just a database row: the project is watched and
    // scheduled for an initial scan before the response is sent.
    match h.watch.try_recv().expect("a watch command was emitted") {
        WatchCommand::Watch(project) => assert_eq!(project.id, id),
    }
    let (queued, work) = h.queue.take().expect("an initial scan was queued");
    assert_eq!(queued, id);
    assert!(work.full);

    // …and its exclusion policy is on disk before the scan runs, so the user
    // can see and edit it without waiting for anything.
    assert!(
        other.path().join(".loreignore").is_file(),
        "registration should have generated a .loreignore"
    );
}

#[tokio::test]
async fn an_explicit_name_overrides_the_directory_name() {
    let h = harness();
    let other = tempfile::tempdir().unwrap();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": other.path().to_string_lossy(), "name": "lexomancy" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "lexomancy");
}

#[tokio::test]
async fn registering_a_nonexistent_root_is_a_client_error() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": "C:/definitely/not/here/at/all" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["message"].is_string(), "ApiError shape: {body}");
}

#[tokio::test]
async fn registering_the_data_directory_is_refused() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": h.fixture.data_dir.as_str() }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("data directory"),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

#[tokio::test]
async fn index_queues_the_named_project_or_all_of_them() {
    let h = harness();

    let (status, body) = post(&h.router, "/v1/index", json!({ "project": "demo" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"][0]["name"], "demo");
    assert!(h.queue.take().is_some());

    let (status, body) = post(&h.router, "/v1/index", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"].as_array().unwrap().len(), 1);
    assert!(h.queue.take().is_some());

    let (status, body) = post(&h.router, "/v1/index", json!({ "project": "nope" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("nope"));
}

#[tokio::test]
async fn a_project_can_be_addressed_by_id_as_well_as_name() {
    let h = harness();
    let id = h.fixture.project.id.to_string();
    let (status, body) = post(&h.router, "/v1/index", json!({ "project": id })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"][0]["name"], "demo");
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_returns_ranked_lexical_results_with_provenance() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(&h.router, "/v1/search", json!({ "query": "daemon owns" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["lexical_only"], true,
        "no endpoint configured ⇒ visible degradation (D-0007)"
    );

    let results = body["results"].as_array().unwrap();
    assert!(!results.is_empty(), "expected a hit: {body}");
    let top = &results[0];
    assert_eq!(top["project"], "demo");
    assert_eq!(top["path"], "docs/design.md");
    assert_eq!(top["design_status"], "decided");
    assert_eq!(top["decision_refs"][0], "D-0007");
    assert!(top["heading_path"].is_array());
    assert!(top["chunk_id"].as_str().unwrap().len() >= 32);
    assert!(top["line_start"].as_u64().unwrap() >= 1);
    assert!(top["line_end"].as_u64().unwrap() >= top["line_start"].as_u64().unwrap());
    assert_eq!(top["excerpt_truncated"], false);
    assert!(top["excerpt"].as_str().unwrap().contains("daemon"));
    assert!(top["score"].as_f64().is_some());
}

#[tokio::test]
async fn code_hits_carry_a_symbol_path_and_a_language() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "alpha", "language": "rust" }),
    )
    .await;
    let top = &body["results"][0];
    assert_eq!(top["language"], "rust");
    assert_eq!(top["path"], "src/lib.rs");
    assert!(top["symbol_path"].is_string(), "{top}");
    assert!(top["heading_path"].is_null());
    assert!(top["design_status"].is_null());
}

#[tokio::test]
async fn search_filters_are_all_applied() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let hits = |body: &Value| body["results"].as_array().unwrap().len();

    let (_, unfiltered) = post(&h.router, "/v1/search", json!({ "query": "daemon" })).await;
    assert!(hits(&unfiltered) >= 1);

    // Path prefix.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "path_prefix": "src/" }),
    )
    .await;
    assert_eq!(hits(&body), 0, "no `daemon` under src/: {body}");

    // Language.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "language": "markdown" }),
    )
    .await;
    assert!(hits(&body) >= 1);
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "language": "csharp" }),
    )
    .await;
    assert_eq!(hits(&body), 0);

    // Vault status, including the "no frontmatter" atom.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "status": ["decided"] }),
    )
    .await;
    assert!(hits(&body) >= 1);
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "daemon", "status": ["unclassified"] }),
    )
    .await;
    assert_eq!(hits(&body), 0, "the only match is a decided doc: {body}");

    // Limit.
    let (_, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "the a demo daemon project", "limit": 1 }),
    )
    .await;
    assert!(hits(&body) <= 1);
}

#[tokio::test]
async fn search_rejects_an_unknown_project_and_an_unknown_status() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "x", "project": "ghost" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("ghost"));

    let (status, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "x", "status": ["definitely-decided"] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("definitely-decided")
    );
}

#[tokio::test]
async fn a_query_with_no_usable_terms_is_empty_not_an_error() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(&h.router, "/v1/search", json!({ "query": ")))  ((" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// expand
// ---------------------------------------------------------------------------

/// `expand` reads the *file*, so context lines can come from outside the
/// chunk — that is the whole point of the endpoint.
#[tokio::test]
async fn expand_widens_a_chunk_with_surrounding_file_context() {
    let h = harness();
    let lines: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    h.fixture.write("notes.md", format!("# Heading\n\n{lines}"));
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "line 150" })).await;
    let hit = &search["results"][0];
    let chunk_id = hit["chunk_id"].as_str().unwrap().to_string();
    let (chunk_start, chunk_end) = (
        hit["line_start"].as_u64().unwrap(),
        hit["line_end"].as_u64().unwrap(),
    );

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": chunk_id, "context_lines": 5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["path"], "notes.md");
    assert_eq!(body["file_lines"], 202);

    let start = body["line_start"].as_u64().unwrap();
    let end = body["line_end"].as_u64().unwrap();
    assert!(start >= 1 && start <= chunk_start);
    assert!(end >= chunk_end);
    assert_eq!(
        body["text"].as_str().unwrap().lines().count() as u64,
        end - start + 1,
        "the returned span and the returned text must agree"
    );
}

#[tokio::test]
async fn expand_clamps_context_and_never_runs_off_the_file() {
    let h = harness();
    h.fixture
        .write("small.md", "# Title\n\nJust one paragraph.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "paragraph" })).await;
    let chunk_id = search["results"][0]["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": chunk_id, "context_lines": 100_000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["line_start"], 1);
    assert_eq!(body["line_end"], 3);
    assert_eq!(body["file_lines"], 3);
}

/// Index and disk can disagree — the file may be gone by the time an agent
/// follows up on a search result. Serving the stored chunk beats a 404 the
/// caller cannot act on.
#[tokio::test]
async fn expand_falls_back_to_stored_text_when_the_file_is_gone() {
    let h = harness();
    h.fixture
        .write("gone.md", "# Gone\n\nSoon to be deleted.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "deleted" })).await;
    let chunk_id = search["results"][0]["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();

    h.fixture.remove("gone.md");

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": chunk_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["text"].as_str().unwrap().contains("deleted"));
    assert_eq!(body["line_end"], body["file_lines"]);
}

/// The S1#3 round trip: a search result hands back a `project_key`, and that
/// key — not the display name — is what identifies the source on the way back
/// in. The name path stays for humans and older clients.
#[tokio::test]
async fn expand_resolves_by_project_key_in_preference_to_the_display_name() {
    let h = harness();
    h.fixture
        .write("notes.md", "# Heading\n\nA line about ranking.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "ranking" })).await;
    let hit = &search["results"][0];
    let key = hit["project_key"].as_str().unwrap().to_string();
    let chunk_id = hit["chunk_id"].as_str().unwrap().to_string();
    assert_eq!(key, "demo", "{search:#}");

    // Key alone, with no `project` field at all.
    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project_key": key, "chunk_id": chunk_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["path"], "notes.md");

    // An unknown key is a 404 that names the key, not a silent fallback to
    // whatever `project` happened to say.
    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "project_key": "ghost-key", "chunk_id": chunk_id }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["message"].as_str().unwrap().contains("ghost-key"),
        "{body:#}"
    );

    // Neither is a bad request, not a confusing 404.
    let (status, body) = post(&h.router, "/v1/expand", json!({ "chunk_id": chunk_id })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("project_key"),
        "{body:#}"
    );
}

/// Two projects sharing a display name make name-based resolution ambiguous,
/// which is how a search hit becomes an `expand` 404 (S1#3). The registry
/// refuses to enter that state, and says which flag fixes it.
#[tokio::test]
async fn registering_a_duplicate_display_name_is_refused_with_a_remedy() {
    let h = harness();
    let other = tempfile::tempdir().unwrap();
    let root = other.path().to_string_lossy().to_string();

    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": root, "name": "demo" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("already registered"), "{message}");
    assert!(message.contains("--name"), "{message}");

    // The same root under a free name is accepted and gets its own key.
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": root, "name": "other" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["key"], "other");
    assert_eq!(body["kind"], "repo");

    // Re-registering the *same* root under its own name is a rename, not a
    // collision with itself.
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": root, "name": "other" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["key"], "other", "the key survives re-registration");
}

/// Precedence, stated as a conflict rather than as an absence: both fields are
/// present, both name a *real* project, and they disagree. The key has to win,
/// or the field is decorative — an agent replaying a stale `project` string
/// alongside a fresh key would silently query the wrong source.
#[tokio::test]
async fn a_valid_but_mismatched_display_name_loses_to_the_project_key() {
    let h = harness();
    h.fixture
        .write("notes.md", "# Heading\n\nA line about ranking.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    // A second, genuinely registered project that does not contain the chunk.
    let other = tempfile::tempdir().unwrap();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": other.path().to_string_lossy(), "name": "decoy" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");

    let (_, search) = post(&h.router, "/v1/search", json!({ "query": "ranking" })).await;
    let hit = &search["results"][0];
    let chunk_id = hit["chunk_id"].as_str().unwrap().to_string();
    assert_eq!(hit["project_key"], "demo", "{search:#}");

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "decoy", "project_key": "demo", "chunk_id": chunk_id }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the key resolved, not the name: {body:#}"
    );
    assert_eq!(body["path"], "notes.md");

    // And the reverse: the name is right, the key is wrong, so it fails —
    // proving the key is consulted first rather than merely as a fallback.
    let (status, _) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "project_key": "decoy", "chunk_id": chunk_id }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The `status` surface of the authority policy, on the wire (design note 1d).
/// The store-level counting is covered in `daemon_authority.rs`; what this adds
/// is that the count and the paths actually reach a client, since every
/// renderer downstream is driven from these two fields.
#[tokio::test]
async fn status_reports_refused_authority_declarations_over_the_wire() {
    let h = harness();
    h.fixture.write(
        "design/0_Canon/DECISIONS.md",
        "# Ledger\n\n## D-0001 — Live\n\n- **Status:** Accepted\n",
    );
    h.fixture.write(
        "design/honest.md",
        "---\ndesign_status: decided\ndecision_refs: [D-0001]\n---\n\n# Honest\n\nBody.\n",
    );
    h.fixture.write(
        "design/overclaim.md",
        "---\ndesign_status: decided\n---\n\n# Overclaim\n\nBody.\n",
    );
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    let project = &body["projects"][0];
    assert_eq!(project["authority_violations"], 1, "{body:#}");
    assert_eq!(
        project["authority_violation_paths"],
        json!(["design/overclaim.md"]),
        "{body:#}"
    );
    // The provenance fields ride along on the same row.
    assert_eq!(project["key"], "demo");
    assert_eq!(project["kind"], "repo");
}

/// A repository's authority posture has to be legible from this row alone
/// (D-0012): the profile it declared, how far that profile reaches, and how
/// much canon Lore found. `lore status` and every other client render nothing
/// but these fields.
#[tokio::test]
async fn status_reports_the_declared_profile_and_its_decision_corpus() {
    // The default fixture commits `lore-v1` with `behavior = "rank"`.
    let h = harness();
    h.fixture.write(
        "design/0_Canon/DECISIONS.md",
        "# Ledger\n\n## D-0001 — Live\n\n- **Status:** Accepted\n\n\
         ## D-0002 — Draft\n\n- **Status:** Proposed\n",
    );
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    let project = &body["projects"][0];
    assert_eq!(project["authority_profile"], "lore-v1", "{body:#}");
    assert_eq!(project["authority_behavior"], "rank", "{body:#}");
    assert_eq!(project["authority_config_error"], Value::Null, "{body:#}");
    assert_eq!(project["decisions_active"], 1, "{body:#}");
    assert_eq!(project["decisions_total"], 2, "{body:#}");
    assert_eq!(
        project["decision_violations"],
        Value::Null,
        "an empty defect list is omitted, not an empty array: {body:#}"
    );
}

/// A repository that never opted in reports *nothing* rather than a default,
/// because "no profile" and "the neutral profile" are different claims and a
/// client rendering the second would be inventing a judgement.
#[tokio::test]
async fn status_reports_an_unconfigured_repo_as_having_no_profile() {
    let h = harness_from(Fixture::neutral("plain"));
    h.fixture.write(
        "design/0_Canon/DECISIONS.md",
        "# Ledger\n\n## D-0001 — Live\n\n- **Status:** Accepted\n",
    );
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    let project = &body["projects"][0];
    assert_eq!(project["authority_profile"], Value::Null, "{body:#}");
    assert_eq!(project["authority_behavior"], Value::Null, "{body:#}");
    assert_eq!(project["authority_config_error"], Value::Null, "{body:#}");
    assert_eq!(project["decisions_active"], 0, "the ledger is not parsed");
    assert_eq!(project["decisions_total"], 0, "nor even counted");
}

/// D-0012's loudness requirement, on the wire. A `.lore.toml` Lore cannot use
/// indexes the repo neutrally — which is indistinguishable from having no
/// file at all *except* for this field. If it did not reach the client, the
/// only symptom of a typo would be a profile that mysteriously never turned
/// on.
#[tokio::test]
async fn status_shouts_about_a_lore_toml_it_cannot_use() {
    let fixture = Fixture::neutral("broken");
    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        "[authority]\nprofile = \"adr\"\n",
    );
    let h = harness_from(fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    let project = &body["projects"][0];
    let error = project["authority_config_error"]
        .as_str()
        .unwrap_or_else(|| panic!("the error must be on the wire: {body:#}"));
    assert!(error.contains("adr"), "it must name the problem: {error:?}");
    assert_eq!(
        project["authority_profile"],
        Value::Null,
        "and the repo is neutral, not half-configured: {body:#}"
    );
}

/// Registration is the only thing that writes the registry outside startup, so
/// it must republish immediately — otherwise the project is indexed now and
/// gone after the next restart.
#[tokio::test]
async fn registering_a_project_republishes_the_manifest() {
    let h = harness();
    let other = tempfile::tempdir().unwrap();
    let root = other.path().to_string_lossy().to_string();
    post(
        &h.router,
        "/v1/projects",
        json!({ "root": root, "name": "second" }),
    )
    .await;

    let manifest = lore::registry::read(&h.fixture.data_dir)
        .expect("read manifest")
        .expect("registration must publish a manifest");
    let names: Vec<&str> = manifest
        .projects
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert!(names.contains(&"second"), "{names:?}");
    assert!(manifest.projects.iter().all(|entry| !entry.key.is_empty()));
}

#[tokio::test]
async fn expand_rejects_unknown_projects_and_unknown_chunks() {
    let h = harness();
    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "ghost", "chunk_id": "abc" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("ghost"));

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": "0000000000000000" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("chunk"));
}

// ---------------------------------------------------------------------------
// Protocol-level behaviour
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_error_body_is_an_api_error_including_rejections() {
    let h = harness();

    // Unknown route.
    let (status, body) = get(&h.router, "/v1/nonsense").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].is_string(), "{body}");

    // Unversioned route: there is no unversioned surface.
    let (status, body) = get(&h.router, "/status").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].is_string(), "{body}");

    // Malformed JSON — axum's own rejection would be plain text.
    let response = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).expect("rejections are JSON too");
    assert!(body["message"].is_string(), "{body}");

    // Missing required field.
    let (status, body) = post(&h.router, "/v1/search", json!({ "limit": 5 })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["message"].is_string(), "{body}");
}

#[tokio::test]
async fn oversized_bodies_are_rejected_rather_than_buffered() {
    let h = harness();
    let huge = "x".repeat(2 * 1024 * 1024);
    let response = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "query": huge })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
