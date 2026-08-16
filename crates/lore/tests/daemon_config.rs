//! `config.toml` parsing. Config is the one thing a user edits by hand, so
//! its failure modes matter more than its happy path.

use lore::config::{Config, DEFAULT_BATCH_MAX_ITEMS, DEFAULT_EMBED_CONCURRENCY};
use lore_core::EmbeddingStatus;

/// Exactly the example from design/3_Retrieval/3.1 §"Embedding provider
/// config" — if this stops parsing, the documented shape and the implemented
/// shape have diverged.
const DOCUMENTED_EXAMPLE: &str = r#"
[embeddings]
endpoint = "http://127.0.0.1:8080/v1"   # OpenAI-compatible; llama-server default
model = "jina-code-embeddings-1.5b"
dimensions = 1536
query_prefix = ""                        # model-card instruction strings
document_prefix = ""
batch_max_items = 64
"#;

#[test]
fn documented_example_parses_field_for_field() {
    let config = Config::parse(DOCUMENTED_EXAMPLE).expect("documented example must parse");
    let embeddings = &config.embeddings;
    assert_eq!(
        embeddings.endpoint.as_deref(),
        Some("http://127.0.0.1:8080/v1")
    );
    assert_eq!(
        embeddings.model.as_deref(),
        Some("jina-code-embeddings-1.5b")
    );
    assert_eq!(embeddings.dimensions, Some(1536));
    assert_eq!(embeddings.query_prefix, "");
    assert_eq!(embeddings.document_prefix, "");
    assert_eq!(embeddings.batch_max_items, 64);
    // Not in the documented example, and adding a key must stay additive for
    // files written before it existed.
    assert_eq!(embeddings.concurrency, DEFAULT_EMBED_CONCURRENCY);
}

#[test]
fn concurrency_is_optional_and_defaults_to_several_batches_in_flight() {
    assert_eq!(DEFAULT_EMBED_CONCURRENCY, 4);
    let config = Config::parse("[embeddings]\nconcurrency = 1\n").expect("an explicit 1 parses");
    assert_eq!(config.embeddings.concurrency, 1);
    let config = Config::parse("[embeddings]\nconcurrency = 0\n").expect("a zero parses");
    // Clamped where it is used, not at parse time, so the file round-trips.
    assert_eq!(config.embeddings.concurrency, 0);
}

#[test]
fn empty_file_and_missing_file_are_the_same_thing() {
    let empty = Config::parse("").expect("empty config is valid");
    assert_eq!(empty, Config::default());

    let dir = tempfile::tempdir().unwrap();
    let data_dir = camino::Utf8Path::from_path(dir.path()).unwrap();
    let missing = Config::load(data_dir).expect("absent config is valid");
    assert_eq!(missing, empty);

    assert_eq!(missing.embeddings.batch_max_items, DEFAULT_BATCH_MAX_ITEMS);
    assert_eq!(missing.embeddings.concurrency, DEFAULT_EMBED_CONCURRENCY);
    assert!(missing.embeddings.endpoint.is_none());
}

#[test]
fn partial_section_keeps_defaults_for_the_rest() {
    let config = Config::parse("[embeddings]\nendpoint = \"http://localhost:9000/v1\"\n").unwrap();
    assert_eq!(
        config.embeddings.endpoint.as_deref(),
        Some("http://localhost:9000/v1")
    );
    assert_eq!(config.embeddings.batch_max_items, DEFAULT_BATCH_MAX_ITEMS);
    assert_eq!(config.embeddings.model, None);
}

#[test]
fn a_typo_is_an_error_not_a_silent_default() {
    let err = Config::parse("[embeddings]\nendpont = \"http://x\"\n")
        .expect_err("misspelled keys must not be ignored");
    assert!(
        format!("{err}").contains("endpont"),
        "error should name the offending key: {err}"
    );

    let err = Config::parse("[embedings]\n").expect_err("misspelled sections too");
    assert!(format!("{err}").contains("embedings"), "{err}");
}

#[test]
fn malformed_toml_fails_loudly_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = camino::Utf8Path::from_path(dir.path()).unwrap();
    std::fs::write(data_dir.join("config.toml"), "[embeddings\n").unwrap();
    let err = Config::load(data_dir).expect_err("truncated table header");
    assert!(format!("{err:#}").contains("config.toml"), "{err:#}");
}

#[test]
fn embedding_status_reflects_configuration() {
    let none = Config::default();
    assert!(matches!(
        none.embedding_status(),
        EmbeddingStatus::Unconfigured
    ));

    let configured = Config::parse(DOCUMENTED_EXAMPLE).unwrap();
    match configured.embedding_status() {
        // Configured but no client yet: reported as degraded, never as Ready.
        EmbeddingStatus::Unreachable { endpoint, error } => {
            assert_eq!(endpoint, "http://127.0.0.1:8080/v1");
            assert!(!error.is_empty());
        }
        other => panic!("expected a visibly degraded status, got {other:?}"),
    }
}
