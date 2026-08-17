//! The snapshot/push wire contract (D-0015).
//!
//! Content reaches the index by **push**: a client sends a snapshot and the
//! index owner never reads a filesystem it does not own. The semantic unit is
//! a [`Manifest`] — the complete listing of a project's ignore-filtered files
//! as `(path, hash, size)`, verified by a full-manifest checksum. Deletion is
//! *absence* from the manifest, so there is no delete event to lose, reorder
//! or replay.
//!
//! Locally the walker feeds these very types in-process (`lore`'s daemon
//! snapshot seam); the push routes that serialize them are a later package.
//! Everything here is therefore shaped as the wire contract already, so that
//! package changes nothing about what these mean:
//!
//! - Paths are project-relative with forward slashes, exactly as
//!   [`crate::SearchResult::path`] reports them.
//! - Hashes are lowercase hex of the file's bytes; the algorithm is the
//!   daemon's business, never the wire's, but both ends must agree — today
//!   blake3, and a change of algorithm is a change of contract.
//! - Additive fields carry `#[serde(default)]`, on the same reasoning as the
//!   rest of the API: daemon and client are not upgraded atomically.
//!
//! Chunking stays daemon-side (`CHUNK_FORMAT_VERSION` never crosses the
//! wire): clients send content, never chunks.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One file in a manifest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Project-relative path, forward slashes.
    pub path: String,
    /// Lowercase hex hash of the file's bytes.
    pub hash: String,
    /// Byte length, as observed with the hash. Carried because the receiver
    /// budgets an upload before it has the bytes, and because a size that
    /// disagrees with the delivered content is the cheapest possible integrity
    /// check.
    pub size: u64,
}

/// A project's complete ignore-filtered file listing.
///
/// Full listing, always: ~100 bytes per file, so even a 300k-file repository
/// is a ~30 MB compressible message. A delta encoding against a
/// daemon-named base generation is a permitted *wire optimization* later; the
/// semantic unit stays the full manifest, which is what makes a push
/// idempotent and a lost message harmless.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Sorted by path, so two observers of the same tree produce byte-identical
    /// manifests and therefore identical checksums.
    pub entries: Vec<ManifestEntry>,
    /// Checksum over every entry ([`Manifest::checksum_of`]). The receiver
    /// verifies it before trusting a listing whose *absences* delete files.
    pub checksum: String,
}

impl Manifest {
    /// Sort, then checksum. The only constructor: a manifest whose checksum
    /// does not describe its entries is not a manifest.
    pub fn new(mut entries: Vec<ManifestEntry>) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let checksum = Self::checksum_of(&entries);
        Self { entries, checksum }
    }

    /// Checksum of an entry list in the canonical (path-sorted) form.
    ///
    /// NUL-separated because it is the one byte a path cannot contain, so no
    /// combination of path, hash and size can be re-partitioned into a
    /// different listing with the same digest.
    pub fn checksum_of(entries: &[ManifestEntry]) -> String {
        let mut hasher = blake3::Hasher::new();
        for entry in entries {
            hasher.update(entry.path.as_bytes());
            hasher.update(b"\0");
            hasher.update(entry.hash.as_bytes());
            hasher.update(b"\0");
            hasher.update(entry.size.to_string().as_bytes());
            hasher.update(b"\n");
        }
        hasher.finalize().to_hex().to_string()
    }

    /// Whether the checksum still describes the entries — the receiver's first
    /// question about any manifest that arrived over a wire.
    pub fn is_intact(&self) -> bool {
        self.checksum == Self::checksum_of(&self.entries)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry for a path, or `None` — which is what deletion *is*.
    pub fn get(&self, path: &str) -> Option<&ManifestEntry> {
        self.entries
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .ok()
            .map(|at| &self.entries[at])
    }
}

/// Why a string could not be a manifest path at all, if it could not.
///
/// This is a *structural* verdict, not a policy one, and the difference decides
/// how a receiver answers. A path that is empty, absolute, backslashed or
/// traversal-shaped is not a file this protocol has an opinion about — it is a
/// client that does not implement the contract, and the honest answer is to
/// refuse the whole listing by name. A perfectly well-formed path that names a
/// secret is the opposite case: the client works, its configuration is wrong,
/// and the receiver drops that entry and says which (see the receiver's
/// hard-exclude backstop, D-0015).
///
/// Nothing here is a security boundary. A receiver must never derive a
/// filesystem path from a client's string in the first place — the push
/// staging area names files by their content hash for exactly that reason.
/// This is about *detecting a broken client loudly*, before its listing's
/// absences are allowed to delete anything.
pub fn malformed_path(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("is empty");
    }
    if path.contains('\\') {
        return Some("contains a backslash; manifest paths use forward slashes on every platform");
    }
    if path.starts_with('/') {
        return Some("is absolute; manifest paths are project-relative");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Some("names a drive letter; manifest paths are project-relative");
    }
    path.split('/').find_map(|part| match part {
        "" => Some("contains an empty path component"),
        ".." => Some("contains a `..` component"),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Push session identity
// ---------------------------------------------------------------------------

/// An unguessable push-session handle, bound to project + epoch by the daemon
/// that minted it. A handle is a *consistency* primitive, never an
/// authorization grant (D-0015: the daemon has no users and no roles).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PushSessionId(pub String);

impl fmt::Display for PushSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A project's push epoch: monotonically increasing, bumped by every lease
/// takeover, and named by every push so a stale pusher is rejected rather than
/// interleaved. Verified in the same transaction that publishes a generation.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PushEpoch(pub u64);

impl fmt::Display for PushEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// The push flow: lease → manifest → needed paths → upload → commit
// ---------------------------------------------------------------------------

/// Step 1 of the push flow: claim the project's push lease.
///
/// Conflict policy is **takeover** (D-0015): an acquire always succeeds and
/// bumps the epoch, displacing whoever held it. There is nothing to
/// authorize — a lease is a consistency primitive, and the daemon has no
/// users and no roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushLeaseRequest {
    /// Project identity — the registry-bound declared name (D-0016).
    pub project: String,
}

/// The lease, plus the two cadences the pusher has to respect to keep it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushLeaseResponse {
    pub session: PushSessionId,
    pub epoch: PushEpoch,
    /// Seconds of silence after which the daemon reaps this lease and deletes
    /// its staging area. Every accepted push request counts as proof of life,
    /// so the heartbeat below is only for the gaps between them.
    pub ttl_secs: u64,
    /// How often to renew while idle. Comfortably inside [`Self::ttl_secs`].
    pub heartbeat_secs: u64,
    /// The receiving daemon's floor on how often it accepts a manifest for one
    /// project. Advertised so a pusher can pace itself rather than discover the
    /// floor by being refused (D-0015: "a receiving daemon enforces a hard
    /// minimum push interval and rejects clients configured hotter").
    #[serde(default)]
    pub min_push_interval_secs: u64,
}

/// Heartbeat: keep an idle lease alive. Names its epoch like every other push
/// request, so a renewal from a session that has been taken over is refused
/// rather than silently resurrecting it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRenewRequest {
    pub project: String,
    pub session: PushSessionId,
    pub epoch: PushEpoch,
}

/// Step 2 of the push flow: here is the whole listing, tell me what you need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushManifestRequest {
    /// Project identity — the registry-bound declared name (D-0016).
    pub project: String,
    pub session: PushSessionId,
    pub epoch: PushEpoch,
    pub manifest: Manifest,
    /// The mass-delete guard's override, carried per push because it is a
    /// per-invocation decision by a human (D-0015 forbids it as a config key).
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_mass_delete: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The paths whose content the index owner does not already have.
///
/// Hashes matching committed *or* already-staged content are skipped, so
/// file-level resumption of an interrupted push falls out for free.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushManifestResponse {
    /// Project-relative paths to upload, in manifest order.
    pub needed: Vec<String>,
    /// What the commit will delete if this manifest is committed as-is —
    /// answered here rather than at commit time so a pusher learns that its
    /// listing looks like a catastrophe *before* uploading anything.
    #[serde(default)]
    pub deletes: u64,
    /// Listed paths the receiver refuses to index, dropped from the accepted
    /// manifest (D-0015's hard-exclude backstop).
    ///
    /// The manifest is still *accepted*: a client listing `.env` is
    /// misconfigured, not broken, and refusing its whole snapshot would take a
    /// working index offline over one file. The refused entries simply are not
    /// in it — which means absence semantics now apply to them, so a copy
    /// indexed before the backstop existed (or before the file matched) is
    /// **deleted** by the next commit. That is the backstop actively purging,
    /// and it is the point: a secret that reached the index once must be able
    /// to leave it.
    ///
    /// Paths only. Which rule refused each one is in the daemon's log — a
    /// pusher's remedy is the same whichever pattern matched, and naming the
    /// pattern on the wire would invite clients to branch on it.
    #[serde(default)]
    pub refused: Vec<String>,
}

/// Step 3 of the push flow: one needed path's content.
///
/// The bytes travel as the request *body*, unencoded, so the largest messages
/// in the protocol do not pay a base64 third on top of themselves; everything
/// that identifies the upload travels as query parameters instead. That is why
/// this is the one wire type here that is not a JSON document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushFileParams {
    pub project: String,
    pub session: PushSessionId,
    pub epoch: PushEpoch,
    /// Project-relative path, forward slashes — and it must be one the
    /// session's manifest listed. The receiver never derives a filesystem path
    /// from this string (staged content is stored under its own hash), so a
    /// traversal attempt is a lookup miss rather than an escape.
    pub path: String,
}

/// What the receiver has, after staging one upload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushFileResponse {
    /// Needed paths whose content is now staged.
    pub staged: u64,
    /// Needed paths still missing. The commit refuses while this is non-zero:
    /// an unstaged path would otherwise read as absent content, and absence is
    /// how this protocol spells deletion.
    #[serde(default)]
    pub needed: u64,
}

/// Step 4 of the push flow: publish everything staged under this session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushCommitRequest {
    pub project: String,
    pub session: PushSessionId,
    pub epoch: PushEpoch,
}

/// What the publishing transaction did. Readers saw the previous generation
/// until it returned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushCommitResponse {
    /// The project generation this commit advanced to.
    pub generation: u64,
    /// Files whose chunks were (re)written.
    #[serde(default)]
    pub indexed: u64,
    /// Files dropped because the manifest omitted them.
    #[serde(default)]
    pub deleted: u64,
}

// ---------------------------------------------------------------------------
// The mass-delete guard
// ---------------------------------------------------------------------------

/// A manifest deleting more than this many files, *and* more than half the
/// project, is refused (D-0015). Both conditions: a small project churning its
/// entire contents is ordinary, and a large project shedding a hundred files
/// out of ten thousand is ordinary too. Together they describe the accident —
/// an ignore rule gone wrong, a wrong root, a half-checked-out tree.
///
/// An operational default, tunable without a new decision.
pub const MASS_DELETE_MIN_FILES: u64 = 100;

/// A refused apply, with the numbers that refused it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MassDeleteTrip {
    /// Files the manifest would have deleted.
    pub deletes: u64,
    /// Files the project had indexed when it was refused.
    pub stored: u64,
}

impl fmt::Display for MassDeleteTrip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refused to drop {} of {} indexed file(s)",
            self.deletes, self.stored
        )
    }
}

/// The guard itself: `Some` means this apply must not happen.
///
/// One function so the local pipeline and a receiving daemon cannot drift into
/// two different notions of "too many". `deletes * 2 > stored` is "more than
/// half" without a float anywhere near a destructive decision.
pub fn mass_delete_trip(deletes: u64, stored: u64) -> Option<MassDeleteTrip> {
    (deletes > MASS_DELETE_MIN_FILES && deletes * 2 > stored)
        .then_some(MassDeleteTrip { deletes, stored })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_path_is_named_by_the_way_it_is_malformed() {
        for (path, reason) in [
            ("", "is empty"),
            ("/etc/shadow", "is absolute"),
            ("//server/share/file", "is absolute"),
            ("C:/Windows/win.ini", "names a drive letter"),
            ("c:relative", "names a drive letter"),
            (r"src\lib.rs", "contains a backslash"),
            ("../escape", "contains a `..` component"),
            ("a/../../escape", "contains a `..` component"),
            ("src//lib.rs", "contains an empty path component"),
            ("src/", "contains an empty path component"),
        ] {
            let verdict = malformed_path(path).unwrap_or_else(|| panic!("{path:?} was accepted"));
            assert!(verdict.starts_with(reason), "{path:?}: {verdict}");
        }
    }

    /// The half that matters more: over-rejecting refuses a whole manifest and
    /// takes a working index offline, so ordinary paths — including the odd
    /// ones real repositories contain — must survive.
    #[test]
    fn ordinary_manifest_paths_are_accepted() {
        for path in [
            "src/lib.rs",
            "README.md",
            ".github/workflows/ci.yml",
            "a/b/c/d/e.txt",
            "docs/日本語.md",
            "weird name with spaces.md",
            "trailing.dots...",
            // A leading `.` component and a `..` *inside* a name are neither
            // traversal nor absolute.
            "./relative.rs",
            "src/..hidden.rs",
            "src/a..b.rs",
        ] {
            assert_eq!(malformed_path(path), None, "{path:?}");
        }
    }
}
