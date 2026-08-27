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
        // No index pass runs through the router in this file, so no project
        // has a refused apply; the push routes have their own file.
        index: fixture.context(),
        push: fixture.push_leases(),
        config: Arc::new(Config::default()),
        // No embedding endpoint: this file covers the lexical-only daemon.
        // Hybrid ranking and health transitions live in `embed_search.rs`.
        embeddings: Embedder::disabled(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        // No chunker plugins: this file is not about them, and an empty
        // registry routes exactly as no registry at all does.
        plugins: std::sync::Arc::new(lore::plugin::PluginRegistry::empty()),
        plugin_diagnostics: std::sync::Arc::new(Vec::new()),
        data_dir: fixture.data_dir.clone(),
        // Nothing here drives a real shutdown; the token exists so the
        // route can cancel something rather than reach for a global.
        shutdown: fixture.cancel.clone(),
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

async fn delete(router: &Router, uri: &str) -> (StatusCode, Value) {
    call(router, "DELETE", uri, None).await
}

/// A scoped search against this file's single-project harness.
///
/// Every query names a project now, and the tests below are about filters,
/// ranking and `expand` — not about scoping. Adding `"project": "demo"` to
/// each of them by hand would say nothing and hide the one place it matters,
/// so it is defaulted here and the scoping tests post to `/v1/search` directly
/// where the *absence* is the thing under test.
async fn search(router: &Router, mut body: Value) -> (StatusCode, Value) {
    if body.get("project").is_none() && body.get("project_key").is_none() {
        body["project"] = json!("demo");
    }
    post(router, "/v1/search", body).await
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
        other => panic!("registration must arm a watch, not {other:?}"),
    }
    let (queued, work) = h.queue.take().expect("an initial scan was queued");
    assert_eq!(queued, id);
    assert!(work.full);

    // …and registration wrote nothing into the project. D-0020 retired
    // `.loreignore` generation: a project's ignore rules exist only where a
    // human wrote them, so enrolling a repo must not leave a file behind that
    // the user then has to review, commit, or wonder about.
    assert!(
        !other.path().join(".loreignore").exists(),
        "registration must not generate a .loreignore"
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
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("nope"), "{message}");
    // The last refusal on this surface that named no remedy. Every other
    // "which project?" answer points at the same two ways out, and a caller
    // should not have to learn which endpoint is the terse one.
    assert!(message.contains("lore status"), "{message}");
    assert!(message.contains("lore add <path>"), "{message}");
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

    let (status, body) = search(&h.router, json!({ "query": "daemon owns" })).await;
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

    let (_, body) = search(&h.router, json!({ "query": "alpha", "language": "rust" })).await;
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

    let (_, unfiltered) = search(&h.router, json!({ "query": "daemon" })).await;
    assert!(hits(&unfiltered) >= 1);

    // Path prefix.
    let (_, body) = search(
        &h.router,
        json!({ "query": "daemon", "path_prefix": "src/" }),
    )
    .await;
    assert_eq!(hits(&body), 0, "no `daemon` under src/: {body}");

    // Language.
    let (_, body) = search(
        &h.router,
        json!({ "query": "daemon", "language": "markdown" }),
    )
    .await;
    assert!(hits(&body) >= 1);
    let (_, body) = search(
        &h.router,
        json!({ "query": "daemon", "language": "csharp" }),
    )
    .await;
    assert_eq!(hits(&body), 0);

    // Vault status, including the "no frontmatter" atom.
    let (_, body) = search(
        &h.router,
        json!({ "query": "daemon", "status": ["decided"] }),
    )
    .await;
    assert!(hits(&body) >= 1);
    let (_, body) = search(
        &h.router,
        json!({ "query": "daemon", "status": ["unclassified"] }),
    )
    .await;
    assert_eq!(hits(&body), 0, "the only match is a decided doc: {body}");

    // Limit.
    let (_, body) = search(
        &h.router,
        json!({ "query": "the a demo daemon project", "limit": 1 }),
    )
    .await;
    assert!(hits(&body) <= 1);
}

#[tokio::test]
async fn search_rejects_an_unknown_project_and_an_unknown_status() {
    let h = harness();
    let (status, body) = search(&h.router, json!({ "query": "x", "project": "ghost" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("ghost"));

    let (status, body) = search(
        &h.router,
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

    let (status, body) = search(&h.router, json!({ "query": ")))  ((" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// shutdown
// ---------------------------------------------------------------------------

/// `POST /v1/shutdown` cancels the daemon's one shutdown token — the same
/// signal ctrl-c raises — and answers before going anywhere, because the
/// response has to reach the caller through a server that is now draining.
#[tokio::test]
async fn shutdown_acknowledges_and_then_cancels_the_daemon() {
    let h = harness();
    assert!(!h.fixture.cancel.is_cancelled());

    let (status, body) = post(&h.router, "/v1/shutdown", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(
        body["pid"].as_u64().unwrap() as u32,
        std::process::id(),
        "the answer names the process that is stopping"
    );
    assert!(
        h.fixture.cancel.is_cancelled(),
        "the shutdown token must actually be cancelled"
    );
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

    let (_, search) = search(&h.router, json!({ "query": "line 150" })).await;
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

    let (_, search) = search(&h.router, json!({ "query": "paragraph" })).await;
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

    let (_, search) = search(&h.router, json!({ "query": "deleted" })).await;
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

    let (_, search) = search(&h.router, json!({ "query": "ranking" })).await;
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
    // 409, not 400: the request is well-formed and the name is legal — the
    // registry's current state is what refuses it, and that state can change.
    assert_eq!(status, StatusCode::CONFLICT, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("already registered"), "{message}");
    // Both remedies, because they are different decisions: rename this
    // project, or give up the name the other one holds.
    assert!(message.contains("--name"), "{message}");
    assert!(message.contains(".lore.toml"), "{message}");
    assert!(message.contains("lore remove demo"), "{message}");
    // Both roots are named, so the user can tell which project they collided
    // with without going looking for it.
    let claimant = lore::daemon::paths::canonicalize_root(&root).unwrap();
    assert!(message.contains(h.fixture.root.as_str()), "{message}");
    assert!(message.contains(claimant.as_str()), "{message}");

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

    let (_, search) = search(&h.router, json!({ "query": "ranking" })).await;
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

/// A project made of more than one directory says so, and says where from.
///
/// Reported because the line above prints one root: with a mount declared, a
/// search result like `engine/render/pass.rs` names a file that lives nowhere
/// near it, and a reader who cannot see the extent goes looking under the
/// wrong directory.
#[tokio::test]
async fn status_reports_a_projects_declared_extent() {
    let outside = tempfile::tempdir().unwrap();
    let outside = camino::Utf8Path::from_path(outside.path()).unwrap();
    std::fs::write(outside.join("lib.rs"), "pub fn engine() {}").unwrap();

    let fixture = Fixture::neutral("mounted");
    let declared = format!("../{}", outside.file_name().expect("a temp dir has a name"));
    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        format!(
            "[[sources]]
path = \".\"

[[sources]]
path = \"{declared}\"
mount = \"engine\"
"
        ),
    );
    let h = harness_from(fixture);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    let sources = body["projects"][0]["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("the extent must be on the wire: {body:#}"));
    assert_eq!(sources.len(), 2, "{body:#}");
    assert_eq!(sources[0]["mount"], "", "the root source carries no prefix");
    assert_eq!(sources[1]["mount"], "engine");
    assert_eq!(
        body["projects"][0]["sources_error"],
        Value::Null,
        "a table that resolved has nothing to report: {body:#}"
    );
}

/// The ordinary project — its own root and nothing else — reports no extent
/// at all. One anonymous source on every project would be noise that means
/// nothing, and its absence is exactly what "this project is its root" says.
#[tokio::test]
async fn status_reports_no_extent_for_a_project_that_is_just_its_root() {
    let h = harness();
    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(
        body["projects"][0]["sources"],
        Value::Null,
        "an empty extent is omitted, not sent as a one-element list: {body:#}"
    );
}

/// A `[[sources]]` table Lore refused is shouted about, for the same reason a
/// broken authority profile is: the project indexed as its root alone, which
/// is a very different project from the one its file described. Silence here
/// would read as "your mounts are fine" while the mounted content is simply
/// missing.
#[tokio::test]
async fn status_shouts_about_a_sources_table_it_cannot_use() {
    let fixture = Fixture::neutral("broken-extent");
    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        "[[sources]]
path = \".\"

[[sources]]
path = \"../nowhere\"
mount = \"gone\"
",
    );
    let h = harness_from(fixture);

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    let error = body["projects"][0]["sources_error"]
        .as_str()
        .unwrap_or_else(|| panic!("the error must be on the wire: {body:#}"));
    assert!(
        error.contains("nowhere"),
        "it must name the problem: {error:?}"
    );
    assert_eq!(
        body["projects"][0]["sources"],
        Value::Null,
        "and the fallback is the root alone, reported as no extent: {body:#}"
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
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("chunk"), "{message}");
    // The remedy for a stale id is a fresh search, and only the message can
    // say so — nothing else tells the caller ids move when files change.
    assert!(message.contains("search again"), "{message}");
}

/// Issue #7: search prints a shortened id, so `expand` has to accept one.
/// The prefix is resolved *within the request's project*, which is what keeps
/// twelve characters comfortable.
#[tokio::test]
async fn expand_accepts_the_shortened_id_search_actually_prints() {
    let h = harness();
    h.fixture
        .write("notes.md", "# Heading\n\nA line about ranking.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, search) = search(&h.router, json!({ "query": "ranking" })).await;
    let chunk_id = search["results"][0]["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(chunk_id.len(), 64, "the wire still carries the whole id");

    // Twelve characters — what the renderers print — and eight, the floor.
    for prefix in [&chunk_id[..12], &chunk_id[..8], chunk_id.as_str()] {
        let (status, body) = post(
            &h.router,
            "/v1/expand",
            json!({ "project": "demo", "chunk_id": prefix }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{prefix} -> {body:#}");
        assert_eq!(body["path"], "notes.md");
    }

    // Upper case is a copy artifact, not a different chunk.
    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": chunk_id[..12].to_uppercase() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
}

#[tokio::test]
async fn expand_refuses_a_prefix_too_short_to_mean_anything() {
    let h = harness();
    for (given, expected) in [
        ("abc123", "at least 8"),
        ("zzzzzzzzzz", "hexadecimal"),
        ("", "hexadecimal"),
    ] {
        let (status, body) = post(
            &h.router,
            "/v1/expand",
            json!({ "project": "demo", "chunk_id": given }),
        )
        .await;
        // A 400, not a 404: nothing was looked up, because the argument could
        // not be one.
        assert_eq!(status, StatusCode::BAD_REQUEST, "{given} -> {body:#}");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains(expected), "{given} -> {message}");
        assert!(message.contains("search result"), "{given} -> {message}");
    }
}

/// Two chunks sharing an id prefix cannot be produced by hashing, so they are
/// written straight into the store. The point is that an ambiguous prefix is
/// answered with the candidates rather than with whichever row sorted first.
#[tokio::test]
async fn an_ambiguous_prefix_names_the_candidates_instead_of_guessing() {
    use lore::types::{Chunk, ChunkId, ChunkKind};

    let h = harness();
    let project = h.fixture.project.id;
    let chunk = |id: &str, text: &str| {
        let path = camino::Utf8PathBuf::from("twins.rs");
        let kind = ChunkKind::Code {
            symbol_kind: "function".into(),
            symbol_path: text.into(),
            window: None,
        };
        Chunk {
            id: ChunkId(id.into()),
            path,
            kind,
            language: Some("rust".into()),
            byte_start: 0,
            byte_end: text.len() as u32,
            line_start: 1,
            line_end: 1,
            text: text.into(),
            vault: None,
        }
    };
    let twins = [
        chunk("dead1234aaaaaaaa", "fn a() {}"),
        chunk("dead1234bbbbbbbb", "fn b() {}"),
    ];
    h.fixture
        .store
        .blocking(move |store| {
            store.replace_file_chunks(project, camino::Utf8Path::new("twins.rs"), "h", &twins)
        })
        .expect("write colliding chunks");

    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": "dead1234" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("dead1234aaaaaaaa"), "{message}");
    assert!(message.contains("dead1234bbbbbbbb"), "{message}");
    assert!(message.contains("in full"), "{message}");

    // One more character is all it takes, and that is the remedy the message
    // names rather than describes.
    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": "demo", "chunk_id": "dead1234b" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["path"], "twins.rs");
}

// ---------------------------------------------------------------------------
// Scoping: every query names exactly one project
// ---------------------------------------------------------------------------

/// The wire contract from the scoping resolution: an unscoped query used to
/// span every project on the machine, which on a shared daemon is one user
/// reading another's code. It is refused, and the refusal says how to scope.
#[tokio::test]
async fn an_unscoped_search_is_refused_with_the_remedy() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    for body in [
        json!({ "query": "daemon" }),
        json!({ "query": "daemon", "project": null }),
        json!({ "query": "daemon", "project": "   " }),
    ] {
        let (status, response) = post(&h.router, "/v1/search", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {response:#}");
        let message = response["message"].as_str().unwrap();
        assert!(message.contains("scoped to one project"), "{message}");
        assert!(message.contains("project"), "{message}");
        assert!(message.contains("lore status"), "{message}");
        assert!(message.contains("lore add <path>"), "{message}");
    }

    // …and the same query with a project is a perfectly ordinary success, so
    // the refusal is about scoping and nothing else.
    let (status, _) = search(&h.router, json!({ "query": "daemon" })).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_unscoped_expand_is_refused_with_the_remedy() {
    let h = harness();
    let (status, body) = post(&h.router, "/v1/expand", json!({ "chunk_id": "abc" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("scoped to one project"), "{message}");
    assert!(message.contains("project_key"), "{message}");
    assert!(message.contains("lore add <path>"), "{message}");

    // A whitespace-only display name is absence, not a project named "  ".
    let (status, body) = post(
        &h.router,
        "/v1/expand",
        json!({ "project": " ", "chunk_id": "abc" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body:#}");
}

/// The key path stays scoped too — `search` resolves it exactly as `expand`
/// does, so an agent replaying a hit's `project_key` reaches the same source.
#[tokio::test]
async fn search_accepts_a_project_key_and_prefers_it_over_the_display_name() {
    let h = harness();
    h.fixture
        .write("notes.md", "# Heading\n\nA line about ranking.\n");
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "ranking", "project_key": "demo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["results"][0]["path"], "notes.md");

    // A wrong key beats a right name, or the field would be decorative.
    let (status, body) = post(
        &h.router,
        "/v1/search",
        json!({ "query": "ranking", "project": "demo", "project_key": "ghost-key" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("ghost-key"), "{message}");
    assert!(message.contains("lore add <path>"), "{message}");
}

/// `status` narrows to one project the same way `search` does. Without a
/// filter it stays machine-wide on purpose — that is the local-admin view.
#[tokio::test]
async fn status_scopes_to_one_project_and_404s_on_an_unknown_one() {
    let h = harness();
    let other = tempfile::tempdir().unwrap();
    post(
        &h.router,
        "/v1/projects",
        json!({ "root": other.path().to_string_lossy(), "name": "second" }),
    )
    .await;

    let names = |body: &Value| -> Vec<String> {
        body["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect()
    };

    let (status, body) = get(&h.router, "/v1/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&body).len(), 2, "the unscoped view is machine-wide");

    let (status, body) = get(&h.router, "/v1/status?project=second").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(names(&body), ["second"]);

    // By key as well as by name: a client holding either can narrow.
    let (status, body) = get(&h.router, "/v1/status?project=demo").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(names(&body), ["demo"]);
    assert_eq!(body["projects"][0]["key"], "demo");

    // An unknown name is a 404, not an empty list: "no such project" and "that
    // project has nothing indexed" are different answers.
    let (status, body) = get(&h.router, "/v1/status?project=ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("ghost"), "{message}");
    assert!(message.contains("lore add <path>"), "{message}");
}

// ---------------------------------------------------------------------------
// resolve (INTERIM — see the handler's doc comment)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_finds_the_project_containing_a_path() {
    let h = harness();
    h.fixture.write("src/deep/nested/file.rs", "fn x() {}\n");

    // The root itself, and a directory well inside it: containment is a prefix
    // test, so a subdirectory costs no walk-up.
    for path in [
        h.fixture.root.clone(),
        h.fixture.root.join("src").join("deep").join("nested"),
    ] {
        let (status, body) = get(
            &h.router,
            &format!("/v1/resolve?path={}", urlencode(path.as_str())),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body:#}");
        assert_eq!(body["name"], "demo");
        assert_eq!(body["key"], "demo");
        assert_eq!(body["root"], h.fixture.root.as_str());
    }
}

/// Roots may legitimately nest (a repo and a package inside it are two
/// projects). The innermost is the one the caller is standing in; first-match
/// would resolve half of them to the wrong project depending on registry order.
#[tokio::test]
async fn resolve_prefers_the_longest_matching_root() {
    let h = harness();
    let inner_path = h.fixture.root.join("packages").join("game");
    std::fs::create_dir_all(&inner_path).unwrap();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": inner_path.as_str(), "name": "game" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");

    let inner = lore::daemon::paths::canonicalize_root(&inner_path).unwrap();
    let (status, body) = get(
        &h.router,
        &format!("/v1/resolve?path={}", urlencode(inner.join("src").as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["name"], "game", "the innermost root wins: {body:#}");

    // A sibling of the inner root still belongs to the outer project.
    let (status, body) = get(
        &h.router,
        &format!(
            "/v1/resolve?path={}",
            urlencode(h.fixture.root.join("docs").as_str())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["name"], "demo");
}

#[tokio::test]
async fn resolve_rejects_a_path_outside_every_root_and_a_relative_one() {
    let h = harness();
    let outside = tempfile::tempdir().unwrap();
    let outside = lore::daemon::paths::canonicalize_root(outside.path()).unwrap();

    let (status, body) = get(
        &h.router,
        &format!("/v1/resolve?path={}", urlencode(outside.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("not inside any registered project"),
        "{message}"
    );
    assert!(message.contains("lore add <path>"), "{message}");

    // Relative and empty are the client's bug, not a missing registration:
    // a relative path means nothing to a daemon started somewhere else.
    for path in ["src/lib.rs", ".", ""] {
        let (status, body) = get(&h.router, &format!("/v1/resolve?path={}", urlencode(path))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path:?}: {body:#}");
        assert!(
            body["message"].as_str().unwrap().contains("absolute"),
            "{body:#}"
        );
    }
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

/// End to end against a real store: register, index, remove — and then check
/// all four things that made it "registered" are gone, not just the one the
/// caller happened to look at.
#[tokio::test]
async fn removing_a_project_forgets_its_index_manifest_entry_and_watch() {
    let mut h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);
    let id = h.fixture.project.id;
    assert!(h.fixture.chunk_count() > 0, "the fixture must have indexed");

    // Registering a second project so the removal is a removal, not an
    // emptying: a bug that wiped the whole store would pass otherwise.
    let other = tempfile::tempdir().unwrap();
    post(
        &h.router,
        "/v1/projects",
        json!({ "root": other.path().to_string_lossy(), "name": "survivor" }),
    )
    .await;
    while h.watch.try_recv().is_ok() {}

    let (status, body) = delete(&h.router, "/v1/projects/demo").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["project"]["name"], "demo");
    assert_eq!(body["project"]["root"], h.fixture.root.as_str());
    assert_eq!(body["files"], 3, "the caller is told what it discarded");
    assert!(body["chunks"].as_u64().unwrap() >= 3, "{body:#}");

    // Gone from `status` immediately — not after a restart.
    let (_, body) = get(&h.router, "/v1/status").await;
    let names: Vec<&str> = body["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["survivor"], "{body:#}");

    // Gone from the store: rows, not just the projects row.
    let rows = h
        .fixture
        .store
        .blocking(move |store| store.list_files(id))
        .expect("list files");
    assert!(rows.is_empty(), "file records survived: {rows:?}");

    // Gone from the authoritative manifest, so it does not come back on the
    // next start — which is the failure `lore remove` exists to prevent.
    let manifest = lore::registry::read(&h.fixture.data_dir)
        .expect("read manifest")
        .expect("a manifest exists");
    let listed: Vec<&str> = manifest
        .projects
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(listed, ["survivor"], "{listed:?}");

    // And the watcher is told to let go of the root.
    match h.watch.try_recv().expect("an unwatch command was emitted") {
        WatchCommand::Unwatch(project) => assert_eq!(project, id),
        other => panic!("removal must disarm the watch, not {other:?}"),
    }
}

#[tokio::test]
async fn a_project_can_be_removed_by_key_and_an_unknown_one_is_a_404() {
    let h = harness();
    let (status, body) = delete(&h.router, "/v1/projects/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("ghost"), "{message}");
    assert!(message.contains("lore add <path>"), "{message}");

    // The key is as good a handle as the name — a caller holding only the key
    // from a search result should not have to translate it first.
    let (status, body) = delete(&h.router, "/v1/projects/demo").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["project"]["key"], "demo");

    // Removal is not idempotent-silent: a second one is an honest 404.
    let (status, _) = delete(&h.router, "/v1/projects/demo").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// D-0016 extended (Wrysk, 2026-08-17): a declared name is one identity however
/// it is cased, and the registry refuses a second project by that name — so
/// every lookup by name has to fold too, or a user who types `LORE` is told
/// their project does not exist while the daemon can plainly see it.
///
/// Every name-keyed route, in one test: registration's refusal, the status
/// filter (which is also how `lore-mcp` resolves a `LORE_PROJECT` pin), and
/// `resolve_project` behind the removal route.
#[tokio::test]
async fn a_project_is_found_by_any_casing_of_its_name() {
    let h = harness();
    let other = tempfile::tempdir().unwrap();

    // A second root cannot take the name in a different spelling, and the 409
    // shows the spelling that is actually stored.
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": other.path().to_string_lossy(), "name": "DEMO" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body:#}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("`demo`"), "{message}");
    assert!(message.contains("`DEMO`"), "{message}");

    // Scoping by a case variant narrows to the project, and reports the stored
    // spelling back.
    let (status, body) = get(&h.router, "/v1/status?project=DeMo").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["projects"][0]["name"], "demo");

    // …and so does addressing it in a path segment.
    let (status, body) = delete(&h.router, "/v1/projects/DEMO").await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["project"]["name"], "demo");
}

/// Names carry spaces and other characters that would otherwise end the path
/// segment early; the CLI percent-encodes, and the router has to decode.
#[tokio::test]
async fn a_project_name_needing_percent_encoding_still_addresses_its_project() {
    let h = harness();
    let other = tempfile::tempdir().unwrap();
    let (status, body) = post(
        &h.router,
        "/v1/projects",
        json!({ "root": other.path().to_string_lossy(), "name": "my design vault" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");

    let (status, body) = delete(
        &h.router,
        &format!("/v1/projects/{}", urlencode("my design vault")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:#}");
    assert_eq!(body["project"]["name"], "my design vault");
}

/// Percent-encoding matching `cli::urlencode`, so these tests exercise the
/// same escaping the real client produces.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
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
    let (status, body) = search(&h.router, json!({ "limit": 5 })).await;
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

// ---------------------------------------------------------------------------
// bundle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bundle_answers_with_verified_spans_read_from_disk() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(
        &h.router,
        "/v1/bundle",
        json!({ "query": "the daemon owns index state", "project": "demo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verdict"], "found", "{body}");

    let text = body["text"].as_str().unwrap();
    assert!(text.starts_with("VERDICT: found ("), "{text}");
    // Lexical-only degradation is visible here exactly as it is in `search`:
    // this harness configures no embedding endpoint.
    assert_eq!(body["lexical_only"], true);
    assert!(text.contains("lexical-only degradation"), "{text}");
    // The span header names the file, the span and the chunker's own label,
    // and the body carries real line numbers off disk.
    assert!(text.contains("=== docs/design.md:"), "{text}");
    assert!(text.contains("The daemon owns index state."), "{text}");
    assert!(text.contains("Topology"), "{text}");

    let spans = body["spans"].as_array().unwrap();
    assert!(!spans.is_empty(), "{body}");
    let top = &spans[0];
    assert_eq!(top["path"], "docs/design.md");
    assert!(top["line_start"].as_u64().unwrap() >= 1);
    assert!(top["chunk_id"].as_str().unwrap().len() >= 32);
    // Nothing in a freshly-scanned tree can be stale, missing or out of range.
    assert_eq!(body["hits_rejected"], 0, "{body}");
    assert!(body["dropped"].as_array().unwrap().is_empty(), "{body}");
    assert!(
        body["bundle_tokens_est"].as_u64().unwrap() <= 4000,
        "{body}"
    );
}

#[tokio::test]
async fn a_bundle_that_covers_nothing_says_so_and_names_the_gap() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(
        &h.router,
        "/v1/bundle",
        json!({ "query": "quantum chromodynamics lattice gauge", "project": "demo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Whatever the retriever's score said, no query term is anywhere in what
    // came back — which is the failure a score threshold cannot see.
    assert_eq!(body["verdict"], "none", "{body}");
    assert_eq!(body["coverage"], 0.0, "{body}");
    let text = body["text"].as_str().unwrap();
    assert!(
        text.contains("NO MATCH FOR: quantum, chromodynamics, lattice, gauge"),
        "{text}"
    );
}

#[tokio::test]
async fn bundle_is_scoped_and_says_how_to_scope_it() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (status, body) = post(&h.router, "/v1/bundle", json!({ "query": "daemon" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("lore add <path>"),
        "{body}"
    );

    let (status, body) = post(
        &h.router,
        "/v1/bundle",
        json!({ "query": "daemon", "project": "nope" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["message"].as_str().unwrap().contains("nope"), "{body}");
}

#[tokio::test]
async fn a_bundle_budget_of_one_span_demotes_the_rest_to_further_reading() {
    let h = harness();
    populate_standard_tree(&h.fixture);
    full_scan(&h.fixture.context(), &h.fixture.project);

    let (_, body) = post(
        &h.router,
        "/v1/bundle",
        json!({
            "query": "daemon watcher alpha beta demo project",
            "project": "demo",
            "budget_tokens": 1
        }),
    )
    .await;
    // One span always renders, and everything past it is a pointer rather
    // than a truncated block.
    assert_eq!(body["spans"].as_array().unwrap().len(), 1, "{body}");
    assert!(
        !body["further_reading"].as_array().unwrap().is_empty(),
        "{body}"
    );
    assert!(
        body["text"].as_str().unwrap().contains("FURTHER READING: "),
        "{body}"
    );
}
