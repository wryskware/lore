//! The source registry: `<data-dir>/projects.toml` plus stable project keys
//! (adversarial review S1#7 and S1#3).
//!
//! # Why the manifest exists
//!
//! D-0006 says the database is "only a derived, rebuildable index". That was
//! only true if something outside it remembered the *inputs* — and the list of
//! registered roots lived nowhere else. Deleting a corrupt `lore.db` therefore
//! destroyed durable configuration and left the user to repeat every
//! `lore add` from memory.
//!
//! So the manifest is **authoritative** and the `projects` table is derived
//! from it:
//!
//! ```toml
//! [[project]]
//! key = "lore"
//! name = "lore"
//! root = 'C:\repos\lore'
//! kind = "repo"
//! ```
//!
//! [`reconcile`] runs once at startup, before the normal rescan:
//!
//! - **manifest missing, database non-empty** → bootstrap the manifest from
//!   the database (this is the upgrade path, and the only direction in which
//!   the database is trusted);
//! - **otherwise the manifest wins** → rows it names are inserted or updated,
//!   rows it no longer lists are removed. Editing the file out from under the
//!   daemon is therefore a supported way to deregister a project.
//!
//! Writes are atomic (temp file in the same directory + rename), exactly like
//! [`crate::daemon::handshake`], so a reader can never see a half-written
//! registry.
//!
//! # Why keys exist
//!
//! Search results round-tripped the project *display name*, which only `root`
//! made unique. Two projects both named `shared` made `expand(project,
//! chunk_id)` resolve the wrong one or 404 (S1#3). Every source now carries a
//! stable opaque `key`, assigned once at registration and never recomputed —
//! renaming a project does not change it. Display names are additionally kept
//! unique at registration so the human-facing resolver stays deterministic.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasher, Hasher, RandomState};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::store::{Project, ProjectId, ProjectSpec, Store};
use crate::types::SourceKind;

/// File name within the data directory.
pub const MANIFEST_FILE: &str = "projects.toml";

/// Where a manifest that cannot be parsed is moved, so reconciliation can
/// proceed without destroying whatever the user actually wrote.
pub const MANIFEST_QUARANTINE: &str = "projects.toml.invalid";

/// Maximum length of the slug part of a key. Keys appear in agent-visible
/// output, so they stay short enough to be quoted without ceremony.
pub const MAX_SLUG_CHARS: usize = 40;

/// Fallback slug for a name with no usable characters at all (e.g. a project
/// named entirely in emoji).
const FALLBACK_SLUG: &str = "project";

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `[[project]]` tables, in registration order.
    #[serde(default, rename = "project")]
    pub projects: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Stable opaque key. Empty/absent is tolerated on read and filled in on
    /// the next write, so a hand-written manifest only needs `root`.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub name: String,
    pub root: Utf8PathBuf,
    #[serde(default)]
    pub kind: SourceKind,
}

impl From<&Project> for ManifestEntry {
    fn from(project: &Project) -> Self {
        Self {
            key: project.key.clone(),
            name: project.name.clone(),
            root: project.root.clone(),
            kind: project.kind,
        }
    }
}

pub fn manifest_path(data_dir: &Utf8Path) -> Utf8PathBuf {
    data_dir.join(MANIFEST_FILE)
}

/// `Ok(None)` means no manifest has ever been written here.
pub fn read(data_dir: &Utf8Path) -> Result<Option<Manifest>> {
    let path = manifest_path(data_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {path}")),
    };
    let manifest = toml::from_str(&raw).with_context(|| format!("parsing {path}"))?;
    Ok(Some(manifest))
}

/// Atomically publish `manifest` (temp file in the same directory, then
/// rename — the same mechanism and the same reasoning as the handshake).
pub fn write(data_dir: &Utf8Path, manifest: &Manifest) -> Result<()> {
    let target = manifest_path(data_dir);
    let temp = data_dir.join(format!("{MANIFEST_FILE}.{}.tmp", std::process::id()));
    let body = render(manifest)?;
    std::fs::write(&temp, body.as_bytes()).with_context(|| format!("writing {temp}"))?;
    match std::fs::rename(&temp, &target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(err).with_context(|| format!("publishing registry {target}"))
        }
    }
}

/// Serialize with a header explaining what the file is for. The manifest is
/// meant to be edited by hand when the daemon is down; a bare table of paths
/// with no explanation invites the wrong kind of edit.
fn render(manifest: &Manifest) -> Result<String> {
    let body = toml::to_string_pretty(manifest).context("serializing the project registry")?;
    Ok(format!(
        "# Lore source registry. This file is authoritative: the `projects` table\n\
         # in lore.db is derived from it and is rebuilt at startup. Removing an\n\
         # entry deregisters that project on the next daemon start; `key` is\n\
         # stable and must not be edited.\n\
         {body}"
    ))
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// Lowercase, dash-separated slug of a display name.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
            if out.chars().count() >= MAX_SLUG_CHARS {
                break;
            }
        } else {
            // Non-ASCII is folded to a separator rather than transliterated:
            // a key is an opaque handle, and the display name carries the
            // actual characters.
            pending_dash = true;
        }
    }
    if out.is_empty() {
        FALLBACK_SLUG.to_string()
    } else {
        out
    }
}

/// A key for `name` that is not in `taken`.
///
/// The slug is tried first, so the common case reads like the project
/// (`lore`, `lexomancy`). Only a genuine collision gets a suffix, and the
/// suffix is random rather than a counter: a counter would make the key a
/// function of *registration order*, which changes when the manifest is
/// reordered or a project is re-added, and the whole point of the key is that
/// it never changes.
pub fn allocate_key(name: &str, taken: &HashSet<String>) -> String {
    let base = slug(name);
    if !taken.contains(&base) {
        return base;
    }
    let hasher = RandomState::new();
    for attempt in 0..64u64 {
        let mut state = hasher.build_hasher();
        state.write(base.as_bytes());
        state.write_u64(attempt);
        let candidate = format!("{base}-{:04x}", state.finish() as u16);
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    // Astronomically unreachable; a monotonic fallback beats looping forever.
    (0..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unbounded sequence contains a free key")
}

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// What [`reconcile`] did, for the startup log and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// No manifest existed; one was written from the database (which may have
    /// been empty). This is the upgrade path from a pre-manifest daemon.
    Bootstrapped {
        projects: usize,
        keys_assigned: usize,
    },
    /// A manifest existed and was applied to the database.
    Applied {
        inserted: usize,
        updated: usize,
        removed: usize,
    },
}

/// Bring the `projects` table in line with the manifest (or vice versa, once).
///
/// Synchronous and file-touching on purpose: the caller runs it inside one
/// store acquisition on the blocking pool, before anything is watched or
/// scanned, so no index work can observe a half-reconciled registry.
pub fn reconcile(store: &mut Store, data_dir: &Utf8Path) -> Result<Reconciliation> {
    let manifest = match read(data_dir) {
        Ok(manifest) => manifest,
        Err(err) => {
            // Never overwrite something the user wrote and we failed to
            // understand: move it aside, say so loudly, and rebuild from the
            // database. The quarantined file is the recovery material.
            let quarantine = data_dir.join(MANIFEST_QUARANTINE);
            tracing::error!(
                error = %format!("{err:#}"),
                quarantine = %quarantine,
                "project registry is unreadable; moving it aside and rebuilding from the index"
            );
            let _ = std::fs::rename(manifest_path(data_dir), &quarantine);
            None
        }
    };

    match manifest {
        None => bootstrap(store, data_dir),
        Some(manifest) => apply(store, data_dir, manifest),
    }
}

fn bootstrap(store: &mut Store, data_dir: &Utf8Path) -> Result<Reconciliation> {
    let projects = store.list_projects()?;
    let mut taken: HashSet<String> = projects
        .iter()
        .map(|project| project.key.clone())
        .filter(|key| !key.is_empty())
        .collect();

    let mut keys_assigned = 0usize;
    let mut entries: Vec<ManifestEntry> = Vec::with_capacity(projects.len());
    for project in &projects {
        let mut entry = ManifestEntry::from(project);
        if entry.key.is_empty() {
            entry.key = allocate_key(&entry.name, &taken);
            taken.insert(entry.key.clone());
            store.upsert_project(&entry.root, &entry.name, Some(&entry.key), entry.kind)?;
            keys_assigned += 1;
        }
        entries.push(entry);
    }

    write(data_dir, &Manifest { projects: entries })?;
    Ok(Reconciliation::Bootstrapped {
        projects: projects.len(),
        keys_assigned,
    })
}

/// Normalize the manifest, then hand the whole end state to the store in one
/// transaction.
///
/// The two halves are deliberately separated. Normalization is pure — it
/// repairs the manifest (fills in names and keys, drops a repeated root) using
/// only the manifest's own contents, so "legal manifest" means *self*-consistent
/// and never depends on what the database happens to hold. The apply is then a
/// single atomic step, because the intermediate states between two legal
/// registries need not themselves be legal: exchanging two projects' keys is
/// the canonical example, and row-at-a-time upserts used to strand it
/// half-applied forever (see [`Store::apply_project_set`]).
///
/// A failed apply therefore leaves *both* the database and the manifest exactly
/// as they were, so the next start reconciles the same inputs rather than a
/// torn intermediate.
fn apply(store: &mut Store, data_dir: &Utf8Path, manifest: Manifest) -> Result<Reconciliation> {
    let before = store.list_projects()?;
    let known: HashMap<String, ProjectId> =
        before.iter().map(|p| (root_key(&p.root), p.id)).collect();
    let mut taken: HashSet<String> = HashSet::new();

    let mut listed: HashSet<String> = HashSet::new();
    let mut normalized: Vec<ManifestEntry> = Vec::with_capacity(manifest.projects.len());
    let mut desired: Vec<ProjectSpec> = Vec::with_capacity(manifest.projects.len());
    let (mut inserted, mut updated) = (0usize, 0usize);

    for mut entry in manifest.projects {
        if !listed.insert(root_key(&entry.root)) {
            tracing::warn!(root = %entry.root, "duplicate root in the project registry; ignoring the later entry");
            continue;
        }
        if entry.name.trim().is_empty() {
            entry.name = entry
                .root
                .file_name()
                .unwrap_or(entry.root.as_str())
                .to_string();
        }
        if entry.key.is_empty() || !taken.insert(entry.key.clone()) {
            if !entry.key.is_empty() {
                tracing::warn!(key = %entry.key, root = %entry.root, "duplicate project key in the registry; reassigning");
            }
            entry.key = allocate_key(&entry.name, &taken);
            taken.insert(entry.key.clone());
        }

        let id = known.get(&root_key(&entry.root)).copied();
        if id.is_some() {
            updated += 1;
        } else {
            inserted += 1;
        }
        desired.push(ProjectSpec {
            id,
            root: entry.root.clone(),
            name: entry.name.clone(),
            key: entry.key.clone(),
            kind: entry.kind,
        });
        normalized.push(entry);
    }

    let mut remove: Vec<ProjectId> = Vec::new();
    for project in &before {
        if !listed.contains(&root_key(&project.root)) {
            tracing::info!(
                project = %project.name,
                root = %project.root,
                "project is no longer in the registry; dropping its index"
            );
            remove.push(project.id);
        }
    }
    let removed = remove.len();

    store.apply_project_set(&desired, &remove)?;

    // Rewrite whenever normalization changed anything (a filled-in key, a
    // dropped duplicate); an unchanged manifest is left alone so the file's
    // mtime tracks real registry changes.
    let rewritten = Manifest {
        projects: normalized,
    };
    if read(data_dir)?.as_ref() != Some(&rewritten) {
        write(data_dir, &rewritten)?;
    }

    Ok(Reconciliation::Applied {
        inserted,
        updated,
        removed,
    })
}

/// Rewrite the manifest from the current contents of the `projects` table.
/// Called after a registration, which the running daemon applies to the
/// database first (it is the one that canonicalizes the root).
pub fn publish(store: &Store, data_dir: &Utf8Path) -> Result<()> {
    let projects = store.list_projects()?;
    write(
        data_dir,
        &Manifest {
            projects: projects.iter().map(ManifestEntry::from).collect(),
        },
    )
}

/// Comparison key for a root path, matching [`crate::daemon::paths`]'s
/// Windows ASCII-case-insensitivity. Roots reach here canonicalized, but a
/// hand-edited manifest need not be.
fn root_key(root: &Utf8Path) -> String {
    let trimmed = root
        .as_str()
        .trim_end_matches(['\\', '/'])
        .replace('\\', "/");
    if cfg!(windows) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed
    }
}

/// Display names that are already in use by a *different* root — the check
/// registration runs so name resolution stays deterministic (S1#3).
pub fn names_taken_by_others(projects: &[Project], root: &Utf8Path) -> BTreeSet<String> {
    let mine = root_key(root);
    projects
        .iter()
        .filter(|project| root_key(&project.root) != mine)
        .map(|project| project.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_lowercase_ascii_with_single_separators() {
        assert_eq!(slug("lore"), "lore");
        assert_eq!(slug("Lexomancy"), "lexomancy");
        assert_eq!(slug("My Design Vault"), "my-design-vault");
        assert_eq!(slug("  spaced  out  "), "spaced-out");
        assert_eq!(slug("a/b\\c"), "a-b-c");
        assert_eq!(slug("données"), "donn-es");
        assert_eq!(slug("🙂"), FALLBACK_SLUG);
        assert_eq!(slug(""), FALLBACK_SLUG);
        assert!(slug(&"x".repeat(200)).chars().count() <= MAX_SLUG_CHARS);
    }

    #[test]
    fn a_collision_gets_a_suffix_and_the_original_keeps_the_bare_slug() {
        let mut taken = HashSet::new();
        let first = allocate_key("shared", &taken);
        assert_eq!(first, "shared");
        taken.insert(first);

        let second = allocate_key("shared", &taken);
        assert_ne!(second, "shared");
        assert!(second.starts_with("shared-"), "{second}");
        assert_eq!(second.len(), "shared-".len() + 4, "{second}");
        taken.insert(second.clone());

        let third = allocate_key("shared", &taken);
        assert!(third != second && third.starts_with("shared-"), "{third}");
    }

    #[test]
    fn a_manifest_round_trips_through_toml_with_windows_roots() {
        let manifest = Manifest {
            projects: vec![
                ManifestEntry {
                    key: "lore".into(),
                    name: "lore".into(),
                    root: Utf8PathBuf::from(r"C:\repos\lore"),
                    kind: SourceKind::Repo,
                },
                ManifestEntry {
                    key: "sessions-1a2b".into(),
                    name: "sessions".into(),
                    root: Utf8PathBuf::from(r"C:\Users\w\AppData\Local\lore\sessions"),
                    kind: SourceKind::Session,
                },
            ],
        };
        let rendered = render(&manifest).unwrap();
        assert!(rendered.starts_with("# Lore source registry"), "{rendered}");
        let parsed: Manifest = toml::from_str(&rendered).unwrap();
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn a_hand_written_manifest_needs_only_a_root() {
        let parsed: Manifest = toml::from_str("[[project]]\nroot = 'C:\\repos\\x'\n").unwrap();
        assert_eq!(parsed.projects.len(), 1);
        assert!(parsed.projects[0].key.is_empty());
        assert_eq!(parsed.projects[0].kind, SourceKind::Repo);
    }

    #[test]
    fn root_comparison_ignores_separators_and_trailing_slashes() {
        assert_eq!(
            root_key(Utf8Path::new(r"C:\repos\lore\")),
            root_key(Utf8Path::new("C:/repos/lore"))
        );
        assert_ne!(
            root_key(Utf8Path::new("C:/repos/lore")),
            root_key(Utf8Path::new("C:/repos/lore-other"))
        );
    }
}
