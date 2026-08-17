//! The receiving side of the push protocol: leases, epochs, staging (D-0015).
//!
//! A push is four steps — lease, manifest, upload, commit — and this module
//! owns the state that spans them. The pipeline itself is untouched: a commit
//! builds the same [`Snapshot`](super::snapshot::Snapshot) the walker builds,
//! over a [`StagedContent`] source instead of a disk one, and
//! [`super::index`] cannot tell the difference.
//!
//! # What a lease is, and is not
//!
//! It is a **consistency primitive**. It answers "whose observation of this
//! project is current?", never "who is allowed to write?" — the daemon has no
//! users and no roles, binds `127.0.0.1` only, and reaching these routes
//! already requires local code execution (D-0007, and the Trust boundary
//! section of `1.2_Ingestion.md`). A session handle is unguessable because a
//! guessed handle would let one pusher's uploads land in another's commit, not
//! because it is a credential.
//!
//! Local indexing never takes a lease. The walker is this machine's one
//! filesystem observer and it feeds the pipeline in-process; nothing on the
//! `observe_project`/`observe_paths` path touches anything here.
//!
//! # Why takeover, and what it costs
//!
//! Conflict policy is takeover (D-0015, decided): a second acquirer bumps the
//! epoch and displaces the holder, whose next request is refused by name. The
//! alternative — refuse the newcomer — makes a crashed pusher block its own
//! successor until a TTL expires, and the TTL is exactly the number nobody can
//! choose correctly. What takeover costs is *flapping*: two pushers contending
//! land alternating whole snapshots, each internally consistent, never
//! interleaved. That is visible as epoch churn in `lore status`, which is why
//! the epoch is reported there.
//!
//! Because a successor never waits, the TTL here decides only when a dead
//! session's staged files are deleted and when `status` stops naming an epoch
//! nobody holds.
//!
//! # Why staged content is stored under its hash
//!
//! Two reasons, one of them safety. A manifest path is a string a client
//! chose; deriving a filesystem path from it is a traversal waiting to happen,
//! and `../` is the least imaginative form. Staged files are named by the
//! content hash the manifest declared and the upload was verified against, so
//! the client's string is only ever a *lookup key* into a map this daemon
//! built. The other reason is that identical content uploaded under two paths
//! is staged once.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use tokio_util::sync::CancellationToken;

use lore_core::snapshot::{Manifest, PushEpoch, PushSessionId};

use super::snapshot::ContentSource;
use crate::store::ProjectId;

/// Directory under the daemon's data dir holding every session's staging area.
///
/// Wiped at startup ([`PushLeases::reset`]): leases are process state, so no
/// session survives a restart, and content staged for a session that can never
/// commit is content that must never be applied.
pub const STAGING_DIR: &str = "push";

/// Bytes of OS randomness behind a session handle. Sixteen — the same width a
/// UUID spends on identity — is far past guessable for a value that lives
/// seconds to minutes and is checked against a single active lease.
const SESSION_BYTES: usize = 16;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every way a push request is refused, each one named.
///
/// Named rather than lumped into "conflict" because the four consistency
/// failures below have four different remedies, and a pusher that cannot tell
/// "you were taken over" from "you sent a path you never listed" retries the
/// wrong thing forever.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error(
        "no push lease is held for project `{project}`; acquire one with \
         POST /v1/push/lease before pushing (a daemon restart drops every lease)"
    )]
    NoLease { project: String },

    #[error(
        "another pusher took over project `{project}` {ago_secs}s ago and the lease is now at \
         epoch {current}; this session (epoch {presented}) is stale and its staged content will \
         not be applied — acquire a new lease and push the current snapshot again"
    )]
    TakenOver {
        project: String,
        presented: PushEpoch,
        current: PushEpoch,
        ago_secs: u64,
    },

    #[error(
        "push session handle does not match the lease held for project `{project}` at epoch \
         {current}; a handle is bound to one project and one epoch"
    )]
    UnknownSession { project: String, current: PushEpoch },

    #[error(
        "push epoch {presented} is ahead of project `{project}`'s lease (epoch {current}); \
         epochs are minted by the daemon and never by a pusher"
    )]
    EpochAhead {
        project: String,
        presented: PushEpoch,
        current: PushEpoch,
    },

    #[error(
        "manifest checksum does not describe its entries; refusing a listing that did not \
         survive the wire, because its absences would delete files"
    )]
    ManifestCorrupt,

    #[error(
        "manifest entry `{path}` {reason}; refusing the whole listing, because a client that \
         sends one path this shape is not implementing the manifest contract and the rest of \
         its listing cannot be trusted to delete files by absence"
    )]
    ManifestMalformed { path: String, reason: &'static str },

    #[error(
        "a manifest for project `{project}` was accepted {since_secs}s ago and this daemon's \
         minimum push interval is {floor_secs}s; pace the pusher at or above the floor \
         (receiver config: `push.min_interval_secs`)"
    )]
    TooSoon {
        project: String,
        since_secs: u64,
        floor_secs: u64,
    },

    #[error(
        "no manifest has been received for this push session; POST /v1/push/manifest before \
         uploading content"
    )]
    NoManifest,

    #[error(
        "`{path}` is not in this session's manifest; only listed paths can be uploaded, and a \
         manifest is the complete listing"
    )]
    NotInManifest { path: String },

    #[error(
        "`{path}` uploaded {got} bytes but the manifest declared {want}; the file was not \
         staged"
    )]
    SizeMismatch { path: String, want: u64, got: u64 },

    #[error(
        "`{path}` hashed {got} but the manifest declared {want}; the file was not staged, \
         because content that disagrees with the listing it was negotiated under is not the \
         content this commit was authorized to publish"
    )]
    HashMismatch {
        path: String,
        want: String,
        got: String,
    },

    #[error(
        "{missing} of this session's needed path(s) have no staged content (first: `{first}`); \
         upload every needed path before committing, because an unstaged path reads as absent \
         content and absence is how this protocol spells deletion"
    )]
    Incomplete { missing: u64, first: String },

    #[error(
        "a commit is already in flight for project `{project}`; wait for it to answer rather \
         than interleaving two publications of one project"
    )]
    CommitInFlight { project: String },

    #[error("push staging area ({context}): {source}")]
    Staging {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One project's push state. Present for a project that has ever been pushed
/// to, whether or not a lease is currently held.
#[derive(Debug, Default)]
struct ProjectPush {
    lease: Option<Lease>,
    /// When this project last had a manifest accepted.
    ///
    /// Per *project*, not per lease, on purpose: a floor a pusher could reset
    /// by re-acquiring is not a floor, and takeover makes re-acquiring free.
    last_manifest: Option<Instant>,
}

#[derive(Debug)]
struct Lease {
    session: PushSessionId,
    epoch: PushEpoch,
    acquired: Instant,
    /// Last sign of life. Renewed by the heartbeat *and* by every accepted
    /// push request — a pusher mid-upload is demonstrably alive, and making it
    /// heartbeat concurrently to prove that would be ceremony.
    seen: Instant,
    staged: Option<Staged>,
    /// A commit is running for this lease. The apply happens with no lock
    /// held, so this flag is what keeps two commits from publishing one
    /// project at once.
    committing: bool,
}

/// What one session has negotiated and collected so far.
#[derive(Debug)]
struct Staged {
    manifest: Manifest,
    /// The human's per-invocation mass-delete override, carried from the
    /// manifest request to the commit. Never a config key (D-0015).
    allow_mass_delete: bool,
    /// Needed path -> the hash its staged file is named by.
    have: BTreeMap<String, String>,
    /// Needed paths with no content yet.
    needed: BTreeSet<String>,
}

/// What a push request claims to be: a project, and the session and epoch
/// asserting authority over it.
///
/// One struct rather than four parameters because these four travel together
/// through every route and every check, and a call site that could transpose
/// two of them is a call site that could check one session against another
/// project's lease. `name` is carried only so refusals can say which project
/// they mean — identity is the id, resolved from the registry-bound declared
/// name before any of this (D-0016).
#[derive(Debug, Clone)]
pub struct PushClaim {
    pub project: ProjectId,
    pub name: String,
    pub session: PushSessionId,
    pub epoch: PushEpoch,
}

/// Per-project push state as `status` reports it.
#[derive(Debug, Clone, Copy)]
pub struct PushState {
    pub epoch: PushEpoch,
    pub staged: bool,
}

/// What a verified upload should be written as.
#[derive(Debug, Clone)]
pub struct StageTarget {
    pub dir: Utf8PathBuf,
    pub hash: String,
    pub size: u64,
}

/// A verified commit, ready for the pipeline.
pub struct CommitPlan {
    pub manifest: Manifest,
    pub allow_mass_delete: bool,
    pub content: StagedContent,
}

/// Progress within one session, echoed back after every upload.
#[derive(Debug, Clone, Copy, Default)]
pub struct PushProgress {
    pub staged: u64,
    pub needed: u64,
}

/// Every project's push state, shared with the routes and the reaper.
#[derive(Clone)]
pub struct PushLeases {
    inner: Arc<Mutex<BTreeMap<ProjectId, ProjectPush>>>,
    staging_root: Utf8PathBuf,
    ttl: Duration,
    min_interval: Duration,
}

impl std::fmt::Debug for PushLeases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushLeases")
            .field("staging_root", &self.staging_root)
            .finish_non_exhaustive()
    }
}

impl PushLeases {
    pub fn new(data_dir: &Utf8Path, ttl: Duration, min_interval: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BTreeMap::new())),
            staging_root: data_dir.join(STAGING_DIR),
            ttl,
            min_interval,
        }
    }

    /// Discard every staging area on disk. Called once at startup: no lease
    /// survives a restart, so anything already there belongs to a session that
    /// can never commit, and staged content nobody can commit must never be
    /// applied.
    pub fn reset(&self) {
        if let Err(err) = remove_dir(&self.staging_root) {
            tracing::warn!(
                dir = %self.staging_root,
                error = %err,
                "could not clear the push staging area; stale sessions' files remain on disk \
                 (they are inert — no lease can reach them)"
            );
        }
    }

    /// Take the project's lease at `epoch`, displacing whoever held it.
    ///
    /// `epoch` comes from the store's monotonic counter, so it is unique and
    /// ordered across every acquirer including ones from earlier daemon runs.
    /// An acquirer that raced and lost — its epoch is *below* the installed
    /// one — is told it was taken over rather than allowed to install a lower
    /// epoch over a higher one.
    ///
    /// Returns the new handle and, when a lease was displaced, its staging
    /// directory for the caller to delete off the lock.
    pub fn acquire(
        &self,
        project: ProjectId,
        name: &str,
        epoch: PushEpoch,
    ) -> Result<(PushSessionId, Option<Utf8PathBuf>), PushError> {
        let session = mint_session();
        let now = Instant::now();
        let mut leases = self.lock();
        let entry = leases.entry(project).or_default();

        if let Some(held) = &entry.lease
            && held.epoch >= epoch
        {
            return Err(PushError::TakenOver {
                project: name.to_string(),
                presented: epoch,
                current: held.epoch,
                ago_secs: now.saturating_duration_since(held.acquired).as_secs(),
            });
        }

        let displaced = entry.lease.take();
        entry.lease = Some(Lease {
            session: session.clone(),
            epoch,
            acquired: now,
            seen: now,
            staged: None,
            committing: false,
        });
        if let Some(displaced) = &displaced {
            tracing::info!(
                project = name,
                displaced_epoch = %displaced.epoch,
                epoch = %epoch,
                "push lease taken over; the previous session's pushes will be refused"
            );
        }
        Ok((
            session,
            displaced.map(|lease| self.staging_for(project, lease.epoch)),
        ))
    }

    /// Verify a session against the live lease and count the request as proof
    /// of life. Every push route calls this first.
    pub fn touch(&self, claim: &PushClaim) -> Result<(), PushError> {
        let mut leases = self.lock();
        lease_mut(&mut leases, claim).map(|_| ())
    }

    /// The floor (D-0015). Checked only for manifests: uploads and commits
    /// belong to a manifest that already passed it, and refusing them would
    /// strand a push that was accepted.
    pub fn check_interval(&self, project: ProjectId, name: &str) -> Result<(), PushError> {
        if self.min_interval.is_zero() {
            return Ok(());
        }
        let leases = self.lock();
        let Some(last) = leases.get(&project).and_then(|push| push.last_manifest) else {
            return Ok(());
        };
        let since = last.elapsed();
        if since >= self.min_interval {
            return Ok(());
        }
        Err(PushError::TooSoon {
            project: name.to_string(),
            since_secs: since.as_secs(),
            floor_secs: self.min_interval.as_secs(),
        })
    }

    /// Install a negotiated manifest, and hand back the paths still missing
    /// content — which is what the pusher is told to upload.
    ///
    /// `needed` is what the caller's diff against committed state asked for;
    /// content already staged under a previous manifest for this same session
    /// is carried over when its hash still matches, which is what makes an
    /// interrupted push resumable at file granularity.
    ///
    /// The session is re-verified here, not merely at the top of the handler:
    /// the diff ran against the store with no lock held, and a takeover in
    /// that window must not leave the displaced session's manifest installed.
    pub fn record_manifest(
        &self,
        claim: &PushClaim,
        manifest: Manifest,
        allow_mass_delete: bool,
        needed: Vec<String>,
    ) -> Result<Vec<String>, PushError> {
        let dir = self.staging_for(claim.project, claim.epoch);
        let created = std::fs::create_dir_all(&dir);
        let mut leases = self.lock();
        let lease = lease_mut(&mut leases, claim)?;
        created.map_err(|source| PushError::Staging {
            context: format!("creating {dir}"),
            source,
        })?;

        let carried: BTreeMap<String, String> = lease
            .staged
            .take()
            .map(|staged| staged.have)
            .unwrap_or_default();
        let mut have = BTreeMap::new();
        let mut missing = BTreeSet::new();
        for path in needed {
            match (carried.get(&path), manifest.get(&path)) {
                (Some(hash), Some(entry)) if hash == &entry.hash => {
                    have.insert(path, entry.hash.clone());
                }
                _ => {
                    missing.insert(path);
                }
            }
        }

        let still_needed: Vec<String> = missing.iter().cloned().collect();
        lease.staged = Some(Staged {
            manifest,
            allow_mass_delete,
            have,
            needed: missing,
        });
        if let Some(push) = leases.get_mut(&claim.project) {
            push.last_manifest = Some(Instant::now());
        }
        Ok(still_needed)
    }

    /// What one needed path's upload must be: where to write it, and what it
    /// has to hash and measure. The verification itself happens off the lock,
    /// because hashing megabytes under a mutex the whole push surface shares
    /// is how a lock becomes a queue.
    pub fn stage_target(&self, claim: &PushClaim, path: &str) -> Result<StageTarget, PushError> {
        let dir = self.staging_for(claim.project, claim.epoch);
        let mut leases = self.lock();
        let lease = lease_mut(&mut leases, claim)?;
        let staged = lease.staged.as_ref().ok_or(PushError::NoManifest)?;
        let entry = staged
            .manifest
            .get(path)
            .ok_or_else(|| PushError::NotInManifest {
                path: path.to_string(),
            })?;
        Ok(StageTarget {
            dir,
            hash: entry.hash.clone(),
            size: entry.size,
        })
    }

    /// Record that a verified upload is on disk.
    pub fn stage_recorded(
        &self,
        claim: &PushClaim,
        path: &str,
        hash: String,
    ) -> Result<PushProgress, PushError> {
        let mut leases = self.lock();
        let lease = lease_mut(&mut leases, claim)?;
        let staged = lease.staged.as_mut().ok_or(PushError::NoManifest)?;
        staged.needed.remove(path);
        staged.have.insert(path.to_string(), hash);
        Ok(PushProgress {
            staged: staged.have.len() as u64,
            needed: staged.needed.len() as u64,
        })
    }

    /// Verify a commit and claim it, returning everything the pipeline needs.
    ///
    /// The epoch is checked *here*, at the commit, and not merely when the
    /// manifest was negotiated: a session taken over mid-upload must not
    /// publish, and the manifest check happened before the takeover could have
    /// existed. What the commit cannot do is hold this lock across the apply,
    /// so [`Self::commit_finished`] closes the pass and a takeover landing
    /// during it wins the *next* commit rather than corrupting this one —
    /// which is exactly D-0015's "flapping between internally-consistent
    /// snapshots, never interleaved state".
    pub fn take_commit(&self, claim: &PushClaim) -> Result<CommitPlan, PushError> {
        let dir = self.staging_for(claim.project, claim.epoch);
        let mut leases = self.lock();
        let lease = lease_mut(&mut leases, claim)?;
        if lease.committing {
            return Err(PushError::CommitInFlight {
                project: claim.name.clone(),
            });
        }
        let staged = lease.staged.as_ref().ok_or(PushError::NoManifest)?;
        if let Some(first) = staged.needed.iter().next() {
            return Err(PushError::Incomplete {
                missing: staged.needed.len() as u64,
                first: first.clone(),
            });
        }
        let plan = CommitPlan {
            manifest: staged.manifest.clone(),
            allow_mass_delete: staged.allow_mass_delete,
            content: StagedContent {
                dir,
                by_path: staged.have.clone(),
            },
        };
        lease.committing = true;
        Ok(plan)
    }

    /// Close a commit. On success the session's staged state is dropped and
    /// its directory handed back for deletion — D-0015's only cleanup. On
    /// failure the staging survives, so a commit refused by the mass-delete
    /// guard can be retried with the override without re-uploading anything.
    ///
    /// Deliberately infallible: a lease that vanished under a commit (reaped,
    /// taken over, project removed) is not an error the committer can act on,
    /// and the apply has already happened either way.
    pub fn commit_finished(&self, claim: &PushClaim, published: bool) -> Option<Utf8PathBuf> {
        let dir = self.staging_for(claim.project, claim.epoch);
        let mut leases = self.lock();
        let lease = leases
            .get_mut(&claim.project)
            .and_then(|push| push.lease.as_mut())
            .filter(|lease| lease.epoch == claim.epoch && lease.session == claim.session)?;
        lease.committing = false;
        if !published {
            return None;
        }
        lease.staged = None;
        Some(dir)
    }

    /// Reap leases that have gone quiet, returning their staging directories.
    ///
    /// With takeover as the conflict policy this never unblocks anybody — a
    /// successor is never waiting. It exists so a crashed pusher's files leave
    /// the disk and `status` stops naming an epoch nobody holds.
    ///
    /// A lease with a commit in flight is never reaped: its apply is still
    /// reading the very files this would delete.
    pub fn reap(&self) -> Vec<Utf8PathBuf> {
        let mut expired = Vec::new();
        let mut leases = self.lock();
        for (project, push) in leases.iter_mut() {
            let Some(lease) = &push.lease else { continue };
            if lease.committing || lease.seen.elapsed() < self.ttl {
                continue;
            }
            tracing::info!(
                project_id = project,
                epoch = %lease.epoch,
                quiet_secs = lease.seen.elapsed().as_secs(),
                "push lease expired; its staged content is discarded unapplied"
            );
            expired.push(self.staging_for(*project, lease.epoch));
            push.lease = None;
        }
        expired
    }

    /// Drop a deregistered project's push state, so a forgotten project's
    /// lease cannot attach itself to whatever row later inherits its id.
    pub fn forget(&self, project: ProjectId) -> Option<Utf8PathBuf> {
        let mut leases = self.lock();
        let push = leases.remove(&project)?;
        push.lease
            .map(|lease| self.staging_for(project, lease.epoch))
    }

    /// The live lease for `status`, or `None` when nobody holds one — the
    /// normal state of a project that is only indexed locally.
    pub fn state(&self, project: ProjectId) -> Option<PushState> {
        let leases = self.lock();
        let lease = leases.get(&project)?.lease.as_ref()?;
        Some(PushState {
            epoch: lease.epoch,
            staged: lease.staged.is_some(),
        })
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// A session's staging directory. Keyed by project *and* epoch so a
    /// takeover's successor cannot inherit a byte of its predecessor's
    /// half-uploaded state, even in the window before the reaper runs.
    fn staging_for(&self, project: ProjectId, epoch: PushEpoch) -> Utf8PathBuf {
        self.staging_root.join(format!("{project}-{epoch}"))
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<ProjectId, ProjectPush>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The one place a presented session is checked against the live lease.
///
/// Order matters. A lower epoch than the lease's is a takeover and says so
/// with a timestamp; a *higher* one cannot be a takeover, because epochs are
/// minted by the daemon, so it is a fabricated handle and gets its own name.
fn lease_mut<'a>(
    leases: &'a mut BTreeMap<ProjectId, ProjectPush>,
    claim: &PushClaim,
) -> Result<&'a mut Lease, PushError> {
    let lease = leases
        .get_mut(&claim.project)
        .and_then(|push| push.lease.as_mut())
        .ok_or_else(|| PushError::NoLease {
            project: claim.name.clone(),
        })?;
    if claim.epoch < lease.epoch {
        return Err(PushError::TakenOver {
            project: claim.name.clone(),
            presented: claim.epoch,
            current: lease.epoch,
            ago_secs: lease.acquired.elapsed().as_secs(),
        });
    }
    if claim.epoch > lease.epoch {
        return Err(PushError::EpochAhead {
            project: claim.name.clone(),
            presented: claim.epoch,
            current: lease.epoch,
        });
    }
    if lease.session != claim.session {
        return Err(PushError::UnknownSession {
            project: claim.name.clone(),
            current: lease.epoch,
        });
    }
    lease.seen = Instant::now();
    Ok(lease)
}

// ---------------------------------------------------------------------------
// Uploads
// ---------------------------------------------------------------------------

/// Verify one upload against the manifest and write it into the staging area.
///
/// Blocking: hashing and writing. Both checks are the manifest's own numbers,
/// which is the point — the size is the cheapest possible integrity check and
/// the hash is the one that matters, and a mismatch means the bytes are not
/// the bytes the commit was negotiated over. A refused upload stages nothing,
/// so the path simply stays needed and the commit refuses until it is right.
pub fn stage_bytes(target: &StageTarget, path: &str, bytes: &[u8]) -> Result<String, PushError> {
    let got = bytes.len() as u64;
    if got != target.size {
        return Err(PushError::SizeMismatch {
            path: path.to_string(),
            want: target.size,
            got,
        });
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    if hash != target.hash {
        return Err(PushError::HashMismatch {
            path: path.to_string(),
            want: target.hash.clone(),
            got: hash,
        });
    }
    // Named by its hash: see the module header on why a client's path string
    // never reaches the filesystem.
    let staged = target.dir.join(&hash);
    std::fs::create_dir_all(&target.dir)
        .and_then(|()| std::fs::write(&staged, bytes))
        .map_err(|source| PushError::Staging {
            context: format!("writing {staged}"),
            source,
        })?;
    Ok(hash)
}

// ---------------------------------------------------------------------------
// The content source
// ---------------------------------------------------------------------------

/// A push session's staging area, seen as content for the index pipeline.
///
/// The map is built by the daemon from verified uploads, so a lookup either
/// finds a hash this daemon wrote or finds nothing — a manifest path is never
/// joined onto the filesystem.
pub struct StagedContent {
    dir: Utf8PathBuf,
    /// Project-relative path -> the hash its staged file is named by.
    by_path: BTreeMap<String, String>,
}

impl ContentSource for StagedContent {
    fn read(&self, path: &Utf8Path) -> std::io::Result<Vec<u8>> {
        // **Never `NotFound`.** To the pipeline `NotFound` means "gone since
        // the manifest was taken", and it answers by *deleting* the file. A
        // path this source cannot serve is a daemon-side gap — the pipeline
        // asked for content the negotiation said it already had, or a staged
        // file vanished out from under a commit — and a gap must be counted as
        // a pass error, never acted on as evidence of deletion.
        let missing = |detail: &str| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{path} has no staged content in this push session ({detail})"),
            )
        };
        let hash = self
            .by_path
            .get(path.as_str())
            .ok_or_else(|| missing("not among the paths this session uploaded"))?;
        std::fs::read(self.dir.join(hash)).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => missing("its staged file is gone"),
            _ => err,
        })
    }
}

// ---------------------------------------------------------------------------
// The reaper
// ---------------------------------------------------------------------------

/// Expire quiet leases and delete their staging areas until cancelled.
///
/// Ticks at half the TTL, so a lease is reaped within one tick of going
/// unreachable rather than up to a whole TTL late.
pub async fn reap(leases: PushLeases, cancel: CancellationToken) {
    let period = (leases.ttl() / 2).max(Duration::from_secs(1));
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // the first tick completes immediately
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {
                let expired = leases.reap();
                if !expired.is_empty() {
                    // Off the runtime: deleting a staging area is file IO, and
                    // the reaper shares its thread with every request handler.
                    let _ = tokio::task::spawn_blocking(move || {
                        for dir in expired {
                            discard(&dir);
                        }
                    })
                    .await;
                }
            }
        }
    }
    tracing::debug!("push lease reaper stopped");
}

/// Delete one staging area, loudly on failure and fatally to nobody: leftover
/// files belong to a session no lease can reach, so they are inert.
pub fn discard(dir: &Utf8Path) {
    if let Err(err) = remove_dir(dir) {
        tracing::warn!(dir = %dir, error = %err, "could not delete a push staging area");
    }
}

fn remove_dir(dir: &Utf8Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// An unguessable session handle (D-0015).
///
/// OS randomness, not a counter or a clock: a handle a second pusher can guess
/// lets its uploads land in the first one's commit, which is a consistency
/// failure whatever the trust model. It is still not a credential — see the
/// module header.
fn mint_session() -> PushSessionId {
    let mut bytes = [0u8; SESSION_BYTES];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    let mut handle = String::with_capacity(SESSION_BYTES * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(handle, "{byte:02x}");
    }
    PushSessionId(handle)
}
