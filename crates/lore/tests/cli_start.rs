//! `lore start` as a real process pair: the CLI spawns a detached `lore
//! daemon`, waits for it to answer, and says so — and a second `lore start`
//! finds it rather than racing it.
//!
//! In-process tests cannot cover this. The whole point of the command is what
//! happens *between* two OS processes: detached stdio landing in a log file,
//! the child outliving the parent that spawned it, and the handshake the
//! parent polls being published by someone else.
//!
//! `Command::output()` is also the regression test for the bug that wrote
//! `cli::detach`'s handle-inheritance comment. Capturing output means talking
//! to `lore start` over a *pipe*, and a daemon that inherits the write end of
//! that pipe keeps it open for as long as it runs — so these tests hang
//! forever rather than fail, which is exactly how the bug presented.

use std::process::{Command, Output};
use std::time::Duration;

use camino::Utf8Path;
use lore::daemon::{DATA_DIR_ENV, discover};

/// Generous for the same reason the other process tests are: a cold start
/// opens SQLite and creates its schema, and CI machines are slow at that.
const PATIENCE: Duration = Duration::from_secs(60);

fn lore(data_dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lore"))
        .args(args)
        .env(DATA_DIR_ENV, data_dir)
        .output()
        .expect("run the lore binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every test here leaves a live daemon behind unless it stops one, and a
/// leaked daemon holds a temp directory the harness is about to delete.
fn stop(data_dir: &std::path::Path) {
    let output = lore(data_dir, &["stop"]);
    assert!(
        output.status.success(),
        "lore stop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn start_backgrounds_the_daemon_and_a_second_start_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let data = Utf8Path::from_path(dir.path()).expect("utf-8 temp dir");

    let first = lore(dir.path(), &["start"]);
    assert!(
        first.status.success(),
        "lore start failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let text = stdout(&first);
    assert!(text.contains("starting the lore daemon"), "{text}");
    assert!(text.contains("answering at http://"), "{text}");

    // The parent returned only once the daemon was discoverable — no polling
    // here, because "you can use it when this command exits" is the contract.
    let record = discover(data)
        .expect("readable handshake")
        .expect("a daemon");
    assert_eq!(record.api_version, lore_core::API_VERSION);
    assert_ne!(
        record.pid,
        std::process::id(),
        "the daemon is a separate process, not this test"
    );

    // Detached stdio has to land somewhere, or a crash after this point leaves
    // nothing to read.
    let log = data.join("daemon.log");
    assert!(log.exists(), "{log} was not created");

    // Idempotent: the second run finds the first daemon rather than spawning a
    // second one for the ownership lock to refuse.
    let second = lore(dir.path(), &["start"]);
    assert!(
        second.status.success(),
        "second lore start failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let text = stdout(&second);
    assert!(text.contains("already running"), "{text}");
    assert!(text.contains(&record.pid.to_string()), "{text}");

    stop(dir.path());
    assert!(
        discover(data).expect("readable handshake").is_none(),
        "the handshake outlived the daemon"
    );
}

/// A dead endpoint is a supported state (D-0007), so it is a printed line and
/// a daemon that starts anyway — not a failure and not a wait.
#[test]
fn an_unreachable_endpoint_without_a_start_command_does_not_block_startup() {
    let dir = tempfile::tempdir().unwrap();
    let data = Utf8Path::from_path(dir.path()).expect("utf-8 temp dir");
    // Port 1 is not something anyone serves on; the probe fails on connect.
    std::fs::write(
        data.join("config.toml"),
        "[embeddings]\nendpoint = \"http://127.0.0.1:1/v1\"\n",
    )
    .unwrap();

    let started = std::time::Instant::now();
    let output = lore(dir.path(), &["start"]);
    assert!(
        output.status.success(),
        "lore start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        started.elapsed() < PATIENCE,
        "startup waited on an endpoint it was never told how to start"
    );
    let text = stdout(&output);
    assert!(text.contains("no `start_command` is configured"), "{text}");
    assert!(text.contains("answering at http://"), "{text}");

    stop(dir.path());
}

/// `start_command` without `endpoint` names nothing to wait for, so running it
/// would be firing a process at no observable outcome.
#[test]
fn a_start_command_without_an_endpoint_is_reported_and_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let data = Utf8Path::from_path(dir.path()).expect("utf-8 temp dir");
    let sentinel = data.join("must-not-exist");
    // If this ever runs, it leaves proof. TOML *literal* strings, because a
    // Windows path in a basic string is a pile of invalid escapes.
    let argv = if cfg!(windows) {
        format!("['cmd', '/c', 'type nul > {sentinel}']")
    } else {
        format!("['touch', '{sentinel}']")
    };
    std::fs::write(
        data.join("config.toml"),
        format!("[embeddings]\nstart_command = {argv}\n"),
    )
    .unwrap();

    let output = lore(dir.path(), &["start"]);
    assert!(
        output.status.success(),
        "lore start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = stdout(&output);
    assert!(text.contains("`endpoint` is not"), "{text}");
    assert!(!sentinel.exists(), "the start command was run anyway");

    stop(dir.path());
}
