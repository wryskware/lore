//! Single-instance handshake — `<data-dir>/daemon.json` (D-0007, CodeGraph
//! pattern).
//!
//! The file is simultaneously three things:
//!
//! 1. **A mutual-exclusion token.** Exactly one process may own the index
//!    (D-0003); a second `lore daemon` must refuse to start rather than race.
//! 2. **A liveness claim.** `heartbeat_at` is refreshed every
//!    [`HEARTBEAT_INTERVAL`]; older than [`STALE_AFTER`] and the claim is not
//!    believed on its own.
//! 3. **A discovery record.** Clients (CLI, `lore-mcp`) read it to find the
//!    ephemeral port — [`read`] is the shared helper they call.
//!
//! Writes are atomic (temp file in the same directory + rename over the
//! target), so a client can never observe a half-written record: readers see
//! either the previous complete file or the new complete file.
//!
//! Deliberately *not* an OS lock file: a crashed daemon must not leave the
//! machine unable to start a new one, and Windows lock semantics on a crashed
//! process are exactly the CCE failure this design avoids. Liveness is proven
//! by a fresh heartbeat or by the port actually answering, never by the mere
//! existence of a file.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

/// File name within the data directory.
pub const HANDSHAKE_FILE: &str = "daemon.json";

/// How often the owning daemon refreshes `heartbeat_at`.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// A heartbeat older than this no longer proves liveness on its own; the
/// port is probed before the record is declared stale. Three missed beats.
pub const STALE_AFTER: Duration = Duration::from_secs(45);

/// How long the takeover probe waits for `/v1/status` to answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The published record. Field set is deliberately small and stable: thin
/// clients parse this with nothing but `serde_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    pub pid: u32,
    pub port: u16,
    pub api_version: u32,
    pub daemon_version: String,
    /// Unix seconds at daemon start.
    pub started_at: i64,
    /// Unix seconds at the last heartbeat refresh.
    pub heartbeat_at: i64,
}

impl Handshake {
    /// A record for *this* process listening on `port`.
    pub fn for_this_process(port: u16, now: i64) -> Self {
        Self {
            pid: std::process::id(),
            port,
            api_version: lore_core::API_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: now,
            heartbeat_at: now,
        }
    }

    /// Base URL of the versioned API, e.g. `http://127.0.0.1:53412/v1`.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
}

pub fn path(data_dir: &Utf8Path) -> Utf8PathBuf {
    data_dir.join(HANDSHAKE_FILE)
}

/// Unix seconds now. Pre-1970 clocks are not a scenario worth modelling.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Read the handshake record, if the daemon has ever published one here.
///
/// `Ok(None)` means "no file". A corrupt file is an `Err` — clients deserve
/// to know the difference, and [`preflight`] treats it as stale.
pub fn read(data_dir: &Utf8Path) -> Result<Option<Handshake>> {
    let file = path(data_dir);
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let parsed =
                serde_json::from_str(&text).with_context(|| format!("parsing handshake {file}"))?;
            Ok(Some(parsed))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading handshake {file}")),
    }
}

/// Is this heartbeat recent enough to believe on its own?
///
/// Pure over `(record, now)` so the policy is testable without sleeping.
/// A heartbeat in the future (clock adjustment, DST-naive clock, VM resume)
/// counts as fresh: refusing to start is the safe direction when the answer
/// is "we cannot tell".
pub fn is_fresh(handshake: &Handshake, now: i64) -> bool {
    now - handshake.heartbeat_at < STALE_AFTER.as_secs() as i64
}

/// Atomically publish `handshake`.
///
/// Temp file in the *same directory* (rename is only atomic within a volume),
/// then rename over the target. `std::fs::rename` maps to `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING` on Windows, so the replace is a single
/// metadata operation and never leaves the target truncated.
///
/// The temp name carries the pid so two racing daemons cannot clobber each
/// other's scratch file (they will still fight over the target, which is what
/// [`preflight`] is for).
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

/// Startup gate. `Ok(())` means this process may take ownership.
///
/// Order matters: the cheap local check (fresh heartbeat) comes first so the
/// common "daemon is already running" case costs one file read and no
/// network. Only an *old* heartbeat gets a probe, and only a probe that gets
/// a well-formed `/v1/status` back counts as proof of life — a stale port can
/// have been recycled by an unrelated process.
pub async fn preflight(data_dir: &Utf8Path) -> Result<()> {
    let existing = match read(data_dir) {
        Ok(Some(existing)) => existing,
        Ok(None) => return Ok(()),
        Err(err) => {
            tracing::warn!(error = %err, "unreadable handshake file; treating as stale");
            return Ok(());
        }
    };

    let age = unix_now() - existing.heartbeat_at;
    if is_fresh(&existing, unix_now()) {
        bail!(
            "daemon already running (pid {}, port {}); heartbeat is {age}s old",
            existing.pid,
            existing.port
        );
    }

    if probe(existing.port).await {
        bail!(
            "daemon already running (pid {}, port {}); heartbeat is {age}s stale but \
             /v1/status still answers",
            existing.pid,
            existing.port
        );
    }

    tracing::warn!(
        stale_pid = existing.pid,
        stale_port = existing.port,
        heartbeat_age_secs = age,
        "taking over stale handshake"
    );
    Ok(())
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
