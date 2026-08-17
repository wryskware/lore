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

use crate::sources::Sources;

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

/// The local content source: files under a project's declared source roots,
/// read on demand.
///
/// Holds the whole [`Sources`] rather than one root because a manifest path is
/// *logical* — `<mount>/<path>` — and only the source table knows which
/// physical directory that names.
pub struct DiskContent {
    sources: Sources,
}

impl DiskContent {
    pub fn new(sources: &Sources) -> Self {
        Self {
            sources: sources.clone(),
        }
    }
}

impl ContentSource for DiskContent {
    fn read(&self, path: &Utf8Path) -> std::io::Result<Vec<u8>> {
        match self.sources.resolve_path(path) {
            Some(abs) => std::fs::read(abs),
            // A path no source owns is a path that is gone: the mount that
            // held it was removed from `.lore.toml`, or renamed. `NotFound` is
            // exactly right — the pipeline drops the file rather than counting
            // an error, which is what a removed mount should do.
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no declared source owns `{path}`"),
            )),
        }
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
    /// Symbolic links (Windows junctions included) the observer declined to
    /// follow. Not part of the manifest and not a deletion signal — they are
    /// the observation's own account of what it chose not to look at, so a
    /// pass can say so instead of reporting a small `seen` with no
    /// explanation.
    pub links: BTreeSet<Utf8PathBuf>,
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
/// The walk *is* the observation. D-0020 collapsed what used to be three more
/// steps here — a `.loreignore` written before the walk, a git-aware basis
/// intersected after it, and a credential screen no ignore file could argue
/// with — into the one evaluator inside [`walk::walk_files`]. What a pusher
/// sends is now exactly what one ignore evaluation admits.
///
/// One walk per declared source root, and every path it yields carries that
/// source's mount. Rules travel down a root and never between roots, so each
/// walk gets its own evaluator with its own matcher stack — which is what the
/// `ignore` crate does naturally, and what the walker's `parents(false)` makes
/// true rather than merely intended.
pub fn observe_project(
    sources: &Sources,
    data_dir: &Utf8Path,
    cancel: &CancellationToken,
) -> Snapshot {
    let mut files: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    let mut links: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    for source in sources.iter() {
        let walk = walk::walk_files(&source.root, &source.root, None, data_dir, Some(cancel));
        files.extend(walk.files.iter().map(|rel| source.logical(rel)));
        links.extend(walk.links.iter().map(|rel| source.logical(rel)));
    }
    // A cancelled walk is partial: applying it would treat every unwalked file
    // as deleted.
    let complete = !cancel.is_cancelled();
    hash_all(sources, files, links, Scope::Project, complete, cancel)
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
///
/// Every verdict comes from the same [`walk::walk_files`] a full scan uses —
/// one evaluator, D-0020 — because a watcher event that indexed something a
/// rescan would delete is the failure this whole seam exists to prevent. It is
/// also the whole cost now: the git subprocess this used to spend per batch is
/// gone with the basis.
pub fn observe_paths(
    sources: &Sources,
    data_dir: &Utf8Path,
    requested: &BTreeSet<Utf8PathBuf>,
    cancel: &CancellationToken,
) -> Snapshot {
    let mut scope: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    let mut files: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    let mut links: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    // Physical parent directory -> the source that owns it and the logical
    // paths under it this batch mentions. Keyed physically because that is
    // what gets listed; the source is carried rather than looked up again,
    // because a parent that *is* a source root is not "inside" it and a
    // containment query would answer `None` for exactly the files sitting at
    // the top of a mount.
    let mut by_parent: BTreeMap<Utf8PathBuf, (&crate::sources::Source, Vec<&Utf8Path>)> =
        BTreeMap::new();

    for rel in requested {
        // The hard floor is the only thing rejected on a name. Every other
        // exclusion is an overridable rule (D-0020), so a cheap name-level
        // shortcut here would silently outrank the `.loreignore` that
        // re-included the path — and the walk below is what evaluates rules.
        if walk::is_git_metadata(rel) {
            continue;
        }
        // In scope whatever happens next: a path whose mount has since been
        // removed from `.lore.toml` has to be *deletable*, and absence from
        // the manifest is how deletion is said.
        scope.insert(rel.to_owned());
        let Some((source, abs)) = sources.resolve(rel) else {
            continue;
        };
        match std::fs::metadata(&abs) {
            Err(_) => {}
            Ok(meta) if meta.is_dir() => {
                let walk = walk::walk_files(&source.root, &abs, None, data_dir, Some(cancel));
                files.extend(walk.files.iter().map(|rel| source.logical(rel)));
                links.extend(walk.links.iter().map(|rel| source.logical(rel)));
            }
            Ok(_) => {
                let parent = abs.parent().unwrap_or(&source.root).to_owned();
                by_parent
                    .entry(parent)
                    .or_insert_with(|| (source, Vec::new()))
                    .1
                    .push(rel);
            }
        }
    }

    for (parent, (source, named)) in by_parent {
        // Rooted at the owning source, never at the parent: the crate matches
        // a parent `.loreignore` against the entry path alone, so a rule that
        // prunes this directory during a descent matches nothing once the walk
        // begins inside it.
        let walk = walk::walk_files(&source.root, &parent, Some(1), data_dir, Some(cancel));
        let allowed: BTreeSet<Utf8PathBuf> =
            walk.files.iter().map(|rel| source.logical(rel)).collect();
        links.extend(walk.links.iter().map(|rel| source.logical(rel)));
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
    hash_all(sources, files, links, Scope::Paths(scope), complete, cancel)
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
    sources: &Sources,
    files: impl IntoIterator<Item = Utf8PathBuf>,
    links: BTreeSet<Utf8PathBuf>,
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
        // Logical path in, physical read out: the source table is the only
        // thing that knows which declared root a manifest path names.
        let Some(abs) = sources.resolve_path(&rel) else {
            continue;
        };
        match std::fs::read(&abs) {
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
        links,
        content: Box::new(DiskContent::new(sources)),
    }
}
