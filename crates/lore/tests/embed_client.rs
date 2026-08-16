//! The embedding client against a real (stub) HTTP server.
//!
//! The theme is failure classification: which HTTP outcomes are worth
//! retrying, which are permanent, and — most importantly — that a vector is
//! never attached to the wrong input.

mod embed_support;

use std::time::{Duration, Instant};

use embed_support::{POISON, Reply, Stub, settings, stub_vector};
use lore::embed::EmbedClient;
use lore::embed::client::{BATCH_MAX_BYTES, batches};
use lore::embed::text::MAX_EMBED_TEXT_BYTES;

fn texts(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

/// A response that arrives out of order must still be reassembled by `index`.
/// Getting this wrong silently attaches every vector to the wrong chunk —
/// the worst failure a retrieval index can have, because nothing downstream
/// can detect it.
#[tokio::test]
async fn responses_are_reordered_by_index_not_by_arrival() {
    let stub = Stub::shuffled().await;
    let client = EmbedClient::new(settings(&stub.base)).unwrap();

    let inputs = texts(&["alpha ranking", "beta json", "gamma watcher", "delta"]);
    let vectors = client.embed(&inputs).await.expect("stub answers");

    assert_eq!(vectors.len(), inputs.len());
    for (input, vector) in inputs.iter().zip(&vectors) {
        assert_eq!(vector, &stub_vector(input), "vector paired with {input:?}");
    }
    stub.shutdown().await;
}

#[tokio::test]
async fn batches_split_on_the_byte_budget_and_results_stay_in_order() {
    let stub = Stub::start().await;
    let mut config = settings(&stub.base);
    // Item count out of the way; only the byte budget may split here.
    config.batch_max_items = 1000;
    let client = EmbedClient::new(config).unwrap();

    // 7 KiB each: 9 fit inside the 64 KiB budget, the 10th starts a batch.
    let inputs: Vec<String> = (0..20)
        .map(|i| format!("doc{i} ") + &"x".repeat(7 * 1024))
        .collect();
    let vectors = client.embed(&inputs).await.expect("stub answers");
    assert_eq!(vectors.len(), 20);

    let seen = stub.state.batches();
    assert!(seen.len() > 1, "the byte budget must have split something");
    for batch in &seen {
        let bytes: usize = batch.iter().map(String::len).sum();
        assert!(bytes <= BATCH_MAX_BYTES, "batch of {bytes} bytes");
    }
    // Wire batches go out concurrently, so their *arrival* order is the
    // server's business; what splitting must never do is drop, duplicate, or
    // mis-pair anything. Every input exactly once…
    let mut sent = stub.state.inputs();
    sent.sort();
    let mut expected = inputs.clone();
    expected.sort();
    assert_eq!(sent, expected);
    // …and every returned vector attached to its own input.
    for (input, vector) in inputs.iter().zip(&vectors) {
        assert_eq!(vector, &stub_vector(input));
    }
    stub.shutdown().await;
}

/// One `embed` call whose text overflows the wire budget must have its wire
/// batches on the server *simultaneously* — the serial version left the
/// server idle for a full round trip per batch. Pairing is asserted with
/// inputs from distinct synonym groups, whose stub vectors provably differ.
#[tokio::test]
async fn wire_batches_of_one_call_overlap_and_stay_paired() {
    let stub = Stub::start().await;
    stub.state.set_delay(Duration::from_millis(200));
    let mut config = settings(&stub.base);
    // One input per wire batch: three inputs, three concurrent POSTs.
    config.batch_max_items = 1;
    let client = EmbedClient::new(config).unwrap();

    let inputs = texts(&["ranking fusion", "json payload", "watcher debounce"]);
    let vectors = client.embed(&inputs).await.expect("stub answers");

    assert!(
        stub.state.max_in_flight() >= 2,
        "wire batches were serial: max {} in flight",
        stub.state.max_in_flight()
    );
    for (input, vector) in inputs.iter().zip(&vectors) {
        assert_eq!(vector, &stub_vector(input), "vector paired with {input:?}");
    }
    stub.shutdown().await;
}

#[tokio::test]
async fn oversized_inputs_are_clipped_for_embedding_only() {
    let stub = Stub::start().await;
    let client = EmbedClient::new(settings(&stub.base)).unwrap();

    let huge = "y".repeat(MAX_EMBED_TEXT_BYTES * 3);
    client
        .embed(std::slice::from_ref(&huge))
        .await
        .expect("stub answers");

    let sent = stub.state.inputs();
    assert_eq!(sent[0].len(), MAX_EMBED_TEXT_BYTES);
    assert!(huge.starts_with(&sent[0]), "the head is kept, not the tail");
    stub.shutdown().await;
}

#[tokio::test]
async fn a_server_error_is_retried_and_then_succeeds() {
    let stub = Stub::start().await;
    stub.state.script([Reply::Status(500)]);
    let client = EmbedClient::new(settings(&stub.base)).unwrap();

    let vectors = client.embed(&texts(&["ranking"])).await.expect("retried");
    assert_eq!(vectors[0], stub_vector("ranking"));
    assert_eq!(stub.state.attempts(), 2, "one failure, one success");
    stub.shutdown().await;
}

#[tokio::test]
async fn retries_are_bounded_and_end_as_a_transient_error() {
    let stub = Stub::start().await;
    // More scripted failures than the policy allows attempts.
    stub.state
        .script(std::iter::repeat_n(Reply::Status(503), 10));
    let client = EmbedClient::new(settings(&stub.base)).unwrap();

    let err = client
        .embed(&texts(&["ranking"]))
        .await
        .expect_err("every attempt failed");
    assert!(!err.is_permanent(), "5xx exhaustion is transient: {err}");
    assert_eq!(stub.state.attempts(), 4, "max_attempts, not more");
    stub.shutdown().await;
}

/// A poison input must not be retried at all, or one bad chunk wedges the
/// whole pipeline behind it.
#[tokio::test]
async fn a_client_error_is_permanent_and_never_retried() {
    let stub = Stub::start().await;
    let client = EmbedClient::new(settings(&stub.base)).unwrap();

    let err = client
        .embed(&texts(&[POISON]))
        .await
        .expect_err("the stub refuses this input");
    assert!(err.is_permanent(), "{err}");
    assert_eq!(stub.state.attempts(), 1, "no retries on a 4xx");
    stub.shutdown().await;
}

#[tokio::test]
async fn retry_after_is_honoured_in_preference_to_the_backoff() {
    let stub = Stub::start().await;
    stub.state.script([Reply::StatusRetryAfter(429, 1)]);

    let mut config = settings(&stub.base);
    // The policy's own backoff is 5ms; only an honoured `Retry-After` can
    // make this call take about a second.
    config.retry.max_delay = Duration::from_secs(5);
    let client = EmbedClient::new(config).unwrap();

    let started = Instant::now();
    client.embed(&texts(&["ranking"])).await.expect("retried");
    let elapsed = started.elapsed();

    assert!(elapsed >= Duration::from_millis(900), "waited {elapsed:?}");
    assert!(elapsed < Duration::from_secs(3), "waited {elapsed:?}");
    assert_eq!(stub.state.attempts(), 2);
    stub.shutdown().await;
}

/// `Retry-After` is still clamped: the daemon's shutdown grace is finite, and
/// a server asking for an hour must not turn into an hour-long hang.
#[tokio::test]
async fn retry_after_cannot_exceed_the_policy_ceiling() {
    let stub = Stub::start().await;
    stub.state.script([Reply::StatusRetryAfter(429, 3600)]);
    let client = EmbedClient::new(settings(&stub.base)).unwrap();

    let started = Instant::now();
    client.embed(&texts(&["ranking"])).await.expect("retried");
    assert!(started.elapsed() < Duration::from_secs(2));
    stub.shutdown().await;
}

#[tokio::test]
async fn an_unreachable_endpoint_is_transient_not_permanent() {
    let stub = Stub::start().await;
    let base = stub.base.clone();
    stub.shutdown().await;

    let mut config = settings(&base);
    // A dead loopback port does not always refuse promptly on Windows — the
    // SYN can simply go unanswered — so keep this to one short attempt.
    config.retry.max_attempts = 1;
    config.request_timeout = Duration::from_millis(500);

    let client = EmbedClient::new(config).unwrap();
    let err = client
        .embed(&texts(&["ranking"]))
        .await
        .expect_err("no server");
    assert!(!err.is_permanent(), "{err}");
    assert!(client.probe().await.is_err());
}

#[tokio::test]
async fn a_declared_dimension_mismatch_is_a_permanent_error() {
    let stub = Stub::start().await;
    let mut config = settings(&stub.base);
    config.dimensions = Some(1536); // the stub returns 16
    let client = EmbedClient::new(config).unwrap();

    let err = client.embed(&texts(&["ranking"])).await.expect_err("width");
    assert!(err.is_permanent(), "{err}");
    assert!(err.to_string().contains("1536"), "{err}");
    stub.shutdown().await;
}

#[tokio::test]
async fn probing_reports_the_observed_width() {
    let stub = Stub::start().await;
    let client = EmbedClient::new(settings(&stub.base)).unwrap();
    assert_eq!(client.probe().await.unwrap(), embed_support::DIMS);
    // The probe is one cheap call, not a corpus sweep.
    assert_eq!(stub.state.attempts(), 1);
    assert_eq!(stub.state.batches()[0].len(), 1);
    stub.shutdown().await;
}

#[tokio::test]
async fn an_empty_request_never_reaches_the_network() {
    let stub = Stub::start().await;
    let client = EmbedClient::new(settings(&stub.base)).unwrap();
    assert!(client.embed(&[]).await.unwrap().is_empty());
    assert_eq!(stub.state.attempts(), 0);
    stub.shutdown().await;
}

#[test]
fn batching_is_reachable_and_bounded_by_both_budgets() {
    let long = "z".repeat(BATCH_MAX_BYTES);
    let items: Vec<&str> = vec![long.as_str(), "a", "b"];
    let split = batches(&items, 8, BATCH_MAX_BYTES);
    assert_eq!(split.len(), 2);
    assert_eq!(split[1], ["a", "b"]);
}
