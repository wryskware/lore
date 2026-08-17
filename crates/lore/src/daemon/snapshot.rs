//! What an index pass consumes: a manifest plus somewhere to get the bytes.
//!
//! D-0015 inverts ingestion — content reaches the index by push, and the index
//! owner never reads a filesystem it does not own. This module is that seam,
//! in-process. The walker (this machine's one filesystem observer) *produces*
//! a [`Snapshot`] using the `lore-core` wire types; [`super::index`] *consumes*
//! one, and cannot tell whether the bytes came off local disk or out of a push
//! session's staging area. That is the whole point: when the push routes land,
//! they build the same [`Snapshot`] from an upload and the pipeline is
//! untouched.
//!
//! Two properties the seam owes the pipeline, because both decide whether
//! files get *deleted*:
//!
//! - **Deletion is absence.** A stored file the manifest does not list is
//!   gone. There is no delete event to lose, reorder or replay.
//! - **Absence only counts inside the [`Scope`].** A full-project manifest
//!   speaks for the whole project; a watcher micro-manifest speaks only for
//!   the paths it names. Anything outside is not "absent", it is unobserved,
//!   and an incomplete or unreadable observation deletes nothing at all.

use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};
use tokio_util::sync::CancellationToken;

use lore_core::snapshot::{Manifest, ManifestEntry};

use super::walk;

/// Where a snapshot's file content comes from.
///
/// The pipeline reads through this and never learns the answer. Locally it is
/// the project's own disk ([`DiskContent`]); remotely it will be a push
/// session's staging directory.
pub trait ContentSource: Send {
    /// The bytes of one manifest entry.
    ///
    /// [`std::io::ErrorKind::NotFound`] means the file is gone since the
    /// manifest was taken — a walk-then-delete race locally, an abandoned
    /// upload remotely. Either way the pipeline drops the file rather than
    /// counting an error.
    fn read(&self, path: &Utf8Path) -> std::io::Result<Vec<u8>>;
}

/// The local content source: files under a project root, read on demand.
pub struct DiskContent {
    root: Utf8PathBuf,
}

impl DiskContent {
    pub fn new(root: &Utf8Path) -> Self {
        Self {
            root: root.to_owned(),
        }
    }
}

impl ContentSource for DiskContent {
    fn read(&self, path: &Utf8Path) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.root.join(path))
    }
}

/// What a manifest is a complete listing *of* — and therefore where absence
/// means deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The whole project. Any stored file the manifest omits is deleted; this
    /// is the shape D-0015 calls the push unit.
    Project,
    /// These paths and everything beneath them: the watcher's micro-manifest.
    /// A stored file under one of them and absent from the manifest is
    /// deleted; a file outside them is untouched, because this observation
    /// never looked at it.
    Paths(BTreeSet<Utf8PathBuf>),
}

/// One observation of a project, ready to apply.
pub struct Snapshot {
    pub manifest: Manifest,
    pub scope: Scope,
    /// False when the observation was cut short (shutdown mid-walk). Absence
    /// is evidence of deletion only in a *complete* listing, so an incomplete
    /// snapshot is applied by nobody.
    pub complete: bool,
    /// Paths the observer saw but could not read. Excluded from the manifest
    /// (there is no hash for them) *and* from deletion: a file that could not
    /// be read is not evidence that it is gone. Counted as pass errors.
    pub unreadable: BTreeSet<Utf8PathBuf>,
    pub content: Box<dyn ContentSource>,
}

impl Snapshot {
    /// Whether this observation covers `path` — i.e. whether the manifest not
    /// listing it means it was deleted.
    pub fn covers(&self, path: &Utf8Path) -> bool {
        match &self.scope {
            Scope::Project => true,
            Scope::Paths(paths) => paths
                .iter()
                .any(|scoped| path == scoped || path.as_str().starts_with(&format!("{scoped}/"))),
        }
    }

    /// Files this pass considered: everything the manifest lists, plus every
    /// named path that turned out to hold nothing (deleted, or no longer
    /// indexable). A named path that *expanded* into manifest entries — a
    /// directory the watcher reported — is not counted twice.
    pub fn considered(&self) -> usize {
        let empty = match &self.scope {
            Scope::Project => 0,
            Scope::Paths(paths) => paths
                .iter()
                .filter(|scoped| !self.manifest_holds_anything_under(scoped))
                .count(),
        };
        self.manifest.len() + empty
    }

    fn manifest_holds_anything_under(&self, scoped: &Utf8Path) -> bool {
        let prefix = format!("{scoped}/");
        self.manifest
            .entries
            .iter()
            .any(|entry| entry.path == scoped.as_str() || entry.path.starts_with(&prefix))
    }
}

/// Observe a whole project: the D-0015 push unit, produced locally.
///
/// `.loreignore` generation happens here rather than in the pipeline because
/// ignore evaluation is the *observer's* job — the pusher decides what to
/// send. Before the walk, not after: the file this may create is what the walk
/// immediately obeys. Never fatal — a root that cannot be written to falls
/// back to the walker's built-in exclusions.
pub fn observe_project(
    root: &Utf8Path,
    data_dir: &Utf8Path,
    cancel: &CancellationToken,
) -> Snapshot {
    super::ignorefile::ensure(root);
    let files = walk::walk_files(root, root, None, data_dir, Some(cancel));
    // A cancelled walk is partial: applying it would treat every unwalked file
    // as deleted.
    let complete = !cancel.is_cancelled();
    hash_all(root, files, Scope::Project, complete, cancel)
}

/// Observe exactly these project-relative paths — what a watcher batch turns
/// into.
///
/// A queued path may be a file, a directory (a whole tree was dropped in), or
/// gone entirely: the watcher cannot reliably tell, and by the time the batch
/// is processed the truth may have changed again. So the disk is consulted
/// here, once, and each case becomes manifest content or manifest absence:
///
/// - **gone**: absent from the manifest, in scope — deleted, and so is
///   anything indexed underneath it (that is how a deleted *directory* prunes
///   its files: the removal event names only the directory).
/// - **directory**: walked, and its files listed.
/// - **file**: confirmed to still pass the ignore rules by listing its parent
///   directory through the same walker the full scan uses. A file that no
///   longer passes (a new `.gitignore` rule, a rename into an excluded tree)
///   is absent from the manifest, and therefore deleted rather than left
///   behind as an orphan.
pub fn observe_paths(
    root: &Utf8Path,
    data_dir: &Utf8Path,
    requested: &BTreeSet<Utf8PathBuf>,
    cancel: &CancellationToken,
) -> Snapshot {
    let mut scope: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    let mut files: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    // Parent directory -> the files under it this batch mentions.
    let mut by_parent: BTreeMap<Utf8PathBuf, Vec<&Utf8Path>> = BTreeMap::new();

    for rel in requested {
        if walk::is_excluded_rel(rel) {
            continue;
        }
        scope.insert(rel.to_owned());
        let abs = root.join(rel);
        match std::fs::metadata(&abs) {
            Err(_) => {}
            Ok(meta) if meta.is_dir() => {
                files.extend(walk::walk_files(root, &abs, None, data_dir, Some(cancel)));
            }
            Ok(_) => {
                let parent = rel.parent().unwrap_or(Utf8Path::new("")).to_owned();
                by_parent.entry(parent).or_default().push(rel);
            }
        }
    }

    for (parent, named) in by_parent {
        let start = if parent.as_str().is_empty() {
            root.to_owned()
        } else {
            root.join(&parent)
        };
        let allowed: BTreeSet<Utf8PathBuf> =
            walk::walk_files(root, &start, Some(1), data_dir, Some(cancel))
                .into_iter()
                .collect();
        files.extend(
            named
                .into_iter()
                .filter(|rel| allowed.contains(*rel))
                .map(|rel| rel.to_owned()),
        );
    }

    // A cancelled walk above may have classified a still-allowed file as
    // missing (its parent listing came back partial); deleting on partial
    // evidence deletes real index rows.
    let complete = !cancel.is_cancelled();
    hash_all(root, files, Scope::Paths(scope), complete, cancel)
}

/// Read and hash every observed file into a manifest.
///
/// Hashing is what makes a listing a manifest, and it is the observer's cost
/// in every deployment. Locally it replaces the read-and-hash the indexer used
/// to do per file, so an unchanged tree still costs exactly one read and one
/// hash per file; a *changed* file is read once here and once more when the
/// pipeline asks for its content, which is the price of the seam and is served
/// from the page cache rather than the disk.
fn hash_all(
    root: &Utf8Path,
    files: impl IntoIterator<Item = Utf8PathBuf>,
    scope: Scope,
    complete: bool,
    cancel: &CancellationToken,
) -> Snapshot {
    let mut entries = Vec::new();
    let mut unreadable = BTreeSet::new();
    let mut complete = complete;

    for rel in files {
        if cancel.is_cancelled() {
            complete = false;
            break;
        }
        match std::fs::read(root.join(&rel)) {
            Ok(content) => entries.push(ManifestEntry {
                path: rel.to_string(),
                hash: blake3::hash(&content).to_hex().to_string(),
                size: content.len() as u64,
            }),
            // Vanished between the walk and the hash: absence is the honest
            // report, and the pipeline deletes it.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(path = %rel, error = %err, "unreadable file");
                unreadable.insert(rel);
            }
        }
    }

    Snapshot {
        manifest: Manifest::new(entries),
        scope,
        complete,
        unreadable,
        content: Box::new(DiskContent::new(root)),
    }
}
