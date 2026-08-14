//! Single-instance handshake: freshness policy, atomic publication, and the
//! two ways a second daemon can be refused.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::Utf8Path;
use lore::daemon::handshake::{
    self, Handshake, STALE_AFTER, is_fresh, preflight, read, remove_if_owned_by, write,
};
use tempfile::TempDir;

fn data_dir(dir: &TempDir) -> &Utf8Path {
    Utf8Path::from_path(dir.path()).expect("utf-8 temp dir")
}

fn record(pid: u32, port: u16, heartbeat_at: i64) -> Handshake {
    Handshake {
        pid,
        port,
        api_version: lore_core::API_VERSION,
        daemon_version: "0.1.0".into(),
        started_at: heartbeat_at,
        heartbeat_at,
    }
}

/// A port nothing is listening on: bind, learn the number, drop the listener.
/// If the OS hands that port to an unrelated process a moment later, the
/// probe still fails — it requires a `/v1/status` body it can deserialize.
fn dead_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// ---------------------------------------------------------------------------
// Freshness is a pure function of (record, now) — no sleeping in tests.
// ---------------------------------------------------------------------------

#[test]
fn freshness_boundary_is_exactly_the_stale_window() {
    let stale_after = STALE_AFTER.as_secs() as i64;
    let now = 1_000_000;
    let beat = record(1, 1, now);

    assert!(is_fresh(&beat, now), "a heartbeat from this instant");
    assert!(is_fresh(&beat, now + stale_after - 1), "one second inside");
    assert!(
        !is_fresh(&beat, now + stale_after),
        "the window is exclusive at its end"
    );
    assert!(!is_fresh(&beat, now + 3600));
}

#[test]
fn a_heartbeat_from_the_future_counts_as_fresh() {
    // Clock adjustment or VM resume. "Cannot tell" must resolve to "do not
    // start a second owner", never to "take over".
    let beat = record(1, 1, 2_000_000);
    assert!(is_fresh(&beat, 1_000_000));
}

// ---------------------------------------------------------------------------
// Publication
// ---------------------------------------------------------------------------

#[test]
fn write_then_read_round_trips_and_leaves_no_scratch_file() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);

    assert_eq!(read(data).unwrap(), None, "nothing published yet");

    let published = record(4242, 53_412, 1_700_000_000);
    write(data, &published).unwrap();
    assert_eq!(read(data).unwrap().as_ref(), Some(&published));

    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "daemon.json")
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

#[test]
fn overwriting_replaces_the_previous_record() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);
    write(data, &record(1, 1, 100)).unwrap();
    write(data, &record(2, 2, 200)).unwrap();
    let current = read(data).unwrap().unwrap();
    assert_eq!(
        (current.pid, current.port, current.heartbeat_at),
        (2, 2, 200)
    );
}

/// The point of temp-file-plus-rename: a reader racing a writer sees the old
/// complete record or the new complete record, never a truncated one.
///
/// A failure here is a real failure (a partial parse), not a flake; the loop
/// only affects how likely the race is to be *sampled*.
#[test]
fn readers_racing_a_writer_never_observe_a_partial_record() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir).to_owned();
    write(&data, &record(1, 1, 0)).unwrap();

    let reads = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let data = data.clone();
            let reads = Arc::clone(&reads);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // `Ok(None)` is impossible after the first write, but a
                    // *corrupt* parse would be an `Err` — which is the bug
                    // this test exists to catch.
                    let observed = read(&data).expect("handshake must always parse");
                    assert!(observed.is_some());
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    for i in 0..300 {
        write(&data, &record(i as u32, i as u16, i as i64)).unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().unwrap();
    }
    assert!(reads.load(Ordering::Relaxed) > 0, "readers actually ran");
}

#[test]
fn withdrawal_only_touches_our_own_record() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);
    write(data, &record(4242, 1, 0)).unwrap();

    assert!(
        !remove_if_owned_by(data, 9999).unwrap(),
        "another daemon's record is not ours to delete"
    );
    assert!(read(data).unwrap().is_some());

    assert!(remove_if_owned_by(data, 4242).unwrap());
    assert_eq!(read(data).unwrap(), None);

    assert!(
        !remove_if_owned_by(data, 4242).unwrap(),
        "removing twice is not an error"
    );
}

// ---------------------------------------------------------------------------
// Startup gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_clean_data_dir_starts() {
    let dir = tempfile::tempdir().unwrap();
    preflight(data_dir(&dir))
        .await
        .expect("nothing to conflict");
}

#[tokio::test]
async fn a_fresh_heartbeat_refuses_startup_without_touching_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);
    // Port 1 is not listening; a fresh heartbeat must be believed anyway, so
    // this also proves the fresh path never probes.
    write(data, &record(4242, 1, handshake::unix_now())).unwrap();

    let err = preflight(data).await.expect_err("must refuse");
    let message = format!("{err}");
    assert!(message.contains("already running"), "{message}");
    assert!(message.contains("4242"), "names the pid: {message}");
}

#[tokio::test]
async fn a_stale_record_from_a_dead_daemon_is_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);
    let stale = handshake::unix_now() - STALE_AFTER.as_secs() as i64 - 60;
    // pid 999_999 is not a live process, and nothing answers on the port.
    write(data, &record(999_999, dead_port(), stale)).unwrap();

    preflight(data).await.expect("stale record must not block");
}

#[tokio::test]
async fn a_corrupt_record_is_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);
    std::fs::write(data.join("daemon.json"), "{not json").unwrap();
    preflight(data)
        .await
        .expect("corrupt record must not block");
}

/// The hard case: heartbeat says the daemon died, but it is actually alive
/// and serving (its heartbeat task was starved, the machine slept, …).
/// Starting a second owner of the index here is exactly what D-0003 forbids.
#[tokio::test]
async fn a_stale_heartbeat_still_loses_to_a_daemon_that_answers() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = axum::Router::new().route(
        "/v1/status",
        axum::routing::get(|| async {
            axum::Json(lore_core::DaemonStatus {
                api_version: lore_core::API_VERSION,
                daemon_version: "0.1.0".into(),
                generation: 7,
                projects: Vec::new(),
                embeddings: lore_core::EmbeddingStatus::Unconfigured,
            })
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });

    let stale = handshake::unix_now() - STALE_AFTER.as_secs() as i64 - 60;
    write(data, &record(4242, port, stale)).unwrap();

    let err = preflight(data).await.expect_err("a live daemon wins");
    let message = format!("{err}");
    assert!(message.contains("already running"), "{message}");
    assert!(message.contains("still answers"), "{message}");

    server.abort();
}

/// …and the mirror image: something else squatting on the recycled port must
/// not be able to veto startup forever.
#[tokio::test]
async fn a_stale_port_recycled_by_a_stranger_does_not_block_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = axum::Router::new().route(
        "/v1/status",
        axum::routing::get(|| async { "I am not a lore daemon" }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });

    let stale = handshake::unix_now() - STALE_AFTER.as_secs() as i64 - 60;
    write(data, &record(4242, port, stale)).unwrap();

    preflight(data)
        .await
        .expect("a non-lore responder is not proof of life");

    server.abort();
}
