//! Process-lifetime ownership of the index (D-0003).
//!
//! [`acquire`] takes an exclusive OS lock on `daemon.lock` in the data
//! directory and holds it for as long as the returned guard lives. The
//! kernel releases the lock when the holding process exits *however it
//! exits* — clean return, panic, `TerminateProcess`, power loss — so a
//! crashed daemon can never strand the machine behind its own lock, and a
//! hung-but-alive daemon correctly keeps it.
//!
//! This is not an existence-check lockfile. The file's *presence* means
//! nothing (an empty `daemon.lock` lingers after shutdown, deliberately:
//! deleting it on the way out would race the next starter's open). Only a
//! *held* lock — a live handle in a live process — blocks admission, which
//! is exactly the distinction that makes it crash-safe where a lockfile is
//! not.
//!
//! `daemon.json` (see [`super::handshake`]) plays no part in admission: it
//! is a discovery record. Deleting it, corrupting it, or stepping the wall
//! clock cannot create or destroy ownership, because ownership here is a
//! kernel fact, not a timestamp.

use anyhow::{Context, Result, bail};
use camino::Utf8Path;

use super::handshake;

/// Lock file name, next to `daemon.json` and the database.
pub const LOCK_FILE: &str = "daemon.lock";

/// Held for the daemon's whole life; dropping it (or dying) releases the
/// index to the next starter.
///
/// Rides inside every [`super::StoreHandle`] clone, so the lock cannot be
/// released while any code that can still reach the store exists —
/// including a blocking store call a shutdown timeout left behind.
#[derive(Debug)]
pub struct OwnershipGuard {
    _file: std::fs::File,
}

/// The admission primitive: exactly one process per data directory.
///
/// On refusal the error names the incumbent from its discovery record when
/// one is readable — the lock proves *that* someone owns the index; the
/// record is only how we say *who*.
pub fn acquire(data_dir: &Utf8Path) -> Result<OwnershipGuard> {
    let path = data_dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        // The file's bytes are meaningless (see module doc); never truncate,
        // so opening to *try* the lock can't disturb anything.
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening ownership lock {path}"))?;
    match file.try_lock() {
        Ok(()) => Ok(OwnershipGuard { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => match handshake::read(data_dir) {
            Ok(Some(existing)) => bail!(
                "daemon already running (pid {}, port {}); it holds {path}",
                existing.pid,
                existing.port
            ),
            _ => bail!("daemon already running: another process holds {path}"),
        },
        Err(std::fs::TryLockError::Error(err)) => {
            Err(err).with_context(|| format!("locking {path}"))
        }
    }
}
