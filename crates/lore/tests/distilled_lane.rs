//! The distilled lane: `distilled/` cards answer beside the ranked page and
//! never inside it.
//!
//! The corpus is engineered so the card is the *best* lexical match for the
//! query — that is the only arrangement under which "the page is unchanged"
//! says anything. A card that could not have won a slot would leave the page
//! unchanged no matter how the lane were built, so the premise is asserted
//! against the store's own lexical arm before the page is.
//!
//! Lexical-only on purpose: the partition is a filter on the candidate
//! population, which both arms share, and `embed_search.rs` already covers
//! what the vector arm adds.

mod daemon_support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use daemon_support::Fixture;
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::full_scan;
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch;
use lore::embed::Embedder;
use lore::store::SearchFilter;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

/// Two ordinary words. Every document below contains both; the card contains
/// them the most often, in the least text.
const QUERY: &str = "ranking order";

struct Harness {
    fixture: Fixture,
    router: Router,
}

/// A project with **no** `.lore.toml`: card detection is the path and nothing
/// else, so it has to work in a repository where no frontmatter is parsed at
/// all (D-0012).
fn harness() -> Harness {
    let fixture = Fixture::neutral("demo");
    let (watch_tx, _watch_rx) = watch::channel();
    let state = AppState {
        store: fixture.store.clone(),
        queue: IndexQueue::new(),
        watch: watch_tx,
        watch_status: watch::WatchStatus::new(),
        index: fixture.context(),
        push: fixture.push_leases(),
        config: Arc::new(Config::default()),
        embeddings: Embedder::disabled(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        plugins: Arc::new(lore::plugin::PluginRegistry::empty()),
        plugin_diagnostics: Arc::new(Vec::new()),
        data_dir: fixture.data_dir.clone(),
        shutdown: fixture.cancel.clone(),
    };
    Harness {
        router: router(state),
        fixture,
    }
}

fn populate(fixture: &Fixture) {
    fixture.write(
        "docs/one.md",
        "# One\n\nThe daemon fixes a ranking, and the order it produces is stable \
         across runs of the same query over the same corpus.\n",
    );
    fixture.write(
        "docs/two.md",
        "# Two\n\nCollapse runs after ranking, so a windowed span holds one slot \
         in the order rather than several.\n",
    );
    // Short, dense, and about the same subject as the two files above: the
    // shape a distiller actually emits, and the shape that wins on BM25.
    fixture.write(
        "distilled/search.md",
        "# Ranking and order\n\nRanking decides order. Sources: `docs/one.md`, \
         `docs/two.md`.\n",
    );
    full_scan(&fixture.context(), &fixture.project);
}

async fn search(router: &Router, mut body: Value) -> Value {
    if body.get("project").is_none() {
        body["project"] = json!("demo");
    }
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("router never fails");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).expect("JSON body")
}

fn paths(body: &Value, field: &str) -> Vec<String> {
    body[field]
        .as_array()
        .unwrap_or_else(|| panic!("`{field}` array in {body:#}"))
        .iter()
        .map(|hit| hit["path"].as_str().unwrap().to_string())
        .collect()
}

/// What the lexical arm makes of the corpus with nothing partitioned away —
/// the premise every assertion below rests on.
fn unpartitioned_lexical_order(fixture: &Fixture) -> Vec<String> {
    let filter = SearchFilter::project(fixture.project.id);
    fixture
        .store
        .blocking(move |store| store.lexical_search(QUERY, &filter, 50))
        .expect("lexical arm")
        .into_iter()
        .map(|hit| hit.chunk.path.into_string())
        .collect()
}

#[tokio::test]
async fn a_card_answers_in_the_lane_and_leaves_the_page_exactly_as_it_found_it() {
    let h = harness();
    populate(&h.fixture);

    // The premise: the card outranks both source files on this query, so a
    // ranking that saw it would have seated it first.
    assert_eq!(
        unpartitioned_lexical_order(&h.fixture)
            .first()
            .map(String::as_str),
        Some("distilled/search.md"),
        "the fixture no longer contests the page; the assertions below would pass vacuously"
    );

    let lane = search(&h.router, json!({ "query": QUERY })).await;
    let off = search(&h.router, json!({ "query": QUERY, "distilled": "off" })).await;

    // The page is not merely card-free, it is the *same page* the corpus
    // would have produced with no `distilled/` directory in it at all — which
    // is what `off` computes.
    assert_eq!(paths(&lane, "results"), paths(&off, "results"), "{lane:#}");
    assert!(
        !paths(&lane, "results")
            .iter()
            .any(|path| path.starts_with("distilled/")),
        "{lane:#}"
    );
    assert_eq!(
        paths(&lane, "results"),
        ["docs/two.md", "docs/one.md"],
        "{lane:#}"
    );

    // Same scores, not just the same paths: the page's ranking is computed
    // over one population under both modes.
    let scores = |body: &Value| -> Vec<f64> {
        body["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["score"].as_f64().unwrap())
            .collect()
    };
    assert_eq!(scores(&lane), scores(&off), "{lane:#}");

    // And the card is not lost — it is answered, ranked, beside the page.
    assert_eq!(
        paths(&lane, "distilled"),
        ["distilled/search.md"],
        "{lane:#}"
    );
}

#[tokio::test]
async fn off_drops_cards_from_both_the_page_and_the_lane() {
    let h = harness();
    populate(&h.fixture);

    let body = search(&h.router, json!({ "query": QUERY, "distilled": "off" })).await;
    assert!(paths(&body, "distilled").is_empty(), "{body:#}");
    assert!(
        !paths(&body, "results")
            .iter()
            .any(|path| path.starts_with("distilled/")),
        "{body:#}"
    );
}

/// The guarantee that makes the lane a partition rather than a post-filter: a
/// card removed from the page frees the slot instead of blanking it.
#[tokio::test]
async fn a_contested_page_still_fills_from_source_material() {
    let h = harness();
    populate(&h.fixture);
    assert_eq!(
        unpartitioned_lexical_order(&h.fixture).len(),
        3,
        "all three documents match the query"
    );

    // Two slots and three matching documents, the best of which is a card:
    // post-filtering the card out of a two-result page would answer with one.
    let body = search(&h.router, json!({ "query": QUERY, "limit": 2 })).await;
    assert_eq!(
        paths(&body, "results"),
        ["docs/two.md", "docs/one.md"],
        "{body:#}"
    );
    assert_eq!(
        paths(&body, "distilled"),
        ["distilled/search.md"],
        "{body:#}"
    );
}

/// The lane is a router, not a second result set: it is capped, and the cap
/// is the daemon's, not the caller's.
#[tokio::test]
async fn the_lane_is_capped_at_three_cards() {
    let h = harness();
    populate(&h.fixture);
    for slug in ["a", "b", "c", "d"] {
        h.fixture.write(
            &format!("distilled/{slug}.md"),
            format!("# Area {slug}\n\nRanking and order in area {slug}.\n"),
        );
    }
    full_scan(&h.fixture.context(), &h.fixture.project);

    let body = search(&h.router, json!({ "query": QUERY, "limit": 50 })).await;
    assert_eq!(paths(&body, "distilled").len(), 3, "{body:#}");
    assert!(
        paths(&body, "distilled")
            .iter()
            .all(|path| path.starts_with("distilled/")),
        "{body:#}"
    );
    // Raising `limit` buys page breadth, never lane breadth.
    assert_eq!(paths(&body, "results"), ["docs/two.md", "docs/one.md"]);
}

/// A caller who scopes the query into `distilled/` is not fought: the
/// partition still applies mechanically, so the page is empty and the lane
/// carries what they asked for.
#[tokio::test]
async fn a_path_prefix_into_the_card_directory_is_answered_by_the_lane() {
    let h = harness();
    populate(&h.fixture);

    let body = search(
        &h.router,
        json!({ "query": QUERY, "path_prefix": "distilled/" }),
    )
    .await;
    assert!(paths(&body, "results").is_empty(), "{body:#}");
    assert_eq!(
        paths(&body, "distilled"),
        ["distilled/search.md"],
        "{body:#}"
    );
}

/// The caller's filters bind both lanes: a language the cards are not written
/// in empties the lane as surely as it empties the page.
#[tokio::test]
async fn caller_filters_apply_to_the_lane_too() {
    let h = harness();
    populate(&h.fixture);

    let body = search(&h.router, json!({ "query": QUERY, "path_prefix": "docs/" })).await;
    assert_eq!(paths(&body, "results"), ["docs/two.md", "docs/one.md"]);
    assert!(paths(&body, "distilled").is_empty(), "{body:#}");

    let body = search(&h.router, json!({ "query": QUERY, "language": "csharp" })).await;
    assert!(paths(&body, "results").is_empty(), "{body:#}");
    assert!(paths(&body, "distilled").is_empty(), "{body:#}");
}
