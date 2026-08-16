//! Indexing: disk → chunks → store.
//!
//! Two entry points, one file-level routine:
//!
//! - [`full_scan`] walks a project and reconciles the store with disk
//!   (index changed files, prune vanished ones).
//! - [`index_paths`] does the same for a specific set of paths, which is what
//!   a watcher batch turns into.
//!
//! Both are **synchronous** and take a [`StoreHandle`], because the store is
//! synchronous and file IO is blocking. The async layer ([`run`]) is a thin
//! pump that hands whole passes to `spawn_blocking`. That split is what makes
//! the interesting behaviour testable without a runtime, a watcher, or any
//! timing at all.
//!
//! Change detection is content hashing (blake3), not mtime: mtime lies across
//! branch switches, restores from backup and Unity's asset pipeline, and a
//! false "unchanged" is a permanently stale index. A file whose hash is
//! unchanged costs one read plus one hash and touches no store state — in
//! particular it does not rewrite `indexed_at`, so re-scans are genuinely
//! free rather than merely fast.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::authority::{DecisionIndex, Decisions, Demotion, is_decision_source, is_ledger_path};
use crate::chunk::{FileChunks, chunk_file};
use crate::repo_config::{Profile, RepoAuthority};
use crate::store::{FileWrite, Project, Recompute, StoreError};

use super::queue::{IndexQueue, ProjectWork};
use super::store_handle::StoreHandle;
use super::walk;

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
}

impl IndexContext {
    pub fn new(store: StoreHandle, data_dir: Utf8PathBuf, cancel: CancellationToken) -> Self {
        Self {
            store,
            data_dir,
            cancel,
            embed_notify: Arc::new(Notify::new()),
        }
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
}

impl PassSummary {
    fn record(&mut self, write: FileWrite) {
        self.indexed += 1;
        self.chunks_inserted += write.inserted;
        self.chunks_kept += write.kept;
        self.chunks_deleted += write.deleted;
    }
}

/// Full reconciliation of one project against disk.
///
/// Prune-by-diff (`list_files` minus what the walk saw) rather than
/// prune-by-generation: it needs no extra column, and it is correct even if
/// the daemon was killed mid-pass — the next pass sees the same difference.
pub fn full_scan(ctx: &IndexContext, project: &Project) -> PassSummary {
    let started = Instant::now();
    let mut summary = PassSummary::default();

    // Before the walk, not after: the file this may create is what the walk
    // immediately obeys. Never fatal — a root that cannot be written to falls
    // back to the walker's built-in exclusions instead of aborting the scan.
    super::ignorefile::ensure(&project.root);

    let profile = refresh_profile(ctx, project, &mut summary);

    let files = walk::walk_files(
        &project.root,
        &project.root,
        None,
        &ctx.data_dir,
        Some(&ctx.cancel),
    );
    let seen: BTreeSet<Utf8PathBuf> = files.into_iter().collect();
    summary.seen = seen.len();
    // A cancelled walk is partial: pruning against it would treat every
    // unwalked file as deleted. Record the cancellation before anything
    // consults `seen`.
    summary.cancelled = ctx.cancel.is_cancelled();

    for rel in &seen {
        if summary.cancelled || ctx.cancel.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        index_one(ctx, project, rel, profile, &mut summary);
    }

    if !summary.cancelled {
        prune_missing(ctx, project, &seen, &mut summary);
        // Unconditional on a full scan: this is also the migration/startup
        // backfill path, where the stored effective tiers may be the V2
        // migration's declared-tier approximation and the active-decision set
        // may never have been parsed at all.
        refresh_authority(ctx, project, profile, true, &mut summary);
    }

    finish(ctx, project, "full_scan", started, &summary);
    summary
}

/// Index exactly these project-relative paths.
///
/// A queued path may be a file, a directory (a whole tree was dropped in), or
/// gone entirely — the watcher cannot reliably tell, and by the time the
/// batch is processed the truth may have changed again. So the disk is
/// consulted here, once, and each case handled:
///
/// - **gone**: remove it, and prune anything indexed underneath it (that is
///   how a deleted *directory* prunes its files: the removal event names only
///   the directory).
/// - **directory**: walk it and index what it contains.
/// - **file**: confirm it still passes the ignore rules by listing its parent
///   directory through the same walker the full scan uses. A file that no
///   longer passes (a new `.gitignore` rule, a rename into an excluded tree)
///   is removed rather than left behind as an orphan.
pub fn index_paths(
    ctx: &IndexContext,
    project: &Project,
    requested: &BTreeSet<Utf8PathBuf>,
) -> PassSummary {
    let started = Instant::now();
    let mut summary = PassSummary::default();
    let profile = refresh_profile(ctx, project, &mut summary);

    let mut missing: Vec<&Utf8Path> = Vec::new();
    let mut to_index: BTreeSet<Utf8PathBuf> = BTreeSet::new();
    // Parent directory -> the files under it this batch mentions.
    let mut by_parent: BTreeMap<Utf8PathBuf, Vec<&Utf8Path>> = BTreeMap::new();

    for rel in requested {
        if walk::is_excluded_rel(rel) {
            continue;
        }
        let abs = project.root.join(rel);
        match std::fs::metadata(&abs) {
            Err(_) => missing.push(rel),
            Ok(meta) if meta.is_dir() => {
                to_index.extend(walk::walk_files(
                    &project.root,
                    &abs,
                    None,
                    &ctx.data_dir,
                    Some(&ctx.cancel),
                ));
            }
            Ok(_) => {
                let parent = rel.parent().unwrap_or(Utf8Path::new("")).to_owned();
                by_parent.entry(parent).or_default().push(rel);
            }
        }
    }

    let mut to_remove: BTreeSet<Utf8PathBuf> = missing.iter().map(|p| (*p).to_owned()).collect();

    for (parent, files) in by_parent {
        let start = if parent.as_str().is_empty() {
            project.root.clone()
        } else {
            project.root.join(&parent)
        };
        let allowed: BTreeSet<Utf8PathBuf> = walk::walk_files(
            &project.root,
            &start,
            Some(1),
            &ctx.data_dir,
            Some(&ctx.cancel),
        )
        .into_iter()
        .collect();
        for rel in files {
            if allowed.contains(rel) {
                to_index.insert(rel.to_owned());
            } else {
                to_remove.insert(rel.to_owned());
            }
        }
    }

    summary.seen = to_index.len() + to_remove.len();
    // A cancelled walk above may have classified a still-allowed file as
    // removable (its parent listing came back partial); removing on partial
    // evidence deletes real index rows, so stop before the removals run.
    summary.cancelled = ctx.cancel.is_cancelled();

    for rel in &to_index {
        if summary.cancelled || ctx.cancel.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        index_one(ctx, project, rel, profile, &mut summary);
    }

    if !summary.cancelled {
        remove_paths_and_subtrees(ctx, project, &to_remove, &mut summary);
        // Only when this batch touched a decision source — a mono ledger or a
        // per-file record (D-0013). Every *other* file already got its
        // effective tier stamped on write; re-deriving the whole project
        // because one source file changed would be pure waste.
        let decisions_touched = to_index
            .iter()
            .chain(to_remove.iter())
            .any(|rel| is_decision_source(rel));
        // A profile change invalidates every tier in the project, not just
        // this batch's. The recompute below fixes the tiers; the *chunks* of
        // files outside this batch are still stale, which is why the watcher
        // answers a `.lore.toml` edit with a full scan rather than a batch.
        // This branch is the safety net for any other route into an
        // incremental pass with a moved profile.
        if decisions_touched || summary.profile_changed {
            refresh_authority(ctx, project, profile, summary.profile_changed, &mut summary);
        }
    }

    finish(ctx, project, "incremental", started, &summary);
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
            "decision record excluded from the active set"
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

/// Read, hash, chunk and store one file. All error paths are logged and
/// counted, never fatal: one unreadable file must not abort a scan.
fn index_one(
    ctx: &IndexContext,
    project: &Project,
    rel: &Utf8Path,
    profile: Option<Profile>,
    summary: &mut PassSummary,
) {
    let abs = project.root.join(rel);
    let content = match std::fs::read(&abs) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Raced with a delete between walk and read.
            remove_one(ctx, project, rel, summary);
            return;
        }
        Err(err) => {
            tracing::warn!(project = %project.name, path = %rel, error = %err, "unreadable file");
            summary.errors += 1;
            return;
        }
    };

    // The format version rides in the stored hash so a chunking-policy bump
    // invalidates the short-circuit below and unchanged bytes re-chunk. The
    // active profile rides along for the same reason and by the same mechanism
    // (D-0012): the same Markdown bytes chunk to different *metadata* under a
    // different profile, so adding, changing or removing `.lore.toml` has to
    // invalidate the hash or the old vault fields would survive forever.
    //
    // Markdown only. Nothing about a code chunk depends on the profile, so
    // code keeps its hash — and therefore its vectors — across a profile flip.
    let profile_tag = match profile {
        Some(profile) if crate::chunk::is_markdown(rel) => profile.chunk_tag(),
        _ => "",
    };
    let hash = format!(
        "v{}{profile_tag}-{}",
        crate::chunk::CHUNK_FORMAT_VERSION,
        blake3::hash(&content).to_hex()
    );
    let known = ctx.store.blocking(|store| store.file_hash(project.id, rel));
    match known {
        Ok(Some(previous)) if previous == hash => {
            summary.unchanged += 1;
            return;
        }
        Ok(_) => {}
        Err(err) => {
            log_store_error(project, rel, &err);
            summary.errors += 1;
            return;
        }
    }

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
        Ok(true) => {
            tracing::debug!(project = %project.name, path = %rel, "removed file from index");
            summary.removed += 1;
        }
        Ok(false) => {}
        Err(err) => {
            log_store_error(project, rel, &err);
            summary.errors += 1;
        }
    }
}

/// Remove each path, plus everything indexed beneath it (directory deletes).
fn remove_paths_and_subtrees(
    ctx: &IndexContext,
    project: &Project,
    paths: &BTreeSet<Utf8PathBuf>,
    summary: &mut PassSummary,
) {
    if paths.is_empty() {
        return;
    }
    // Removals are idempotent, so stopping mid-list is safe: the next pass
    // sees the same difference and finishes the job.
    for rel in paths {
        if ctx.cancel.is_cancelled() {
            summary.cancelled = true;
            return;
        }
        remove_one(ctx, project, rel, summary);
    }

    let indexed = match ctx.store.blocking(|store| store.list_files(project.id)) {
        Ok(indexed) => indexed,
        Err(err) => {
            tracing::warn!(project = %project.name, error = %err, "listing files for prune failed");
            summary.errors += 1;
            return;
        }
    };
    let prefixes: Vec<String> = paths.iter().map(|p| format!("{p}/")).collect();
    for record in indexed {
        if ctx.cancel.is_cancelled() {
            summary.cancelled = true;
            return;
        }
        if prefixes.iter().any(|p| record.path.as_str().starts_with(p)) {
            remove_one(ctx, project, &record.path, summary);
        }
    }
}

/// Drop index entries for files that are no longer on disk (or no longer
/// pass the ignore rules — both present as "the walk did not see it").
fn prune_missing(
    ctx: &IndexContext,
    project: &Project,
    seen: &BTreeSet<Utf8PathBuf>,
    summary: &mut PassSummary,
) {
    let indexed = match ctx.store.blocking(|store| store.list_files(project.id)) {
        Ok(indexed) => indexed,
        Err(err) => {
            tracing::warn!(project = %project.name, error = %err, "listing files for prune failed");
            summary.errors += 1;
            return;
        }
    };
    for record in indexed {
        if ctx.cancel.is_cancelled() {
            summary.cancelled = true;
            return;
        }
        if !seen.contains(&record.path) {
            remove_one(ctx, project, &record.path, summary);
        }
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
    summary: &PassSummary,
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
        full_scan(ctx, project)
    } else {
        index_paths(ctx, project, &work.paths)
    }
}
