//! Two real `lore daemon` *processes* racing one data directory — the
//! process-level proof behind D-0003 that in-process tests cannot give:
//! kernel lock arbitration between real PIDs, and release on a hard kill
//! (`TerminateProcess`), which no drop/cleanup code gets to run for.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use camino::Utf8Path;
use lore::daemon::{DATA_DIR_ENV, discover, ownership};

/// Generous: each starter cold-opens SQLite and runs migrations, and CI
/// machines are slow at exactly that.
const PATIENCE: Duration = Duration::from_secs(30);

fn spawn_daemon(data_dir: &std::path::Path) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_lore"))
        .arg("daemon")
        .env(DATA_DIR_ENV, data_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lore daemon")
}

#[test]
fn simultaneous_processes_admit_exactly_one_and_a_kill_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let data = Utf8Path::from_path(dir.path()).expect("utf-8 temp dir");

    let mut a = spawn_daemon(dir.path());
    let mut b = spawn_daemon(dir.path());

    // Exactly one process loses the lock race and exits; whichever it is.
    let deadline = Instant::now() + PATIENCE;
    let (mut winner, loser) = loop {
        assert!(
            Instant::now() < deadline,
            "neither process gave up the data dir"
        );
        if a.try_wait().unwrap().is_some() {
            break (b, a);
        }
        if b.try_wait().unwrap().is_some() {
            break (a, b);
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let refused = loser.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "the loser exits nonzero: {stderr}"
    );
    assert!(stderr.contains("already running"), "stderr: {stderr}");

    // The winner is genuinely serving: it publishes a discovery record and
    // stays alive to heartbeat it.
    let deadline = Instant::now() + PATIENCE;
    while discover(data).ok().flatten().is_none() {
        assert!(
            winner.try_wait().unwrap().is_none(),
            "the winning daemon died before publishing"
        );
        assert!(Instant::now() < deadline, "the winner never published");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Kill — not shut down. No Rust drop runs in the winner; only the kernel
    // cleans up. A crashed daemon must never strand the machine (the exact
    // fear that once argued against an OS lock, answered).
    winner.kill().unwrap();
    winner.wait().unwrap();
    ownership::acquire(data).expect("a killed owner's lock is released by the kernel");
}
