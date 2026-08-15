//! The ownership lock is the admission primitive (D-0003): a held OS lock,
//! exclusive per data directory, released by the kernel on process death and
//! by nothing else. The discovery record cannot grant, destroy, or fake
//! ownership — these tests delete it, corrupt it, and timestamp it from the
//! future to prove that.

use std::time::Duration;

use camino::Utf8Path;
use lore::daemon::handshake::{self, Handshake};
use lore::daemon::{StoreHandle, ownership};
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

#[test]
fn exactly_one_acquire_per_data_dir_and_drop_releases() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);

    let first = ownership::acquire(data).expect("a clean dir admits the first starter");
    let err = ownership::acquire(data).expect_err("a second owner must be refused");
    assert!(format!("{err}").contains("already running"), "{err}");

    drop(first);
    ownership::acquire(data).expect("dropping the guard releases the lock");
}

#[test]
fn simultaneous_starters_admit_exactly_one() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir).to_owned();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let starters: Vec<_> = (0..4)
        .map(|_| {
            let data = data.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ownership::acquire(&data)
            })
        })
        .collect();

    let results: Vec<_> = starters.into_iter().map(|t| t.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        winners, 1,
        "exactly one simultaneous starter may own the index"
    );
}

/// The failure Session 2 called out: an incumbent must not lose exclusivity
/// because something deleted or corrupted its discovery record.
#[test]
fn a_live_owner_survives_deleted_and_corrupt_discovery_records() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);
    let guard = ownership::acquire(data).unwrap();
    handshake::write(data, &record(4242, 555, handshake::unix_now())).unwrap();

    std::fs::remove_file(data.join(handshake::HANDSHAKE_FILE)).unwrap();
    let err = ownership::acquire(data).expect_err("a deleted record changes nothing");
    assert!(format!("{err}").contains("already running"), "{err}");

    std::fs::write(data.join(handshake::HANDSHAKE_FILE), "{not json").unwrap();
    ownership::acquire(data).expect_err("a corrupt record changes nothing");

    drop(guard);
    ownership::acquire(data).expect("only the guard's release ends ownership");
}

#[test]
fn refusal_names_the_incumbent_when_its_record_is_readable() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);
    let _guard = ownership::acquire(data).unwrap();
    handshake::write(data, &record(4242, 555, handshake::unix_now())).unwrap();

    let message = format!("{}", ownership::acquire(data).expect_err("refused"));
    assert!(message.contains("4242"), "names the pid: {message}");
    assert!(message.contains("555"), "names the port: {message}");
}

/// A crashed daemon's record can look fresh (it died between beats) or even
/// come from the future (the clock stepped back under it). Neither delays a
/// restart by a second: the dead process's lock is already released. This is
/// the behavior that replaced the 45-second freshness gate.
#[test]
fn admission_ignores_heartbeat_freshness_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);

    handshake::write(data, &record(999_999, 1, handshake::unix_now())).unwrap();
    drop(ownership::acquire(data).expect("a fresh record from a dead process must not block"));

    handshake::write(data, &record(999_999, 1, handshake::unix_now() + 3_600)).unwrap();
    ownership::acquire(data).expect("a future heartbeat from a dead process must not block");
}

/// Shutdown safety (Session 2, finding 1): a blocking store call that a
/// shutdown timeout left behind still holds the ownership lock through the
/// clone of the handle it captured, so no successor can open the store while
/// it can still write.
#[test]
fn ownership_outlives_detached_store_work() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = data_dir(&dir);

    let guard = ownership::acquire(data).unwrap();
    let store = StoreHandle::open(data.join("lore.db"))
        .unwrap()
        .with_owner(guard);

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let task = {
        let store = store.clone();
        runtime.spawn(async move {
            store
                .with(move |_store| {
                    entered_tx.send(()).unwrap();
                    // An uncancellable blocking call, mid-flight.
                    release_rx.recv_timeout(Duration::from_secs(30)).unwrap();
                })
                .await
        })
    };
    entered_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the blocking call started");

    // The daemon's own handle is gone — this is `run` having returned after
    // its shutdown grace expired.
    drop(store);
    ownership::acquire(data).expect_err("a detached blocking store call must still hold ownership");

    release_tx.send(()).unwrap();
    runtime.block_on(task).unwrap().unwrap();
    ownership::acquire(data).expect("ownership ends when the last writer stops");
}
