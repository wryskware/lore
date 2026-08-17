//! The whole daemon, started for real: `run` binds a port, publishes the
//! handshake, serves `/v1` over loopback, and withdraws the handshake on the
//! way out.
//!
//! These are the only tests that use a real socket, because the thing under
//! test *is* the wiring — startup order, discovery, and the mutual exclusion
//! between two daemons over one data directory.

use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use lore::daemon::{DaemonOptions, discover, handshake, run};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Generous: a cold start compiles nothing but does cold-open SQLite and
/// create its schema, and CI machines are slow at exactly that.
const PATIENCE: Duration = Duration::from_secs(30);

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8Path::from_path(dir.path())
        .expect("utf-8 temp dir")
        .to_owned()
}

/// Wait for the daemon to publish its handshake.
async fn await_handshake(data_dir: &Utf8Path) -> handshake::Handshake {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(record)) = discover(data_dir) {
            return record;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not publish a handshake within {PATIENCE:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_starts_serves_and_withdraws_its_handshake() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("notes.md"),
        "# Notes\n\nfindablekeyword\n",
    )
    .unwrap();

    let data_dir = utf8(&data);
    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(run(DaemonOptions {
        data_dir: data_dir.clone(),
        shutdown: shutdown.clone(),
    }));

    let record = await_handshake(&data_dir).await;
    assert_eq!(record.pid, std::process::id());
    assert_eq!(record.api_version, lore_core::API_VERSION);
    assert_ne!(record.port, 0, "an ephemeral port was actually assigned");

    // Discovery is enough to talk to it: that is the contract the CLI and
    // lore-mcp depend on.
    let client = reqwest::Client::new();
    let base = record.base_url();
    let status: lore_core::DaemonStatus = client
        .get(format!("{base}/status"))
        .send()
        .await
        .expect("status request")
        .json()
        .await
        .expect("status body");
    assert_eq!(status.api_version, lore_core::API_VERSION);
    assert!(status.projects.is_empty());

    // Register → the daemon indexes without any further prompting.
    let registered: lore_core::ProjectInfo = client
        .post(format!("{base}/projects"))
        .json(&lore_core::RegisterProjectRequest {
            root: project.path().to_string_lossy().into_owned(),
            name: Some("fixture".into()),
        })
        .send()
        .await
        .expect("register request")
        .json()
        .await
        .expect("register body");
    assert_eq!(registered.name, "fixture");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let found = loop {
        let response: lore_core::SearchResponse = client
            .post(format!("{base}/search"))
            .json(&lore_core::SearchRequest {
                query: "findablekeyword".into(),
                // Every query is scoped to one project now; this daemon has
                // exactly one and the test is about indexing, not scoping.
                project: Some("fixture".into()),
                ..Default::default()
            })
            .send()
            .await
            .expect("search request")
            .json()
            .await
            .expect("search body");
        if !response.results.is_empty() {
            break response;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the registered project was never indexed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(found.lexical_only);
    assert_eq!(found.results[0].path, "notes.md");

    shutdown.cancel();
    tokio::time::timeout(PATIENCE, daemon)
        .await
        .expect("daemon exits promptly on shutdown")
        .expect("daemon task did not panic")
        .expect("daemon returned Ok");

    assert_eq!(
        discover(&data_dir).unwrap(),
        None,
        "a clean shutdown withdraws the handshake"
    );
    // The port must actually be released, not merely forgotten.
    assert!(!handshake::probe(record.port).await);
}

/// Issue #8, end to end: a daemon stopped over the API removes its handshake,
/// releases its port, and lets a fresh one start immediately — where a killed
/// daemon leaves a fresh-looking record that costs every client up to
/// `STALE_AFTER` before they stop believing it.
///
/// The real `lore stop` also *waits* for exactly what this asserts; the wait
/// itself is unit-tested in `cli.rs`, since it needs a data directory the CLI
/// discovers for itself.
#[tokio::test(flavor = "multi_thread")]
async fn the_shutdown_endpoint_stops_the_daemon_and_withdraws_its_handshake() {
    let data = tempfile::tempdir().unwrap();
    let data_dir = utf8(&data);

    let daemon = tokio::spawn(run(DaemonOptions::new(data_dir.clone())));
    let record = await_handshake(&data_dir).await;

    let ack: lore_core::ShutdownResponse = reqwest::Client::new()
        .post(format!("{}/shutdown", record.base_url()))
        .send()
        .await
        .expect("the shutdown request is answered, not dropped")
        .json()
        .await
        .expect("shutdown body");
    assert_eq!(ack.pid, record.pid);

    tokio::time::timeout(PATIENCE, daemon)
        .await
        .expect("the daemon exits on its own after acknowledging")
        .expect("daemon task did not panic")
        .expect("daemon returned Ok");

    assert_eq!(
        discover(&data_dir).unwrap(),
        None,
        "the handshake is withdrawn, so no client follows it to a dead port"
    );
    assert!(!handshake::probe(record.port).await);

    // The whole point: a successor starts at once, rather than waiting out a
    // stale window it should never have had to.
    let shutdown = CancellationToken::new();
    let next = tokio::spawn(run(DaemonOptions {
        data_dir: data_dir.clone(),
        shutdown: shutdown.clone(),
    }));
    let restarted = await_handshake(&data_dir).await;
    assert_ne!(restarted.port, 0);
    shutdown.cancel();
    tokio::time::timeout(PATIENCE, next)
        .await
        .expect("successor exits")
        .unwrap()
        .unwrap();
}

/// One owner per data directory (D-0003). The second daemon must fail to
/// start rather than open the same database.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_daemon_on_the_same_data_dir_refuses_to_start() {
    let data = tempfile::tempdir().unwrap();
    let data_dir = utf8(&data);

    let shutdown = CancellationToken::new();
    let first = tokio::spawn(run(DaemonOptions {
        data_dir: data_dir.clone(),
        shutdown: shutdown.clone(),
    }));
    let record = await_handshake(&data_dir).await;

    let err = run(DaemonOptions::new(data_dir.clone()))
        .await
        .expect_err("a second owner must be refused");
    let message = format!("{err}");
    assert!(message.contains("already running"), "{message}");
    assert!(message.contains(&record.port.to_string()), "{message}");

    // The refusal must not have disturbed the incumbent's handshake.
    assert_eq!(discover(&data_dir).unwrap().as_ref(), Some(&record));

    shutdown.cancel();
    tokio::time::timeout(PATIENCE, first)
        .await
        .expect("first daemon exits")
        .unwrap()
        .unwrap();

    // Once it is gone, a fresh daemon starts cleanly on the same directory.
    let shutdown = CancellationToken::new();
    let third = tokio::spawn(run(DaemonOptions {
        data_dir: data_dir.clone(),
        shutdown: shutdown.clone(),
    }));
    let restarted = await_handshake(&data_dir).await;
    assert_ne!(restarted.started_at + i64::from(restarted.port), 0);
    shutdown.cancel();
    tokio::time::timeout(PATIENCE, third)
        .await
        .expect("restarted daemon exits")
        .unwrap()
        .unwrap();
}

/// A daemon that was down while the disk changed must reconcile on restart —
/// registrations survive, and so does the indexed state they imply.
#[tokio::test(flavor = "multi_thread")]
async fn registrations_and_index_state_survive_a_restart() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let data_dir = utf8(&data);
    std::fs::write(project.path().join("first.md"), "# One\n\noriginalword\n").unwrap();

    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(run(DaemonOptions {
        data_dir: data_dir.clone(),
        shutdown: shutdown.clone(),
    }));
    let record = await_handshake(&data_dir).await;
    let client = reqwest::Client::new();
    client
        .post(format!("{}/projects", record.base_url()))
        .json(&lore_core::RegisterProjectRequest {
            root: project.path().to_string_lossy().into_owned(),
            name: Some("fixture".into()),
        })
        .send()
        .await
        .unwrap();
    shutdown.cancel();
    tokio::time::timeout(PATIENCE, daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Changed while nobody was watching.
    std::fs::remove_file(project.path().join("first.md")).unwrap();
    std::fs::write(
        project.path().join("second.md"),
        "# Two\n\nreplacementword\n",
    )
    .unwrap();

    let shutdown = CancellationToken::new();
    let daemon = tokio::spawn(run(DaemonOptions {
        data_dir: data_dir.clone(),
        shutdown: shutdown.clone(),
    }));
    let record = await_handshake(&data_dir).await;
    let base = record.base_url();

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let status: lore_core::DaemonStatus = client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status.projects.len(), 1, "the registration survived");
        let search = |query: &str| {
            let client = client.clone();
            let base = base.clone();
            let query = query.to_string();
            async move {
                client
                    .post(format!("{base}/search"))
                    .json(&lore_core::SearchRequest {
                        query,
                        project: Some("fixture".into()),
                        ..Default::default()
                    })
                    .send()
                    .await
                    .unwrap()
                    .json::<lore_core::SearchResponse>()
                    .await
                    .unwrap()
            }
        };
        let gone = search("originalword").await.results.is_empty();
        let arrived = !search("replacementword").await.results.is_empty();
        if gone && arrived {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the startup rescan never reconciled offline changes"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    shutdown.cancel();
    tokio::time::timeout(PATIENCE, daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
