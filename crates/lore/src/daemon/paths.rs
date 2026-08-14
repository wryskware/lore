//! Where the daemon keeps its state, and how filesystem paths are normalized
//! before they reach the store.
//!
//! Layout (Windows shown; `dirs` picks the platform equivalent elsewhere):
//!
//! ```text
//! %LOCALAPPDATA%\lore\
//!   lore.db       SQLite: metadata + FTS5 + vectors (one transaction domain)
//!   config.toml   optional, see crate::config
//!   daemon.json   single-instance handshake, see super::handshake
//! ```
//!
//! `LORE_DATA_DIR` overrides the whole directory — that is how tests (and a
//! second, isolated daemon instance) get their own world.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use camino::Utf8PathBuf;

/// Data-dir resolution lives in the contract crate so thin clients resolve
/// the same directory without linking this crate; re-exported for the
/// daemon's own call sites.
pub use lore_core::discovery::{DATA_DIR_ENV, data_dir, resolve_data_dir};

/// SQLite database file name within the data directory.
pub const DB_FILE: &str = "lore.db";

/// Canonicalize a user-supplied project root and hand back a path the store
/// can hold forever.
///
/// [`crate::store::Store::register_project`] deliberately does not
/// canonicalize, so this is the one place it happens: `C:\repos\x`,
/// `c:\repos\.\x` and a `subst`/symlink alias must all register as the same
/// project or the watcher and the index disagree about identity.
///
/// The verbatim (`\\?\`) prefix `std::fs::canonicalize` returns on Windows is
/// stripped dunce-style — it is correct but leaks into every log line, error
/// message and `status` payload, and many Windows APIs (plus users) dislike
/// it.
pub fn canonicalize_root(path: impl AsRef<Path>) -> Result<Utf8PathBuf> {
    let path = path.as_ref();
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("resolving project root {}", path.display()))?;
    Ok(strip_verbatim(to_utf8(canonical)?))
}

fn to_utf8(path: PathBuf) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path)
        .map_err(|p| anyhow!("path is not valid UTF-8: {}", p.display()))
}

/// Dunce-style simplification: drop `\\?\` from paths that do not need it.
///
/// Kept as a pure string transform (no filesystem access) so it is trivially
/// testable and a no-op on non-Windows. Verbatim paths that are *not* plain
/// drive paths (device namespace, over-`MAX_PATH` paths that genuinely need
/// the prefix) are left alone.
pub fn strip_verbatim(path: Utf8PathBuf) -> Utf8PathBuf {
    let s = path.as_str();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Utf8PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        let plain_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        // Over-MAX_PATH paths still need the prefix to be usable.
        if plain_drive && rest.len() < 260 {
            return Utf8PathBuf::from(rest);
        }
    }
    path
}

/// Project-relative path in the canonical wire form (forward slashes), or
/// `None` when `abs` is not strictly under `root`.
///
/// Comparison is ASCII-case-insensitive on Windows: watcher events echo back
/// whatever casing the OS reports, which need not match the canonicalized
/// root, and treating `C:\Repos\X\a.cs` as "outside the project" would
/// silently drop edits.
pub fn relative_to(root: &camino::Utf8Path, abs: &camino::Utf8Path) -> Option<Utf8PathBuf> {
    let root_str = root.as_str().trim_end_matches(['\\', '/']);
    let abs_str = abs.as_str();
    if abs_str.len() <= root_str.len() {
        return None;
    }
    let (head, tail) = abs_str.split_at(root_str.len());
    let same_root = if cfg!(windows) {
        head.eq_ignore_ascii_case(root_str)
    } else {
        head == root_str
    };
    if !same_root {
        return None;
    }
    // Guard against `C:\repos\x` matching `C:\repos\xtra`.
    let rest = tail.strip_prefix(['\\', '/'])?;
    if rest.is_empty() {
        return None;
    }
    Some(Utf8PathBuf::from(rest.replace('\\', "/")))
}

/// True when `path` is `dir` itself or lives under it (same casing rules as
/// [`relative_to`]). Used to keep the daemon's own data directory out of
/// every walk and every watcher event.
pub fn is_within(dir: &camino::Utf8Path, path: &camino::Utf8Path) -> bool {
    let dir_str = dir.as_str().trim_end_matches(['\\', '/']);
    let path_str = path.as_str();
    if path_str.len() < dir_str.len() {
        return false;
    }
    let (head, tail) = path_str.split_at(dir_str.len());
    let same = if cfg!(windows) {
        head.eq_ignore_ascii_case(dir_str)
    } else {
        head == dir_str
    };
    same && (tail.is_empty() || tail.starts_with(['\\', '/']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefix_is_stripped_only_where_safe() {
        assert_eq!(
            strip_verbatim(Utf8PathBuf::from(r"\\?\C:\repos\x")).as_str(),
            r"C:\repos\x"
        );
        assert_eq!(
            strip_verbatim(Utf8PathBuf::from(r"\\?\UNC\server\share\x")).as_str(),
            r"\\server\share\x"
        );
        // Device namespace is not a drive path: leave it exactly as-is.
        assert_eq!(
            strip_verbatim(Utf8PathBuf::from(r"\\?\GLOBALROOT\Device\Foo")).as_str(),
            r"\\?\GLOBALROOT\Device\Foo"
        );
        // Already plain.
        assert_eq!(
            strip_verbatim(Utf8PathBuf::from(r"C:\repos\x")).as_str(),
            r"C:\repos\x"
        );
    }

    #[test]
    fn relative_paths_use_forward_slashes() {
        let root = camino::Utf8Path::new(r"C:\repos\x");
        let abs = camino::Utf8Path::new(r"C:\repos\x\src\lib.rs");
        assert_eq!(relative_to(root, abs).unwrap().as_str(), "src/lib.rs");
        assert_eq!(relative_to(root, root), None);
        assert_eq!(
            relative_to(root, camino::Utf8Path::new(r"C:\other\a")),
            None
        );
        // Sibling directory sharing a name prefix is not "inside".
        assert_eq!(
            relative_to(root, camino::Utf8Path::new(r"C:\repos\xtra\a")),
            None
        );
    }

    #[test]
    fn data_dir_containment_covers_self_and_children() {
        let dir = camino::Utf8Path::new(r"C:\data\lore");
        assert!(is_within(dir, dir));
        assert!(is_within(
            dir,
            camino::Utf8Path::new(r"C:\data\lore\lore.db")
        ));
        assert!(!is_within(
            dir,
            camino::Utf8Path::new(r"C:\data\lore-other\x")
        ));
        assert!(!is_within(dir, camino::Utf8Path::new(r"C:\data")));
    }
}
