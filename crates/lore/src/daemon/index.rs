//! Indexing: snapshot → chunks → store.
//!
//! The pipeline consumes a [`Snapshot`] — a manifest plus a content source —
//! and reconciles the store with it: index what changed, delete what the
//! manifest omits. It does **not** observe anything itself, which is D-0015's
//! inversion: the walker produces the snapshot ([`super::snapshot`]) today and
//! a push session will produce the identical thing tomorrow, and this module
//! cannot tell the difference.
//!
//! Two entry points, one file-level routine:
//!
//! - [`full_scan`] applies a whole-project snapshot (the push unit).
//! - [`index_paths`] applies a snapshot scoped to the paths a watcher batch
//!   named.
//!
//! Both are **synchronous** and take a [`StoreHandle`], because the store is
//! synchronous and file IO is blocking. The async layer ([`run`]) is a thin
//! pump that hands whole passes to `spawn_blocking`. That split is what makes
//! the interesting behaviour testable without a runtime, a watcher, or any
//! timing at all.
//!
//! Change detection is content hashing (blake3), not mtime: mtime lies across
//! branch switches, restores from backup and Unity's asset pipeline, and a
//! false "unchanged" is a permanently stale index. A file whose manifest hash
//! matches the stored one is never even read by the pipeline and touches no
//! store state — in particular it does not rewrite `indexed_at`, so re-scans
//! are genuinely free rather than merely fast.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use lore_core::snapshot::{ManifestEntry, MassDeleteTrip, mass_delete_trip};

use crate::authority::{DecisionIndex, Decisions, Demotion, is_decision_source, is_ledger_path};
use crate::chunk::{FileChunks, chunk_file};
use crate::repo_config::{Profile, RepoAuthority};
use crate::store::{FileWrite, Project, ProjectId, Recompute, StoreError};

use super::queue::{IndexQueue, ProjectWork};
use super::snapshot::{self, ContentSource, Scope, Snapshot};
use super::store_handle::StoreHandle;

/// Everything an index pass needs besides the work itself.
#[derive(Clone)]
pub struct IndexContext {
    pub store: StoreHandle,
    /// Excluded from every walk; the daemon's own writes must not feed back
    /// into the indexer.
    pub data_dir: Utf8PathBuf,
    pub cancel: CancellationToken,
    /// Pulsed at the end of every completed pass so the embed worker starts
    /// on new chunks immediately instead of at its next idle tick. Nothing
    /// depends on anyone listening — a lexical-only daemon simply has no
    /// subscriber.
    pub embed_notify: Arc<Notify>,
    /// Where a refused apply is recorded for `lore status`.
    pub guard: GuardStatus,
    /// Where a degraded manifest basis is recorded for `lore status`.
    pub basis: BasisStatus,
}

impl IndexContext {
    pub fn new(store: StoreHandle, data_dir: Utf8PathBuf, cancel: CancellationToken) -> Self {
        Self {
            store,
            data_dir,
            cancel,
            embed_notify: Arc::new(Notify::new()),
            guard: GuardStatus::new(),
            basis: BasisStatus::new(),
        }
    }
}

/// Per-pass decisions a caller may override.
///
/// Deliberately not a config struct: the only member is the mass-delete
/// override, which D-0015 requires to be per invocation and never a stored
/// setting. It travels as an argument for exactly that reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyOptions {
    /// Apply this snapshot even if it trips the mass-delete guard.
    pub allow_mass_delete: bool,
}

/// Per-project record of an apply the mass-delete guard refused, shared
/// read-only with `/v1/status`.
///
/// In memory only, like the embed worker's abandoned count: it describes what
/// this daemon refused to do, not what the index is. The next apply that
/// proceeds clears it, so the report never outlives the condition.
#[derive(Clone, Debug, Default)]
pub struct GuardStatus {
    trips: Arc<Mutex<BTreeMap<ProjectId, MassDeleteTrip>>>,
}

impl GuardStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn of(&self, project: ProjectId) -> Option<MassDeleteTrip> {
        self.lock().get(&project).copied()
    }

    fn record(&self, project: ProjectId, trip: MassDeleteTrip) {
        self.lock().insert(project, trip);
    }

    fn clear(&self, project: ProjectId) {
        self.lock().remove(&project);
    }

    /// Drop a deregistered project's record, so a forgotten project's refusal
    /// cannot attach itself to whatever row later inherits its id.
    pub fn forget(&self, project: ProjectId) {
        self.lock().remove(&project);
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<ProjectId, MassDeleteTrip>> {
        self.trips
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Per-project record that the last local observation could not take the
/// git-aware manifest basis (D-0017), shared read-only with `/v1/status`.
///
/// Same shape and same reasoning as [`GuardStatus`]: in memory, describing what
/// this daemon's last pass *did* rather than what the index is, and cleared by
/// the next pass that manages it. A degraded basis is not an error — the pass
/// succeeds on the walker's own rules, which over-include — but it silently
/// changes what a manifest means, and D-0012 already established that this
/// daemon's answer to a silent change of meaning is to report it.
#[derive(Clone, Debug, Default)]
pub struct BasisStatus {
    errors: Arc<Mutex<BTreeMap<ProjectId, String>>>,
}

impl BasisStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn of(&self, project: ProjectId) -> Option<String> {
        self.lock().get(&project).cloned()
    }

    fn set(&self, project: ProjectId, error: Option<&str>) {
        match error {
            Some(error) => self.lock().insert(project, error.to_string()),
            None => self.lock().remove(&project),
        };
    }

    /// Drop a deregistered project's record, for the reason
    /// [`GuardStatus::forget`] gives.
    pub fn forget(&self, project: ProjectId) {
        self.lock().remove(&project);
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<ProjectId, String>> {
        self.errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What one pass did. Logged verbatim; also the assertion surface for tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassSummary {
    /// Files considered (walked, or named by the watcher).
    pub seen: usize,
    /// Files whose chunks were (re)written.
    pub indexed: usize,
    /// Files whose content hash matched the stored one — no store write.
    pub unchanged: usize,
    /// Files the chunker refused (binary, oversize, invalid UTF-8).
    pub skipped: usize,
    /// Files dropped from the index (deleted, ignored, or newly binary).
    pub removed: usize,
    pub chunks_inserted: usize,
    pub chunks_kept: usize,
    pub chunks_deleted: usize,
    /// Files that could not be read or written; the pass continues.
    pub errors: usize,
    /// Files whose effective authority was rewritten by the recompute pass
    /// (a ledger edit changed what `decided` is allowed to mean).
    pub authority_recomputed: usize,
    /// Files declaring `decided` without citing an active decision, as of the
    /// end of this pass.
    pub authority_violations: usize,
    /// Decision records excluded for an identity defect — a heading that
    /// disagrees with its filename, or a duplicated id (D-0013).
    pub decision_violations: usize,
    /// The repo's `.lore.toml` verdict differs from the stored one, so this
    /// pass re-chunks the project's Markdown (D-0012).
    pub profile_changed: bool,
    /// The repo has a `.lore.toml` Lore cannot use; it indexed neutrally.
    pub config_error: bool,
    /// True when the pass stopped early because shutdown was requested.
    pub cancelled: bool,
    /// Set when the mass-delete guard refused this snapshot (D-0015). The
    /// pass then writes nothing at all: a refused apply is refused whole.
    pub mass_delete_blocked: Option<MassDeleteTrip>,
    /// The generation this pass advanced to. Zero for a pass that never
    /// completed (cancelled, or the bump itself failed) — which is the same
    /// thing `finish` logs, and what a push commit reports back to its pusher.
    pub generation: u64,
}

impl PassSummary {
    fn record(&mut self, write: FileWrite) {
        self.indexed += 1;
        self.chunks_inserted += write.inserted;
        self.chunks_kept += write.kept;
        self.chunks_deleted += write.deleted;
    }
}

/// Full reconciliation of one project against its own filesystem.
pub fn full_scan(ctx: &IndexContext, project: &Project) -> PassSummary {
    full_scan_with(ctx, project, ApplyOptions::default())
}

/// [`full_scan`] with the per-invocation overrides an explicit `lore index`
/// may carry.
pub fn full_scan_with(ctx: &IndexContext, project: &Project, options: ApplyOptions) -> PassSummary {
    let started = Instant::now();
    let snapshot = snapshot::observe_project(&project.root, &ctx.data_dir, &ctx.cancel);
    apply(ctx, project, snapshot, options, started)
}

/// Index exactly these project-relative paths — what a watcher batch turns
/// into. The scoped snapshot ([`snapshot::observe_paths`]) decides what the
/// batch actually means; this is only the plumbing.
pub fn index_paths(
    ctx: &IndexContext,
    project: &Project,
    requested: &BTreeSet<Utf8PathBuf>,
) -> PassSummary {
    let started = Instant::now();
    let snapshot = snapshot::observe_paths(&project.root, &ctx.data_dir, requested, &ctx.cancel);
    apply(ctx, project, snapshot, ApplyOptions::default(), started)
}

/// Apply a snapshot this daemon did not observe — the push commit path
/// (D-0015).
///
/// The whole seam in one function: a push session's manifest and staging area
/// arrive as an ordinary [`Snapshot`], and everything below this line is the
/// same code the walker's snapshot runs through. The scope is the caller's to
/// set, and for a push it is [`Scope::Project`] — a manifest is the complete
/// listing, which is what makes its absences deletions.
pub fn apply_snapshot(
    ctx: &IndexContext,
    project: &Project,
    snapshot: Snapshot,
    options: ApplyOptions,
) -> PassSummary {
    apply(ctx, project, snapshot, options, Instant::now())
}

/// The stored `files.content_hash` for content that hashed to `hash`.
///
/// Not the bare content hash: the chunk format version rides in it so a
/// chunking-policy bump invalidates every short-circuit, and the active
/// authority profile rides along for Markdown so a `.lore.toml` change
/// re-chunks the documents whose *metadata* it changed (D-0012).
///
/// Public because the push manifest diff has to ask the same question the
/// pipeline asks — "does the store already have this content?" — and a second,
/// almost-identical stamping rule would present as a daemon that re-requests
/// every file it already has after a format bump, or worse, one that skips
/// files it should re-chunk.
pub fn content_stamp(rel: &Utf8Path, hash: &str, profile: Option<Profile>) -> String {
    let profile_tag = match profile {
        Some(profile) if crate::chunk::is_markdown(rel) => profile.chunk_tag(),
        _ => "",
    };
    format!(
        "v{}{profile_tag}-{hash}",
        crate::chunk::CHUNK_FORMAT_VERSION
    )
}

/// Reconcile the store with one snapshot.
///
/// Diff-then-apply rather than prune-by-generation: it needs no extra column,
/// and it is correct even if the daemon was killed mid-pass — the next pass
/// sees the same difference. The whole diff comes from one `list_files`, which
/// is what a manifest diff *is*.
fn apply(
    ctx: &IndexContext,
    project: &Project,
    snapshot: Snapshot,
    options: ApplyOptions,
    started: Instant,
) -> PassSummary {
    let mut summary = PassSummary::default();
    let kind = match snapshot.scope {
        Scope::Project => "full_scan",
        Scope::Paths(_) => "incremental",
    };
    let profile = refresh_profile(ctx, project, &mut summary);
    // Recorded before the early returns below: a pass that a cancelled walk or
    // the mass-delete guard cuts short still observed the basis it observed,
    // and the degradation is exactly the kind of thing that would explain the
    // shrunken listing the guard just refused.
    ctx.basis.set(project.id, snapshot.basis_error.as_deref());

    summary.seen = snapshot.considered();
    // A file the observer could not read is not evidence of anything: it is
    // neither indexed nor deleted, only counted.
    summary.errors += snapshot.unreadable.len();
    // An incomplete observation must not delete, and must not half-index
    // either — the next pass sees the same difference and finishes the job.
    summary.cancelled = !snapshot.complete || ctx.cancel.is_cancelled();
    if summary.cancelled {
        finish(ctx, project, kind, started, &mut summary);
        return summary;
    }

    let stored = match ctx.store.blocking(|store| store.list_files(project.id)) {
        Ok(stored) => stored,
        Err(err) => {
            tracing::warn!(project = %project.name, error = %err, "listing files for the snapshot diff failed");
            summary.errors += 1;
            finish(ctx, project, kind, started, &mut summary);
            return summary;
        }
    };

    // Deletion is absence from the manifest — inside the snapshot's scope, and
    // never for a file the observer merely failed to read.
    let deletions: Vec<Utf8PathBuf> = stored
        .iter()
        .filter(|record| {
            snapshot.manifest.get(record.path.as_str()).is_none()
                && snapshot.covers(&record.path)
                && !snapshot.unreadable.contains(&record.path)
        })
        .map(|record| record.path.clone())
        .collect();

    if !options.allow_mass_delete
        && let Some(trip) = mass_delete_trip(deletions.len() as u64, stored.len() as u64)
    {
        // Loud, refused whole, and remembered: an index that stops tracking
        // its project has to say so rather than quietly shrink.
        tracing::error!(
            project = %project.name,
            deletes = trip.deletes,
            stored = trip.stored,
            "mass-delete guard tripped; this pass was refused and nothing was written \
             (re-run `lore index --allow-mass-delete` if the deletion is intended)"
        );
        summary.mass_delete_blocked = Some(trip);
        ctx.guard.record(project.id, trip);
        finish(ctx, project, kind, started, &mut summary);
        return summary;
    }
    ctx.guard.clear(project.id);

    let known: BTreeMap<&str, &str> = stored
        .iter()
        .map(|record| (record.path.as_str(), record.content_hash.as_str()))
        .collect();
    for entry in &snapshot.manifest.entries {
        if ctx.cancel.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        index_one(
            ctx,
            project,
            entry,
            known.get(entry.path.as_str()).copied(),
            snapshot.content.as_ref(),
            profile,
            &mut summary,
        );
    }

    if !summary.cancelled {
        remove_all(ctx, project, &deletions, &mut summary);
        match snapshot.scope {
            // Unconditional on a full scan: this is also the migration/startup
            // backfill path, where the stored effective tiers may be the V2
            // migration's declared-tier approximation and the active-decision
            // set may never have been parsed at all.
            Scope::Project => refresh_authority(ctx, project, profile, true, &mut summary),
            // Otherwise only when this batch touched a decision source — a
            // mono ledger or a per-file record (D-0013). Every *other* file
            // already got its effective tier stamped on write; re-deriving the
            // whole project because one source file changed would be pure
            // waste.
            //
            // A profile change invalidates every tier in the project, not just
            // this batch's. The recompute below fixes the tiers; the *chunks*
            // of files outside this batch are still stale, which is why the
            // watcher answers a `.lore.toml` edit with a full scan rather than
            // a batch. This branch is the safety net for any other route into
            // an incremental pass with a moved profile.
            Scope::Paths(_) => {
                let decisions_touched = snapshot
                    .manifest
                    .entries
                    .iter()
                    .map(|entry| Utf8Path::new(&entry.path))
                    .chain(deletions.iter().map(Utf8PathBuf::as_path))
                    .any(is_decision_source);
                if decisions_touched || summary.profile_changed {
                    refresh_authority(ctx, project, profile, summary.profile_changed, &mut summary);
                }
            }
        }
    }

    finish(ctx, project, kind, started, &mut summary);
    summary
}

/// Re-read the repo's `.lore.toml` and persist the verdict (D-0012).
///
/// Runs at the top of **every** pass, before a single file is hashed, because
/// the profile decides how the files in this pass are chunked. The stored
/// verdict doubles as the profile fingerprint: `set_project_authority`
/// reporting a change is how "someone added, edited or deleted `.lore.toml`"
/// reaches the rest of the pipeline, and the change is loud in the log because
/// it silently alters what every document in the repo means.
///
/// Config problems do not stop the pass. The repo indexes neutrally and the
/// error is stored where `lore status` will put it in front of the user —
/// visible failure, not fatal failure.
fn refresh_profile(
    ctx: &IndexContext,
    project: &Project,
    summary: &mut PassSummary,
) -> Option<Profile> {
    let authority = RepoAuthority::load(&project.root);
    let id = project.id;
    let stored = {
        let authority = authority.clone();
        ctx.store
            .blocking(move |store| store.set_project_authority(id, &authority))
    };
    match stored {
        Ok(true) => {
            tracing::info!(
                project = %project.name,
                profile = authority.profile.map(Profile::as_str).unwrap_or("none"),
                behavior = authority.behavior.as_str(),
                "authority profile changed; the project's Markdown will re-chunk"
            );
            summary.profile_changed = true;
        }
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(project = %project.name, error = %err, "storing the authority profile failed");
            summary.errors += 1;
        }
    }
    // Loud on every pass, not only when it changed: a broken `.lore.toml` that
    // is broken again tomorrow is still a repo silently running neutral.
    if let Some(error) = &authority.error {
        tracing::warn!(
            project = %project.name,
            error = %error,
            "{} is not usable; the project indexes with no authority semantics (see `lore status`)",
            crate::repo_config::REPO_CONFIG_FILE,
        );
        summary.config_error = true;
    }
    authority.active()
}

/// Re-read the project's decision corpus, and rebuild every effective tier if
/// that changed what counts as canon (or if `force`, which is how a full scan
/// doubles as the migration/startup backfill).
///
/// Sources are read from **disk**, not reassembled from their chunks: chunk
/// text is a verbatim slice but the chunker splits and trims, and parsing a
/// reassembly of the thing that defines canon is exactly where a subtle
/// mis-parse would be least visible.
///
/// Two shapes feed one corpus (D-0013): the mono ledger
/// `**/0_Canon/DECISIONS.md` and per-file records
/// `**/0_Canon/decisions/D-NNNN-<slug>.md`. They are resolved *together*,
/// because either may supersede the other.
///
/// A project may legitimately have more than one ledger (two vaults in one
/// repo); their active sets are unioned, because a document citing either
/// ledger is citing an active decision *of this project*. Two vaults that
/// number their decisions independently therefore share one `D-NNNN`
/// namespace and can collide — a known limitation, deliberately left here:
/// D-0012 defers multi-root ID-namespace resolution.
///
/// With no profile in force there is no decision corpus at all: the stored set
/// is emptied so nothing lingers from a repo's configured past, and the
/// recompute flattens every tier back to neutral.
fn refresh_authority(
    ctx: &IndexContext,
    project: &Project,
    profile: Option<Profile>,
    force: bool,
    summary: &mut PassSummary,
) {
    let mut sources = 0usize;
    let decisions = match profile {
        None => Decisions::default(),
        Some(_) => {
            let files = match ctx.store.blocking(|store| store.list_files(project.id)) {
                Ok(files) => files,
                Err(err) => {
                    tracing::warn!(project = %project.name, error = %err, "listing files for the ledger scan failed");
                    summary.errors += 1;
                    return;
                }
            };

            let mut index = DecisionIndex::default();
            let mut unreadable = false;
            for record in files.iter().filter(|f| is_decision_source(&f.path)) {
                sources += 1;
                match std::fs::read_to_string(project.root.join(&record.path)) {
                    Ok(text) if is_ledger_path(&record.path) => {
                        index.add_ledger(&record.path, &text);
                    }
                    Ok(text) => index.add_record(&record.path, &text),
                    Err(err) => {
                        // An unreadable file is a transient condition, and
                        // treating it as "no decisions are active" would
                        // mass-demote the vault. The previously stored set is
                        // folded back in below.
                        tracing::warn!(
                            project = %project.name,
                            path = %record.path,
                            error = %err,
                            "decision source is unreadable; leaving its decisions active"
                        );
                        summary.errors += 1;
                        unreadable = true;
                    }
                }
            }
            let mut decisions = index.resolve();
            if unreadable {
                match ctx
                    .store
                    .blocking(|store| store.active_decisions(project.id))
                {
                    Ok(previous) => decisions.active.extend(previous),
                    Err(err) => {
                        tracing::warn!(project = %project.name, error = %err, "reading the stored decision set failed")
                    }
                }
                decisions.total = decisions.total.max(decisions.active.len());
            }
            decisions
        }
    };

    for violation in &decisions.violations {
        tracing::warn!(
            project = %project.name,
            path = %violation.path,
            detail = %violation.detail,
            // Covers both kinds the resolver reports: a record excluded from
            // the active set, and a project carrying more than one authority
            // root. The detail says which.
            "decision corpus defect"
        );
    }
    summary.decision_violations = decisions.violations.len();

    let changed = {
        let decisions = decisions.clone();
        match ctx
            .store
            .blocking(move |store| store.set_decisions(project.id, &decisions))
        {
            Ok(changed) => changed,
            Err(err) => {
                tracing::warn!(project = %project.name, error = %err, "storing the active decision set failed");
                summary.errors += 1;
                return;
            }
        }
    };
    if !changed && !force {
        return;
    }

    match ctx
        .store
        .blocking(|store| store.recompute_effective_authority(project.id))
    {
        Ok(recompute) => {
            record_recompute(project, sources, &decisions, changed, recompute, summary)
        }
        Err(err) => {
            tracing::warn!(project = %project.name, error = %err, "recomputing effective authority failed");
            summary.errors += 1;
        }
    }
}

fn record_recompute(
    project: &Project,
    sources: usize,
    decisions: &Decisions,
    ledger_changed: bool,
    recompute: Recompute,
    summary: &mut PassSummary,
) {
    summary.authority_recomputed = recompute.files_changed;
    summary.authority_violations = recompute.violations;
    if ledger_changed || recompute.files_changed > 0 {
        tracing::info!(
            project = %project.name,
            decision_sources = sources,
            active_decisions = decisions.active.len(),
            total_decisions = decisions.total,
            files_changed = recompute.files_changed,
            chunks_changed = recompute.chunks_changed,
            violations = recompute.violations,
            "effective authority recomputed"
        );
    }
    if recompute.violations > 0 {
        tracing::warn!(
            project = %project.name,
            files = recompute.violations,
            "files declare `decided` without citing an active decision; they rank as neutral (see `lore status`)"
        );
    }
}

/// Fetch, chunk and store one manifest entry. All error paths are logged and
/// counted, never fatal: one unreadable file must not abort a pass.
fn index_one(
    ctx: &IndexContext,
    project: &Project,
    entry: &ManifestEntry,
    stored_hash: Option<&str>,
    content_source: &dyn ContentSource,
    profile: Option<Profile>,
    summary: &mut PassSummary,
) {
    let rel = Utf8Path::new(&entry.path);

    // [`content_stamp`] carries the chunk format version and — for Markdown —
    // the active authority profile, so a policy bump or a `.lore.toml` change
    // invalidates the short-circuit below and unchanged bytes re-chunk.
    let stamp = |hash: &str| content_stamp(rel, hash, profile);

    // The manifest hash is what the diff is done against; nothing is fetched
    // for a file whose content the store already has.
    if stored_hash == Some(stamp(&entry.hash).as_str()) {
        summary.unchanged += 1;
        return;
    }

    let content = match content_source.read(rel) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Gone between the manifest and the fetch.
            remove_one(ctx, project, rel, summary);
            return;
        }
        Err(err) => {
            tracing::warn!(project = %project.name, path = %rel, error = %err, "unreadable file");
            summary.errors += 1;
            return;
        }
    };

    // Hashed again from the bytes that are about to be chunked, not copied
    // from the manifest: the stored hash must always describe what the chunks
    // were made of, whatever the observer declared or however far the file has
    // moved on since.
    let hash = stamp(&blake3::hash(&content).to_hex());

    match chunk_file(rel, &content, profile) {
        FileChunks::Chunked(chunks) => {
            let written = ctx
                .store
                .blocking(|store| store.replace_file_chunks(project.id, rel, &hash, &chunks));
            match written {
                Ok(write) => {
                    tracing::debug!(
                        project = %project.name,
                        path = %rel,
                        inserted = write.inserted,
                        kept = write.kept,
                        deleted = write.deleted,
                        effective_tier = write.authority.tier,
                        "indexed file"
                    );
                    // One line per offending file, at index time: `status`
                    // gives the count, the log says which file and why.
                    if let Some(demotion) = write.authority.demotion
                        && demotion.is_violation()
                    {
                        log_violation(project, rel, demotion);
                    }
                    summary.record(write);
                }
                Err(err) => {
                    log_store_error(project, rel, &err);
                    summary.errors += 1;
                }
            }
        }
        FileChunks::Skipped(reason) => {
            summary.skipped += 1;
            tracing::debug!(project = %project.name, path = %rel, ?reason, "skipped file");
            // A file that *became* unindexable — whatever the reason — must
            // lose its stale chunks. This deliberately does not enumerate
            // `SkipReason` variants: an earlier enumerated list silently
            // exempted the later-added `MachineText`, leaving a pruned-policy
            // file searchable forever (caught dogfooding on Lexomancy).
            // `remove_one` is a no-op for files that were never indexed.
            remove_one(ctx, project, rel, summary);
        }
    }
}

fn remove_one(ctx: &IndexContext, project: &Project, rel: &Utf8Path, summary: &mut PassSummary) {
    match ctx
        .store
        .blocking(|store| store.remove_file(project.id, rel))
    {
        Ok(Some(chunks)) => {
            tracing::debug!(project = %project.name, path = %rel, chunks, "removed file from index");
            summary.removed += 1;
            // Deletions through this path are chunk deletions too. Counting
            // only the ones `replace_file_chunks` reports made a prune look
            // like it touched nothing (issue #9: removed=19, deleted=0).
            summary.chunks_deleted += chunks;
        }
        Ok(None) => {}
        Err(err) => {
            log_store_error(project, rel, &err);
            summary.errors += 1;
        }
    }
}

/// Drop every file the snapshot's manifest omitted — deleted on disk, no
/// longer passing the ignore rules, or under a directory that vanished. All
/// three present identically: the observation covered them and did not list
/// them.
fn remove_all(
    ctx: &IndexContext,
    project: &Project,
    deletions: &[Utf8PathBuf],
    summary: &mut PassSummary,
) {
    // Removals are idempotent, so stopping mid-list is safe: the next pass
    // sees the same difference and finishes the job.
    for rel in deletions {
        if ctx.cancel.is_cancelled() {
            summary.cancelled = true;
            return;
        }
        remove_one(ctx, project, rel, summary);
    }
}

/// Close out a pass: bump the generation and log one summary line.
///
/// The generation bumps on every *completed* pass, even one that changed
/// nothing. It is the client's "did the index move under me?" signal and,
/// more practically, the thing `lore index` polls to learn that its request
/// finished; a no-op pass that never bumped would hang that poll forever.
fn finish(
    ctx: &IndexContext,
    project: &Project,
    kind: &'static str,
    started: Instant,
    summary: &mut PassSummary,
) {
    if summary.cancelled {
        tracing::info!(project = %project.name, kind, "index pass cancelled by shutdown");
        return;
    }
    let generation = ctx.store.blocking(|store| store.bump_generation());
    let generation = match generation {
        Ok(generation) => generation,
        Err(err) => {
            tracing::warn!(project = %project.name, error = %err, "bumping generation failed");
            0
        }
    };
    summary.generation = generation;
    tracing::info!(
        project = %project.name,
        kind,
        generation,
        seen = summary.seen,
        indexed = summary.indexed,
        unchanged = summary.unchanged,
        skipped = summary.skipped,
        removed = summary.removed,
        chunks_inserted = summary.chunks_inserted,
        chunks_kept = summary.chunks_kept,
        chunks_deleted = summary.chunks_deleted,
        authority_recomputed = summary.authority_recomputed,
        authority_violations = summary.authority_violations,
        decision_violations = summary.decision_violations,
        profile_changed = summary.profile_changed,
        config_error = summary.config_error,
        mass_delete_blocked = summary.mass_delete_blocked.is_some(),
        errors = summary.errors,
        duration_ms = started.elapsed().as_millis() as u64,
        "index pass complete"
    );

    // Wake the embed worker. `notify_one` rather than `notify_waiters` on
    // purpose: it stores a permit, so a pulse that lands while the worker is
    // mid-batch still wakes it afterwards instead of being dropped. Pulsed on
    // every completed pass, not only on passes that wrote chunks — a pass
    // that merely *kept* chunks can still have left work (vectors discarded
    // by a fingerprint reset), and an unnecessary pulse costs one no-op query.
    ctx.embed_notify.notify_one();
}

fn log_store_error(project: &Project, rel: &Utf8Path, err: &StoreError) {
    tracing::warn!(project = %project.name, path = %rel, error = %err, "store call failed");
}

/// An authority declaration Lore refused to honor. A warning, not a debug
/// line: the document claims to be canon, Lore has decided it is not, and the
/// author is the only one who can reconcile that.
fn log_violation(project: &Project, rel: &Utf8Path, demotion: Demotion) {
    tracing::warn!(
        project = %project.name,
        path = %rel,
        reason = demotion.note(),
        "authority declaration not honored"
    );
}

/// The indexer task: drain the coalescing queue until cancelled.
///
/// One pass at a time, deliberately. Concurrent passes would contend on the
/// single store lock anyway, and serial passes keep the log readable and the
/// generation counter meaningful.
pub async fn run(ctx: IndexContext, queue: IndexQueue) {
    while let Some((project_id, work)) = queue.next(&ctx.cancel).await {
        let projects = match ctx.store.with(|store| store.list_projects()).await {
            Ok(Ok(projects)) => projects,
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "listing projects failed; dropping index work");
                continue;
            }
            Err(err) => {
                tracing::warn!(error = %err, "store task failed; dropping index work");
                continue;
            }
        };
        let Some(project) = projects.into_iter().find(|p| p.id == project_id) else {
            tracing::debug!(
                project_id,
                "index work for an unregistered project; dropped"
            );
            continue;
        };

        let ctx_for_pass = ctx.clone();
        let result =
            tokio::task::spawn_blocking(move || run_work(&ctx_for_pass, &project, work)).await;
        if let Err(err) = result {
            tracing::error!(error = %err, "index pass panicked");
        }
    }
    tracing::debug!("indexer stopped");
}

fn run_work(ctx: &IndexContext, project: &Project, work: ProjectWork) -> PassSummary {
    if work.full {
        full_scan_with(
            ctx,
            project,
            ApplyOptions {
                allow_mass_delete: work.allow_mass_delete,
            },
        )
    } else {
        index_paths(ctx, project, &work.paths)
    }
}
