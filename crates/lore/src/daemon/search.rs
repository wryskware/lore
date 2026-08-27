//! `POST /v1/search` — hybrid query execution (3.1 §Ranking).
//!
//! Two arms run against the same pre-filtered row set — BM25 over FTS5 and
//! cosine over stored vectors — and are fused with Reciprocal Rank Fusion,
//! after which a vault-authority multiplier is applied. The wire filters are
//! pushed down into SQL for *both* arms, so ranking never sees a row the
//! caller excluded.
//!
//! # Why RRF and not score blending
//!
//! The two arms produce incomparable numbers: BM25 is an unbounded corpus
//! statistic (negated here so higher is better), cosine is bounded in
//! `[-1, 1]`. Normalizing them into a shared range means inventing a mapping
//! that changes meaning with every corpus. RRF ignores magnitudes entirely and
//! fuses *ranks*, which is why it is the standard answer and what 3.1 asks for.
//!
//! # Candidate depth is proved, not guessed
//!
//! Fusing ranks means a chunk both arms merely *like* can outscore a chunk one
//! arm loves: agreement at rank 51 in both lists (`2/111`) beats a rank-1
//! singleton (`1/61`). A fixed 50-per-arm pull therefore does not truncate the
//! tail of the ranking, it silently omits results that belong at the top of
//! it. Collapse makes the same pull underfill pages: one oversized symbol's
//! windows can occupy the whole pool and leave a page of one.
//!
//! So acquisition loops. Each round fetches both arms at the current depth and
//! re-runs fusion, authority and collapse *from scratch*, then asks whether
//! anything outside the page could still get in (see [`page_is_final`]). It
//! stops when the answer is provably no, when both arms are exhausted, or at
//! [`MAX_CANDIDATES`]. Depth itself is nearly free in this store — the vector
//! arm is an O(n) scan whose cost does not depend on `k`, and FTS5 scores
//! every matching posting regardless of `LIMIT` — so what a round costs is
//! one more scan, and the ladder is kept short for that reason.
//!
//! On the ordinary corpus this settles in one round: both arms run out of
//! matching rows inside the first 50, which makes the ranking exact rather
//! than merely unbeatable. Read [`MAX_CANDIDATES`] for what happens when they
//! do not, and for the one thing this still does not promise.
//!
//! # Degradation is a first-class path
//!
//! `lexical_only` means exactly "vectors did not participate in *this*
//! response" — no endpoint, an unhealthy one, a query that could not be
//! embedded in time, or a corpus with no vectors yet in the filtered scope.
//! Every one of those returns results; none of them returns an error (D-0007).

use std::collections::{HashMap, HashSet};

use lore_core::{SearchRequest, SearchResponse, SearchResult};

use crate::authority::Authority;
use crate::store::{Project, ProjectId, SearchFilter, SearchHit, StatusFilter, Store};
use crate::types::{Chunk, ChunkKind, DesignStatus, SourceKind};

/// Results returned when the caller does not ask for a specific number.
///
/// Was 20 through RCB-W round 1; the 2026-08-20 transcript debug showed no
/// agent ever passed `limit` and none consumed past rank 3, while every
/// returned token is re-read on each later turn of an agent loop. Ten keeps
/// real breadth (the MCP rendering carries excerpts only for the top ranks
/// anyway) at roughly half the payload; callers who want more still get it,
/// clamped at [`MAX_LIMIT`].
pub const DEFAULT_LIMIT: u32 = 10;

/// Hard ceiling; `search` is meant to stay token-lean (`expand` exists for
/// depth, 3.1).
pub const MAX_LIMIT: u32 = 100;

/// Excerpts are capped so a handful of results cannot blow an agent's
/// context window on one tool call.
pub const EXCERPT_MAX_CHARS: usize = 2000;

/// Depth of the *first* lexical fetch. Deeper than the default page on
/// purpose: a chunk the vector arm loves is worth surfacing even if BM25 put
/// it at 40, and that only works if BM25 was asked for 50. Later rounds grow
/// from here — this is a floor, not a ceiling.
pub const LEXICAL_CANDIDATES: usize = 50;

/// Depth of the first vector fetch. See [`LEXICAL_CANDIDATES`].
pub const VECTOR_CANDIDATES: usize = 50;

/// Per-arm depth multiplier between acquisition rounds.
///
/// A *round*, not a row, is the unit of cost here: the vector arm is a full
/// scan whose price does not depend on the depth requested, so four rounds of
/// 50 cost roughly four times one round of 200. The ladder is therefore short
/// and steep — 50 → 200 → 800 → [`MAX_CANDIDATES`], four rounds at worst —
/// rather than the gentler doubling that would pay for six scans to reach the
/// same place.
pub const CANDIDATE_GROWTH: usize = 4;

/// Hard ceiling on per-arm candidate depth, and the one approximation left in
/// ranking.
///
/// **This cap is load-bearing, not a safety valve.** Proving a page final
/// means bounding the candidate *just* below the cutoff, and with RRF damping
/// `K` the gap between consecutive ranks near rank `r` is about
/// `1/(K + r)²`, so a real proof for a 20-result page over a dense corpus
/// needs depth on the order of `(K + 20)² = 6400` — measured, not estimated.
/// Any query whose arms are both still open at 1000 therefore stops here.
///
/// What the loop buys is nonetheless real, and is what the two reported
/// failures asked for: cross-arm agreement anywhere in the first 1000 now
/// wins the page it deserves (rank 51 was the reported case), and collapse can
/// no longer strand a page at one result while eligible chunks sit below the
/// pool. What remains approximate is only the tail beyond 1000, and hitting
/// the cap is logged so it is visible rather than assumed.
pub const MAX_CANDIDATES: usize = 1000;

/// RRF damping constant. 60 is the value from the original Cormack et al.
/// result and the de-facto default everywhere since; it makes the top of each
/// list matter without letting rank 1 dominate rank 3.
pub const RRF_K: f64 = 60.0;

// Vault-authority multipliers, applied *after* fusion (3.1 step 2), keyed by
// the **effective** tier (`crate::authority`) rather than the declared one.
//
// The **ordering** — decided > leaning > exploration/unclassified >
// deprecated — is the canon requirement (3.1, and `types::authority_tier`).
// The numbers are tuning, and they must be read in *rank* space, not score
// space: RRF scores are a pure function of rank and `RRF_K`, compressed to
// within ~15% of each other over the whole first page, so a multiplier `m`
// on a fused score moves a rank-`r` hit to the equivalent of rank
// `(RRF_K + r)/m − RRF_K`. The original values (1.15 / 1.05 / 0.7) looked
// gentle but shifted hits by 10–30 ranks — a demoted top lexical match for a
// *navigational* query landed below two dozen mediocre hits, and decided docs
// leapfrogged the query's actual target from rank 20. The values below are
// derived from the shift we actually want at the top of the list: decided
// reaches ~5 ranks (2.1's "comparable similarity" includes the few-rank BM25
// spread a pack of near-identical docs produces — the laundering tests pin
// this), leaning ~2, deprecated loses ~3–4. Authority breaks ties and lifts
// a near-miss; a decisively better match wins regardless of tier.
//
// Because RRF scores carry no corpus statistics, these rank shifts are the
// same for every repo — the multipliers transfer without per-corpus retuning.
/// Effective tier 3 — validated `decided`, and the ledger itself.
pub const AUTHORITY_DECIDED: f64 = 1.07;
/// Effective tier 2 — `leaning`.
pub const AUTHORITY_LEANING: f64 = 1.02;
/// Effective tier 1 — `exploration`, unclassified, non-vault (code) chunks,
/// `7_Research`, and any declaration that failed validation.
pub const AUTHORITY_NEUTRAL: f64 = 1.0;
/// Effective tier 0 — `deprecated` and `99_Scratch`: still searchable,
/// deliberately demoted.
pub const AUTHORITY_DEPRECATED: f64 = 0.95;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error(
        "unknown project `{0}`; name a registered project (see `lore status`), or register one with `lore add <path>`"
    )]
    UnknownProject(String),
    #[error(
        "unknown project key `{0}`; name a registered project (see `lore status`), or register one with `lore add <path>`"
    )]
    UnknownProjectKey(String),
    #[error(
        "unknown design status `{0}` (expected exploration, leaning, decided, deprecated or unclassified)"
    )]
    UnknownStatus(String),
    #[error("unknown source kind `{0}` (expected repo or session)")]
    UnknownSource(String),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

/// Run a search against the store.
///
/// Takes `&mut Store` (not a handle) so the caller controls the lock scope:
/// project resolution, both ranking arms and name lookup all happen inside
/// one acquisition, which also makes the result set internally consistent.
///
/// `query_vector` is the caller's business precisely because embedding it is
/// network I/O: it must happen *before* the store lock is taken, never while
/// holding it. `None` means "run lexical-only", and is the normal path
/// whenever the endpoint is absent, unhealthy or slow (D-0007).
pub fn execute(
    store: &mut Store,
    request: &SearchRequest,
    query_vector: Option<&[f32]>,
) -> Result<SearchResponse, SearchError> {
    let projects = store.list_projects()?;
    let sources: HashMap<ProjectId, Project> = projects.iter().map(|p| (p.id, p.clone())).collect();

    let mut filter = SearchFilter::default();
    // Key first, exactly as `expand` resolves it: a client replaying a stale
    // display name alongside a fresh key must not silently query the wrong
    // source. The two resolvers stay separate so neither accepts the other's
    // vocabulary (see `daemon::resolve_project_key`).
    if let Some(key) = &request.project_key {
        let project = super::resolve_project_key(&projects, key)
            .ok_or_else(|| SearchError::UnknownProjectKey(key.clone()))?;
        filter.project = Some(project.id);
    } else if let Some(name) = &request.project {
        let project = super::resolve_project(&projects, name)
            .ok_or_else(|| SearchError::UnknownProject(name.clone()))?;
        filter.project = Some(project.id);
    }
    filter.path_prefix = request
        .path_prefix
        .as_ref()
        .map(|prefix| prefix.replace('\\', "/"));
    filter.language = request.language.as_ref().map(|l| l.to_ascii_lowercase());
    if !request.status.is_empty() {
        let statuses = request
            .status
            .iter()
            .map(|s| parse_status(s).ok_or_else(|| SearchError::UnknownStatus(s.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        filter.statuses = Some(statuses);
    }
    if let Some(sources) = &request.sources {
        filter.source_kinds = Some(
            sources
                .iter()
                .map(|s| SourceKind::parse(s).ok_or_else(|| SearchError::UnknownSource(s.clone())))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }

    let limit = request.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT) as usize;

    // Which corpora let authority touch *ordering* (D-0012). `annotate`
    // computes and reports every tier and note but leaves the ranking to pure
    // retrieval, so the multiplier is keyed on the source, not applied
    // globally — one repo opting into `rank` must not re-weight another's
    // results in the same response.
    let ranked: HashSet<ProjectId> = sources
        .values()
        .filter(|source| source.authority.ranks())
        .map(|source| source.id)
        .collect();

    // Candidate acquisition is a loop, not one fixed pull (see the module
    // header). Both arms are always asked for the same depth, because the stop
    // condition is stated in terms of a single `depth`. The floor keeps a
    // large page from being served out of a shallower pool than itself
    // (`limit = 100` must not top out at 50); the cap governs the other end.
    let mut depth = limit.clamp(LEXICAL_CANDIDATES.max(VECTOR_CANDIDATES), MAX_CANDIDATES);

    let (fusion, lexical_only) = loop {
        let lexical = store.lexical_search(&request.query, &filter, depth)?;

        // A vector-arm failure degrades this request instead of failing it:
        // the usual cause is a fingerprint that moved under a live query
        // (dimension mismatch), which the worker is already fixing by
        // re-embedding.
        let vector = match query_vector {
            Some(query_vector) => match store.vector_search(query_vector, &filter, depth) {
                Ok(hits) => hits,
                Err(err) => {
                    tracing::debug!(error = %err, "vector arm failed; falling back to lexical-only");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        // An arm that returned fewer rows than it was asked for has nothing
        // left to give, and neither has one that never ran (no query vector,
        // a vector error, a query that sanitized to nothing). "Open" arms are
        // the only ones a deeper round could hear from.
        let open = u32::from(lexical.len() >= depth) | (u32::from(vector.len() >= depth) << 1);

        // Honest by construction: an embedded query over a corpus with no
        // vectors in the filtered scope contributed nothing, and says so.
        let lexical_only = vector.is_empty();
        let fusion = fuse_detailed(vec![lexical, vector], limit, &ranked);

        if open == 0 {
            // Both arms are fully enumerated: this ranking is exact, not an
            // approximation. Short pages here are genuinely short.
            break (fusion, lexical_only);
        }
        if page_is_final(&fusion, depth, open) {
            break (fusion, lexical_only);
        }
        if depth >= MAX_CANDIDATES {
            tracing::debug!(
                depth,
                limit,
                query = %request.query,
                "candidate cap reached; this page is the best of the fetched pool, not proven best"
            );
            break (fusion, lexical_only);
        }
        // `.max(depth + 1)` makes growth strictly monotonic, so the cap above
        // is always reached and the loop always terminates.
        depth = depth
            .saturating_mul(CANDIDATE_GROWTH)
            .max(depth + 1)
            .min(MAX_CANDIDATES);
    };

    let results = fusion
        .page
        .into_iter()
        .map(|hit| to_result(&sources, hit))
        .collect();
    Ok(SearchResponse {
        results,
        lexical_only,
    })
}

/// Can anything we have not placed on the page still reach it?
///
/// A candidate absent from an arm that returned a *full* `depth` rows sits at
/// rank ≥ `depth + 1` there, so that arm can contribute at most
/// `1 / (RRF_K + depth + 1)` to it. Bounding every reachable candidate by
///
/// ```text
/// max_score(c) = authority_max(c) · (rrf_seen(c) + missing_open_arms(c) · 1/(RRF_K + depth + 1))
/// ```
///
/// and comparing against the page's own minimum turns "50 per arm looked like
/// enough" into a proof. A candidate no arm has shown us yet is the same
/// formula with `rrf_seen = 0` and the most generous authority there is.
///
/// The comparison is `>=`, not `>`: equal scores break on chunk id, so a
/// candidate that merely *ties* the cutoff can still take the slot.
fn page_is_final(fusion: &Fusion, depth: usize, open: u32) -> bool {
    // A short page is never final while any arm might still have rows: the
    // missing results may be exactly the ones collapse took out of it.
    let Some(cutoff) = fusion.cutoff else {
        return false;
    };
    let arm_ceiling = 1.0 / (RRF_K + depth as f64 + 1.0);

    // The unseen case: a chunk in no list yet could enter every open arm at
    // once, at the best rank still available in each, and be `decided`.
    if AUTHORITY_DECIDED * f64::from(open.count_ones()) * arm_ceiling >= cutoff {
        return false;
    }
    fusion.outside.iter().all(|c| {
        let missing = f64::from((open & !c.seen).count_ones());
        c.authority * (c.rrf + missing * arm_ceiling) < cutoff
    })
}

/// One chunk's accumulating fusion state.
struct Candidate {
    project: ProjectId,
    chunk: Chunk,
    authority: Authority,
    /// Σ over the lists this chunk appears in.
    rrf: f64,
    /// Bit `i` set = this chunk appeared in `lists[i]`. Which arms are still
    /// *missing* a candidate is what bounds how much better it could get.
    seen: u32,
}

/// One fused page, plus what acquisition needs to know about everything that
/// did not make it onto the page.
struct Fusion {
    page: Vec<SearchHit>,
    /// Score of the last result on a *full* page — the bar a newcomer has to
    /// clear. `None` when the page is short, which is never final while any
    /// arm still has rows.
    cutoff: Option<f64>,
    outside: Vec<Outside>,
}

/// A candidate that could still displace something, reduced to the three
/// numbers that bound it. Deliberately holds no chunk: acquisition may build
/// this for a thousand rows per round.
struct Outside {
    /// Σ 1/(RRF_K + rank) over the arms that have already shown it.
    rrf: f64,
    authority: f64,
    /// Same bitmask as [`Candidate::seen`].
    seen: u32,
}

/// Reciprocal Rank Fusion + vault authority + window collapse.
///
/// ```text
/// score(c) = authority(c) · Σ_lists 1 / (RRF_K + rank_list(c))     rank 1-based
/// ```
///
/// Applied uniformly, including when only one list is non-empty: a client
/// cannot tell which arms ran, so the score must not change meaning with the
/// endpoint's health, and 3.1's authority modifier is a property of ranking
/// rather than of hybridity.
///
/// Production goes through [`fuse_detailed`], which is this plus the numbers
/// acquisition needs; this shape is what the ranking tests assert against.
#[cfg(test)]
fn fuse(lists: Vec<Vec<SearchHit>>, limit: usize) -> Vec<SearchHit> {
    // Every project in the ranking tests is an authority-ranking one, which
    // is what those tests were written against.
    let ranked: HashSet<ProjectId> = lists.iter().flatten().map(|hit| hit.project).collect();
    fuse_detailed(lists, limit, &ranked).page
}

/// [`fuse`], keeping the bookkeeping acquisition needs to decide whether to
/// go deeper. Recomputed from scratch every round — nothing here is patched
/// incrementally, so a deeper fetch cannot leave a stale ordering behind.
fn fuse_detailed(lists: Vec<Vec<SearchHit>>, limit: usize, ranked: &HashSet<ProjectId>) -> Fusion {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen: HashMap<(ProjectId, String), usize> = HashMap::new();

    for (list_index, list) in lists.into_iter().enumerate() {
        let arm = 1u32 << list_index;
        for (rank, hit) in list.into_iter().enumerate() {
            let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
            let key = (hit.project, hit.chunk.id.0.clone());
            match seen.get(&key) {
                Some(&index) => {
                    candidates[index].rrf += contribution;
                    candidates[index].seen |= arm;
                }
                None => {
                    seen.insert(key, candidates.len());
                    candidates.push(Candidate {
                        project: hit.project,
                        chunk: hit.chunk,
                        authority: hit.authority,
                        rrf: contribution,
                        seen: arm,
                    });
                }
            }
        }
    }

    // (score, authority, candidate) — authority is kept because the bound on
    // an off-page candidate needs it separately from the product.
    let mut scored: Vec<(f64, f64, Candidate)> = candidates
        .into_iter()
        .map(|candidate| {
            let authority = if ranked.contains(&candidate.project) {
                authority_weight(candidate.authority.tier)
            } else {
                AUTHORITY_NEUTRAL
            };
            (candidate.rrf * authority, authority, candidate)
        })
        .collect();
    // Chunk id breaks ties so identical scores rank identically across runs;
    // an unstable top-N would make every snapshot and agent flaky.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.2.chunk.id.0.cmp(&b.2.chunk.id.0))
    });

    // Window collapse. The chunker splits an oversized symbol or section into
    // overlapping windows, so a query matching the overlap hits several of
    // them; they are one place in one file, the best-scoring one represents
    // them, and the rest are noise the caller would have to deduplicate.
    //
    // The gate is *positive membership of one generated family*, never the
    // shape of an anchor. Everything without a family stays a distinct result
    // even when anchors collide: two C# overloads share `Parser.Parse`, two
    // sibling `## Notes` sections share their heading path, and two
    // independently windowed overloads both spell `#w0`/`#w1` while being
    // different code. Rows written before `CHUNK_FORMAT_VERSION` 4 carry no
    // family and therefore never collapse - the safe direction, showing a
    // duplicate window rather than hiding a result, until the re-chunk pass
    // reaches them.
    let mut kept: HashSet<(ProjectId, camino::Utf8PathBuf, u32)> = HashSet::new();
    let mut out = Vec::with_capacity(limit.min(scored.len()));
    let mut outside: Vec<Outside> = Vec::new();
    let mut cutoff = f64::INFINITY;
    for (score, authority, candidate) in scored {
        let family = candidate.chunk.kind.window_family().map(|window| {
            (
                candidate.project,
                candidate.chunk.path.clone(),
                window.family,
            )
        });
        if out.len() < limit {
            if let Some(key) = family
                && !kept.insert(key)
            {
                continue;
            }
            cutoff = score;
            out.push(SearchHit {
                project: candidate.project,
                chunk: candidate.chunk,
                authority: candidate.authority,
                score: score as f32,
            });
            continue;
        }
        // Everything below the page is a reason to fetch deeper — with one
        // exception. A window whose family already holds a slot cannot add a
        // result no matter how much its score grows: at best it overtakes its
        // own family's representative, which swaps *which window* of one span
        // is shown and leaves the page the same size, the same set of places,
        // and its minimum no lower. (Which window represents a split span is
        // therefore decided at the depth reached, not proven — a deliberate,
        // bounded approximation.) Members of a family that is entirely off the
        // page get no such exemption: that family can still climb into it, so
        // every one of its members is bounded here.
        if family.is_some_and(|key| kept.contains(&key)) {
            continue;
        }
        outside.push(Outside {
            rrf: candidate.rrf,
            authority,
            seen: candidate.seen,
        });
    }
    Fusion {
        cutoff: (out.len() >= limit).then_some(cutoff),
        page: out,
        outside,
    }
}

/// The 3.1 vault-status modifier, over the **effective** tier.
///
/// It used to read the declared status directly, with one "refinement": an
/// unclassified chunk that cited *any* `D-NNNN` was weighted as `leaning`.
/// That was the laundering vector the review found (S1#2) — scratch notes
/// quote decision numbers constantly — and it is gone. Citations are still
/// reported on every result; they simply buy nothing. Everything the tier
/// encodes (ledger validation, path ceilings, the ledger pin, the session cap)
/// happened at index time in [`crate::authority`], so ranking stays a table
/// lookup.
fn authority_weight(effective_tier: u8) -> f64 {
    match effective_tier {
        3 => AUTHORITY_DECIDED,
        2 => AUTHORITY_LEANING,
        0 => AUTHORITY_DEPRECATED,
        _ => AUTHORITY_NEUTRAL,
    }
}

fn parse_status(status: &str) -> Option<StatusFilter> {
    Some(match status {
        "unclassified" => StatusFilter::Unclassified,
        "exploration" => StatusFilter::Status(DesignStatus::Exploration),
        "leaning" => StatusFilter::Status(DesignStatus::Leaning),
        "decided" => StatusFilter::Status(DesignStatus::Decided),
        "deprecated" => StatusFilter::Status(DesignStatus::Deprecated),
        _ => return None,
    })
}

/// One store hit → one wire result.
///
/// `pub(crate)` for [`super::follow`], which turns a resolved definition into a
/// result the bundle assembler can verify. Reusing this is not tidiness:
/// `bundle::verify` reads `excerpt` and `excerpt_truncated` with exactly the
/// meanings search gives them, and a second constructor would eventually
/// disagree about one of them.
pub(crate) fn to_result(sources: &HashMap<ProjectId, Project>, hit: SearchHit) -> SearchResult {
    let source = sources.get(&hit.project);
    let chunk = hit.chunk;
    let (symbol_path, heading_path) = match &chunk.kind {
        ChunkKind::Code { symbol_path, .. } => (Some(symbol_path.clone()), None),
        ChunkKind::Section { heading_path, .. } => (None, Some(heading_path.clone())),
        ChunkKind::Window { .. } => (None, None),
    };
    let (design_status, decision_refs) = match &chunk.vault {
        Some(vault) => (
            vault.design_status.map(status_label),
            merge_refs(&vault.decision_refs, &vault.body_decision_refs),
        ),
        None => (None, Vec::new()),
    };
    let (excerpt, excerpt_truncated) = excerpt(&chunk.text);
    // A neutral repository is not "neutral authority", it is *unjudged*
    // (D-0012). Reporting a tier for a repo that never opted in would put a
    // verdict on the wire that nothing computed, so the fields are simply
    // absent. `design_status` and `decision_refs` are already empty for such a
    // repo — the chunker never parsed them.
    let annotates = source.is_some_and(|source| source.authority.annotates());

    SearchResult {
        chunk_id: chunk.id.0,
        project: source
            .map(|source| source.name.clone())
            .unwrap_or_else(|| hit.project.to_string()),
        project_key: source.map(|source| source.key.clone()).unwrap_or_default(),
        path: chunk.path.into_string(),
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        language: chunk.language,
        symbol_path,
        heading_path,
        design_status: design_status.map(str::to_string),
        effective_authority: annotates.then(|| hit.authority.label().to_string()),
        authority_note: annotates
            .then(|| {
                hit.authority
                    .demotion
                    .map(|demotion| demotion.note().to_string())
            })
            .flatten(),
        decision_refs,
        score: f64::from(hit.score),
        excerpt,
        excerpt_truncated,
    }
}

/// Frontmatter refs first (they describe the whole document), then refs found
/// in this chunk's own body; duplicates dropped, order preserved.
fn merge_refs(frontmatter: &[String], body: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(frontmatter.len() + body.len());
    for reference in frontmatter.iter().chain(body) {
        if !out.iter().any(|existing| existing == reference) {
            out.push(reference.clone());
        }
    }
    out
}

/// Truncate on a character boundary — chunk text is arbitrary UTF-8 and a
/// byte-sliced excerpt would panic on the first non-ASCII file.
fn excerpt(text: &str) -> (String, bool) {
    match text.char_indices().nth(EXCERPT_MAX_CHARS) {
        Some((byte, _)) => (text[..byte].to_string(), true),
        None => (text.to_string(), false),
    }
}

fn status_label(status: DesignStatus) -> &'static str {
    match status {
        DesignStatus::Exploration => "exploration",
        DesignStatus::Leaning => "leaning",
        DesignStatus::Decided => "decided",
        DesignStatus::Deprecated => "deprecated",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::Demotion;
    use crate::repo_config::{Behavior, Profile, RepoAuthority};
    use crate::types::{Chunk, ChunkId, VaultMeta, WindowFamily, authority_tier};
    use camino::Utf8PathBuf;

    /// `id` is what ties a chunk together across the two ranked lists, so the
    /// helper takes it explicitly.
    fn hit(id: &str, path: &str, kind: ChunkKind, vault: Option<VaultMeta>) -> SearchHit {
        at_tier(id, path, kind, vault, authority_tier(None))
    }

    /// A hit whose *effective* tier is stated outright — which is how the
    /// store hands it over. Deriving it from the vault metadata here would
    /// re-implement the policy inside its own test; the policy itself is
    /// covered in `crate::authority`.
    fn at_tier(
        id: &str,
        path: &str,
        kind: ChunkKind,
        vault: Option<VaultMeta>,
        tier: u8,
    ) -> SearchHit {
        SearchHit {
            project: 1,
            chunk: Chunk {
                id: ChunkId(id.to_string()),
                path: Utf8PathBuf::from(path),
                kind,
                language: Some("markdown".into()),
                byte_start: 0,
                byte_end: 1,
                line_start: 1,
                line_end: 1,
                text: format!("body of {id}"),
                vault,
            },
            authority: Authority {
                tier,
                demotion: None,
            },
            // Deliberately meaningless: fusion must use ranks, not scores.
            score: 0.0,
        }
    }

    fn section(title: &str) -> ChunkKind {
        ChunkKind::Section {
            heading_path: vec![title.to_string()],
            window: None,
        }
    }

    fn vault(status: Option<DesignStatus>, refs: &[&str]) -> Option<VaultMeta> {
        Some(VaultMeta {
            design_status: status,
            decision_refs: refs.iter().map(|r| r.to_string()).collect(),
            body_decision_refs: Vec::new(),
        })
    }

    fn ids(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.chunk.id.0.as_str()).collect()
    }

    #[test]
    fn rrf_rewards_agreement_between_the_two_arms() {
        // `b` is second on both lists and beats `a`/`c`, each first on one.
        let lexical = vec![
            hit("a", "a.md", section("A"), None),
            hit("b", "b.md", section("B"), None),
        ];
        let vector = vec![
            hit("c", "c.md", section("C"), None),
            hit("b", "b.md", section("B"), None),
        ];
        let fused = fuse(vec![lexical, vector], 10);
        assert_eq!(ids(&fused), ["b", "a", "c"]);

        // The exact arithmetic, not just the order.
        let expect_b = (1.0 / (RRF_K + 2.0)) * 2.0;
        let expect_a = 1.0 / (RRF_K + 1.0);
        assert!((f64::from(fused[0].score) - expect_b).abs() < 1e-6);
        assert!((f64::from(fused[1].score) - expect_a).abs() < 1e-6);
        // `a` and `c` are both rank 1 of one list: the tie breaks on chunk id.
        assert!((f64::from(fused[2].score) - expect_a).abs() < 1e-6);
    }

    #[test]
    fn a_single_list_keeps_its_order_and_the_limit_is_respected() {
        let lexical: Vec<SearchHit> = ["a", "b", "c", "d"]
            .iter()
            .map(|id| hit(id, &format!("{id}.md"), section(id), None))
            .collect();
        let fused = fuse(vec![lexical, Vec::new()], 2);
        assert_eq!(ids(&fused), ["a", "b"]);
    }

    #[test]
    fn authority_orders_equal_ranks_by_effective_tier() {
        // Every chunk is rank 1 of its own list, so fusion scores are equal
        // and only the authority multiplier can separate them.
        let lists: Vec<Vec<SearchHit>> = [
            ("deprecated", 0),
            ("neutral", 1),
            ("decided", 3),
            ("leaning", 2),
        ]
        .into_iter()
        .map(|(id, tier)| vec![at_tier(id, &format!("{id}.md"), section(id), None, tier)])
        .collect();

        let fused = fuse(lists, 10);
        assert_eq!(ids(&fused), ["decided", "leaning", "neutral", "deprecated"]);
    }

    /// The laundering vector, at the ranking layer: a chunk that quotes
    /// decisions used to be lifted to `leaning` purely for quoting them. The
    /// multiplier now reads one number, and citations are inert.
    #[test]
    fn citations_no_longer_move_a_chunk_at_all() {
        use DesignStatus::*;
        let cited = at_tier(
            "cited",
            "design/99_Scratch/notes.md",
            section("Notes"),
            vault(None, &["D-0007", "D-0003"]),
            authority_tier(None),
        );
        let plain = at_tier(
            "plain",
            "src/lib.rs",
            section("Plain"),
            None,
            authority_tier(None),
        );
        assert_eq!(
            authority_weight(cited.authority.tier),
            authority_weight(plain.authority.tier)
        );
        // …and a *declared* status the store already refused still ranks where
        // the store put it, not where the frontmatter asked.
        let refused = at_tier(
            "refused",
            "design/x.md",
            section("X"),
            vault(Some(Decided), &[]),
            crate::authority::UNCITED_DECIDED_TIER,
        );
        assert_eq!(authority_weight(refused.authority.tier), AUTHORITY_NEUTRAL);
    }

    #[test]
    fn authority_weights_match_the_documented_tiers() {
        assert_eq!(authority_weight(3), AUTHORITY_DECIDED);
        assert_eq!(authority_weight(2), AUTHORITY_LEANING);
        assert_eq!(authority_weight(1), AUTHORITY_NEUTRAL);
        assert_eq!(authority_weight(0), AUTHORITY_DEPRECATED);
    }

    /// One window of a family, as the chunker emits it: the `#w<n>` suffix
    /// *and* the metadata that says the suffix is bookkeeping.
    fn window(family: u32, index: u32) -> Option<WindowFamily> {
        Some(WindowFamily { family, index })
    }

    #[test]
    fn overlapping_windows_of_one_span_collapse_to_the_best_one() {
        let code = |suffix: &str, window| ChunkKind::Code {
            symbol_kind: "method_declaration".into(),
            symbol_path: format!("Board.Update{suffix}"),
            window,
        };
        // Both windows of `Board.Update` match; only the better one survives.
        let lexical = vec![
            hit("w0", "Board.cs", code("#w0", window(0, 0)), None),
            hit("w1", "Board.cs", code("#w1", window(0, 1)), None),
            hit("other", "Board.cs", code("Other", None), None),
        ];
        let fused = fuse(vec![lexical, Vec::new()], 10);
        assert_eq!(ids(&fused), ["w0", "other"]);

        // Same anchor *and* same family ordinal in a different file is a
        // different place: the key is per file, and families are per file too.
        let cross_file = vec![
            hit("a", "Board.cs", code("#w0", window(0, 0)), None),
            hit("b", "Other.cs", code("#w0", window(0, 0)), None),
        ];
        assert_eq!(ids(&fuse(vec![cross_file], 10)).len(), 2);
    }

    #[test]
    fn section_windows_collapse_but_equal_anchors_do_not() {
        let windowed = |suffix: Option<&str>, window| ChunkKind::Section {
            heading_path: match suffix {
                Some(s) => vec!["Ranking".into(), s.to_string()],
                None => vec!["Ranking".into()],
            },
            window,
        };
        let hits = vec![
            hit("s0", "3.1.md", windowed(Some("#w0"), window(0, 0)), None),
            hit("s1", "3.1.md", windowed(Some("#w1"), window(0, 1)), None),
            hit("whole", "3.1.md", windowed(None, None), None),
            hit(
                "dup",
                "3.1.md",
                ChunkKind::Section {
                    heading_path: vec!["Ranking".into(), "#d1".into()],
                    window: None,
                },
                None,
            ),
        ];
        // Only s0/s1 are two views of one span. `whole` is an unsplit section
        // that merely shares the base heading path — a sibling `## Ranking`,
        // as far as ranking can tell — and `#d1` is a real duplicate span;
        // folding either would delete content the caller asked for.
        assert_eq!(ids(&fuse(vec![hits], 10)), ["s0", "whole", "dup"]);
    }

    /// A heading a human typed as `#w0` is content, not bookkeeping: without
    /// the family it is never folded away, whatever it is spelled.
    #[test]
    fn a_user_authored_window_shaped_heading_is_not_bookkeeping() {
        let heading = |title: &str| ChunkKind::Section {
            heading_path: vec!["Escapes".into(), title.into()],
            window: None,
        };
        let hits = vec![
            hit("a", "notes.md", heading("#w0"), None),
            hit("b", "notes.md", heading("#w1"), None),
        ];
        assert_eq!(ids(&fuse(vec![hits], 10)), ["a", "b"]);
    }

    /// S3#1 / S4#4 required test. Collapse folds *membership of one generated
    /// window family* and nothing else, so every shape of "same anchor,
    /// different content" has to survive it.
    ///
    /// The four cases are the ones the reviews named: C# overloads (D-0003's
    /// flagship language gives `Parse(string)` and `Parse(Stream)` the same
    /// structural path), repeated Markdown headings, one real family, and two
    /// families whose anchors are indistinguishable as strings.
    #[test]
    fn collapse_preserves_overloads_and_repeated_headings() {
        /// Same helper as [`hit`], with a real span so the two chunks differ
        /// in everything except their anchor.
        fn spanned(id: &str, path: &str, kind: ChunkKind, line: u32) -> SearchHit {
            let mut h = hit(id, path, kind, None);
            h.chunk.line_start = line;
            h.chunk.line_end = line + 20;
            h.chunk.byte_start = line * 40;
            h.chunk.byte_end = (line + 20) * 40;
            h
        }
        let overload = |id: &str, line: u32| {
            spanned(
                id,
                "Assets/Scripts/Parser.cs",
                ChunkKind::Code {
                    symbol_kind: "method_declaration".into(),
                    symbol_path: "Parser.Parse".into(),
                    window: None,
                },
                line,
            )
        };
        let notes = |id: &str, line: u32| {
            spanned(
                id,
                "docs/notes.md",
                ChunkKind::Section {
                    heading_path: vec!["Design".into(), "Notes".into()],
                    window: None,
                },
                line,
            )
        };

        // 1. Two overloads: one anchor, one file, two methods. Both are results.
        assert_eq!(
            ids(&fuse(
                vec![vec![
                    overload("parse-text", 10),
                    overload("parse-stream", 90)
                ]],
                10
            )),
            ["parse-text", "parse-stream"],
        );

        // 2. Two sibling `## Notes` sections under the same parent heading.
        assert_eq!(
            ids(&fuse(
                vec![vec![notes("notes-a", 4), notes("notes-b", 60)]],
                10
            )),
            ["notes-a", "notes-b"],
        );

        // 3. One oversized method, split by the chunker. Its windows are three
        //    views of one span, and the best-scoring one speaks for them.
        let member = |id: &str, index: u32| {
            spanned(
                id,
                "Assets/Scripts/Parser.cs",
                ChunkKind::Code {
                    symbol_kind: "method_declaration".into(),
                    symbol_path: format!("Parser.Parse#w{index}"),
                    window: Some(WindowFamily { family: 0, index }),
                },
                10 + index * 30,
            )
        };
        assert_eq!(
            ids(&fuse(
                vec![vec![member("w0", 0), member("w1", 1), member("w2", 2)]],
                10
            )),
            ["w0"],
        );

        // 4. Both overloads oversized: two families, and every string they
        //    carry — anchor, `#w` suffix, file — is identical across them.
        //    Only the family ordinal separates them, and it must, or two
        //    methods become one result.
        let of_family = |id: &str, family: u32, index: u32| {
            spanned(
                id,
                "Assets/Scripts/Parser.cs",
                ChunkKind::Code {
                    symbol_kind: "method_declaration".into(),
                    symbol_path: format!("Parser.Parse#w{index}"),
                    window: Some(WindowFamily { family, index }),
                },
                10 + family * 100 + index * 30,
            )
        };
        let both = vec![
            of_family("text-w0", 0, 0),
            of_family("stream-w0", 1, 0),
            of_family("text-w1", 0, 1),
            of_family("stream-w1", 1, 1),
        ];
        // One result per family, in the order the arm ranked their best
        // members — never one total, and never all four.
        assert_eq!(ids(&fuse(vec![both], 10)), ["text-w0", "stream-w0"]);
    }

    #[test]
    fn text_windows_are_not_collapsed() {
        // Consecutive log windows are different content, not one split span.
        let hits = vec![
            hit("w0", "run.log", ChunkKind::Window { index: 0 }, None),
            hit("w1", "run.log", ChunkKind::Window { index: 1 }, None),
        ];
        assert_eq!(ids(&fuse(vec![hits], 10)).len(), 2);
    }

    #[test]
    fn fusion_of_nothing_is_nothing() {
        assert!(fuse(vec![Vec::new(), Vec::new()], 10).is_empty());
        assert!(fuse(vec![vec![hit("a", "a.md", section("A"), None)]], 0).is_empty());
    }

    #[test]
    fn excerpt_truncates_on_a_character_boundary() {
        let text: String = std::iter::repeat_n('é', EXCERPT_MAX_CHARS + 10).collect();
        let (clipped, truncated) = excerpt(&text);
        assert!(truncated);
        assert_eq!(clipped.chars().count(), EXCERPT_MAX_CHARS);

        let short = "fn main() {}";
        assert_eq!(excerpt(short), (short.to_string(), false));
    }

    /// The wire has to carry *both* readings of authority, and the note only
    /// when they disagree — an agent that cannot see the disagreement will
    /// trust the declaration.
    #[test]
    fn results_report_declared_and_effective_authority_separately() {
        let sources: HashMap<ProjectId, Project> = [(
            1,
            Project {
                id: 1,
                root: camino::Utf8PathBuf::from(r"C:\repos\lore"),
                name: "lore".into(),
                key: "lore".into(),
                kind: SourceKind::Repo,
                // The corpus these assertions were written against: a repo
                // that committed `lore-v1` with authority-aware ranking.
                authority: RepoAuthority {
                    profile: Some(Profile::LoreV1),
                    behavior: Behavior::Rank,
                    error: None,
                },
            },
        )]
        .into_iter()
        .collect();

        let mut demoted = at_tier(
            "x",
            "design/99_Scratch/notes.md",
            section("Notes"),
            vault(Some(DesignStatus::Decided), &["D-0007"]),
            0,
        );
        demoted.authority.demotion = Some(Demotion::ScratchPath);
        let result = to_result(&sources, demoted);
        assert_eq!(result.project, "lore");
        assert_eq!(result.project_key, "lore");
        assert_eq!(result.design_status.as_deref(), Some("decided"));
        assert_eq!(result.effective_authority.as_deref(), Some("deprecated"));
        assert_eq!(
            result.authority_note.as_deref(),
            Some("99_Scratch path cap")
        );
        // Citations survive as metadata even though they earn nothing.
        assert_eq!(result.decision_refs, ["D-0007"]);

        let honest = to_result(
            &sources,
            at_tier(
                "y",
                "design/1.1.md",
                section("Overview"),
                vault(Some(DesignStatus::Decided), &["D-0007"]),
                3,
            ),
        );
        assert_eq!(honest.effective_authority.as_deref(), Some("decided"));
        assert_eq!(
            honest.authority_note, None,
            "no note when nothing was demoted"
        );

        // The same hit from a repository that never opted in carries no
        // authority reading at all — absent, not "neutral" (D-0012).
        let neutral_sources: HashMap<ProjectId, Project> = sources
            .iter()
            .map(|(id, source)| {
                let mut source = source.clone();
                source.authority = RepoAuthority::default();
                (*id, source)
            })
            .collect();
        let unjudged = to_result(
            &neutral_sources,
            at_tier("z", "design/99_Scratch/notes.md", section("Notes"), None, 1),
        );
        assert_eq!(unjudged.effective_authority, None);
        assert_eq!(unjudged.authority_note, None);
    }

    #[test]
    fn decision_refs_merge_without_duplicates() {
        let merged = merge_refs(
            &["D-0003".into(), "D-0004".into()],
            &["D-0004".into(), "D-0007".into()],
        );
        assert_eq!(merged, ["D-0003", "D-0004", "D-0007"]);
    }
}
