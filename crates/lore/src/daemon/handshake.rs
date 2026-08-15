//! Discovery handshake — `<data-dir>/daemon.json` (D-0007, CodeGraph
//! pattern).
//!
//! The file is two things:
//!
//! 1. **A discovery record.** Clients (CLI, `lore-mcp`) read it to find the
//!    ephemeral port — [`read`] is the shared helper they call.
//! 2. **A liveness claim.** `heartbeat_at` is refreshed every
//!    [`HEARTBEAT_INTERVAL`]; older than [`STALE_AFTER`] and a client should
//!    not believe the record without probing the port.
//!
//! It is deliberately *not* the mutual-exclusion mechanism. A file that
//! anything can delete, corrupt, or mistime cannot prove exclusivity: that
//! job belongs to [`super::ownership`], whose held OS lock is released by
//! the kernel on process death and by nothing else. Admission never reads
//! this file except to name the incumbent in an error message.
//!
//! Writes are atomic (temp file in the same directory + rename over the
//! target), so a client can never observe a half-written record: readers see
//! either the previous complete file or the new complete file.

use std::time::Duration;

use anyhow::{Context, Result};
use camino::Utf8Path;

/// Read-side contract (record type, freshness rule, `read`) lives in
/// `lore_core::discovery` so thin clients discover the daemon without
/// linking this crate; re-exported here for the daemon's own call sites.
pub use lore_core::discovery::{
    HANDSHAKE_FILE, HEARTBEAT_INTERVAL, Handshake, STALE_AFTER, handshake_path as path, is_fresh,
    read, unix_now,
};

/// How long the takeover probe waits for `/v1/status` to answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// A record for *this* process listening on `port`.
pub fn for_this_process(port: u16, now: i64) -> Handshake {
    Handshake {
        pid: std::process::id(),
        port,
        api_version: lore_core::API_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: now,
        heartbeat_at: now,
    }
}

/// Atomically publish `handshake`.
///
/// Temp file in the *same directory* (rename is only atomic within a volume),
/// then rename over the target. `std::fs::rename` maps to `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING` on Windows, so the replace is a single
/// metadata operation and never leaves the target truncated.
///
/// The temp name carries the pid so two processes cannot clobber each
/// other's scratch file. Two *daemons* never race the target: only the
/// [`super::ownership`] lock holder publishes here.
pub fn write(data_dir: &Utf8Path, handshake: &Handshake) -> Result<()> {
    let target = path(data_dir);
    let temp = data_dir.join(format!("{HANDSHAKE_FILE}.{}.tmp", std::process::id()));
    let body = serde_json::to_vec_pretty(handshake)?;
    std::fs::write(&temp, &body).with_context(|| format!("writing {temp}"))?;
    match std::fs::rename(&temp, &target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(err).with_context(|| format!("publishing handshake {target}"))
        }
    }
}

/// Remove the handshake file, but only if it still names `pid`.
///
/// A daemon that took over from us (because we hung past the stale window)
/// owns the file now; deleting it on our way out would leave the live daemon
/// undiscoverable. Returns whether the file was removed.
pub fn remove_if_owned_by(data_dir: &Utf8Path, pid: u32) -> Result<bool> {
    match read(data_dir) {
        Ok(Some(current)) if current.pid == pid => {
            std::fs::remove_file(path(data_dir))?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        // Corrupt file: not provably ours, leave it. The next start treats it
        // as stale anyway.
        Err(_) => Ok(false),
    }
}

/// Does a *Lore daemon* answer on this loopback port?
///
/// Deserializing the body as [`lore_core::DaemonStatus`] is the point: a bare
/// TCP connect, or even a 200, would let any unrelated process squatting on a
/// recycled port veto our startup forever.
pub async fn probe(port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() else {
        return false;
    };
    let url = format!("http://127.0.0.1:{port}/v1/status");
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            response.json::<lore_core::DaemonStatus>().await.is_ok()
        }
        _ => false,
    }
}
