//! Client-side daemon discovery: the `daemon.json` handshake record and the
//! data-directory resolution every client shares.
//!
//! The daemon (in the `lore` crate) *writes* the handshake; thin clients
//! (`lore-mcp`, CLI subcommands) only ever *read* it — so the record type,
//! the freshness rule, and "where is the data dir" live here in the contract
//! crate. Write-side machinery (atomic publish, the ownership lock that
//! actually enforces single-instance, the probe) stays daemon-side.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

/// Environment override for the data directory.
pub const DATA_DIR_ENV: &str = "LORE_DATA_DIR";

/// Handshake file name within the data directory.
pub const HANDSHAKE_FILE: &str = "daemon.json";

/// How often the owning daemon refreshes `heartbeat_at`.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// A heartbeat older than this no longer proves liveness on its own; a
/// client should probe the port before believing the record. Three missed
/// beats.
pub const STALE_AFTER: Duration = Duration::from_secs(45);

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
    /// Base URL of the versioned API, e.g. `http://127.0.0.1:53412/v1`.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
}

/// Data directory for this process, honoring [`DATA_DIR_ENV`].
pub fn data_dir() -> Result<Utf8PathBuf> {
    resolve_data_dir(std::env::var_os(DATA_DIR_ENV).as_deref())
}

/// Pure resolution so tests never have to mutate process environment
/// (which is racy across parallel tests and unsafe in edition 2024).
pub fn resolve_data_dir(override_value: Option<&OsStr>) -> Result<Utf8PathBuf> {
    let raw: PathBuf = match override_value {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => dirs::data_local_dir()
            .ok_or_else(|| anyhow!("no platform-local data directory available"))?
            .join("lore"),
    };
    Utf8PathBuf::from_path_buf(raw).map_err(|p| anyhow!("path is not valid UTF-8: {}", p.display()))
}

pub fn handshake_path(data_dir: &Utf8Path) -> Utf8PathBuf {
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
/// to know the difference. (The daemon itself doesn't care: admission is the
/// ownership lock, and this record is only discovery.)
pub fn read(data_dir: &Utf8Path) -> Result<Option<Handshake>> {
    let file = handshake_path(data_dir);
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
/// counts as fresh: refusing to start a second owner is the safe direction
/// when the answer is "we cannot tell".
pub fn is_fresh(handshake: &Handshake, now: i64) -> bool {
    now - handshake.heartbeat_at < STALE_AFTER.as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_override_wins_and_empty_override_falls_back() {
        let explicit = resolve_data_dir(Some(OsStr::new(r"C:\tmp\lore-test"))).unwrap();
        assert_eq!(explicit.as_str(), r"C:\tmp\lore-test");

        let fallback = resolve_data_dir(Some(OsStr::new(""))).unwrap();
        assert!(
            fallback.as_str().ends_with("lore"),
            "platform default ends in the app folder: {fallback}"
        );
        assert_eq!(fallback, resolve_data_dir(None).unwrap());
    }
}
