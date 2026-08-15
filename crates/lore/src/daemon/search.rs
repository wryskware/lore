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
//! # Degradation is a first-class path
//!
//! `lexical_only` means exactly "vectors did not participate in *this*
//! response" — no endpoint, an unhealthy one, a query that could not be
//! embedded in time, or a corpus with no vectors yet in the filtered scope.
//! Every one of those returns results; none of them returns an error (D-0007).

use std::collections::{HashMap, HashSet};

use lore_core::{SearchRequest, SearchResponse, SearchResult};

use crate::embed::text::{WINDOW_MARKER, is_discriminator, strip_discriminators};
use crate::store::{ProjectId, SearchFilter, SearchHit, StatusFilter, Store};
use crate::types::{Chunk, ChunkKind, DesignStatus, authority_tier};

/// Results returned when the caller does not ask for a specific number.
pub const DEFAULT_LIMIT: u32 = 20;

/// Hard ceiling; `search` is meant to stay token-lean (`expand` exists for
/// depth, 3.1).
pub const MAX_LIMIT: u32 = 100;

/// Excerpts are capped so a handful of results cannot blow an agent's
/// context window on one tool call.
pub const EXCERPT_MAX_CHARS: usize = 2000;

/// Candidates pulled from the lexical arm before fusion. Deeper than the
/// default page on purpose: a chunk the vector arm loves is worth surfacing
/// even if BM25 put it at 40, and that only works if BM25 was asked for 50.
pub const LEXICAL_CANDIDATES: usize = 50;

/// Candidates pulled from the vector arm before fusion.
pub const VECTOR_CANDIDATES: usize = 50;

/// RRF damping constant. 60 is the value from the original Cormack et al.
/// result and the de-facto default everywhere since; it makes the top of each
/// list matter without letting rank 1 dominate rank 3.
pub const RRF_K: f64 = 60.0;

// Vault-authority multipliers, applied *after* fusion (3.1 step 2).
//
// The **ordering** — decided > leaning > exploration/unclassified >
// deprecated — is the canon requirement (3.1, and `types::authority_tier`).
// These exact numbers are tuning: they are deliberately gentle, so authority
// breaks ties and lifts a near-miss, and never resurrects an irrelevant
// document. Expect them to move during dogfooding.
/// `decided` — ratified material.
pub const AUTHORITY_DECIDED: f64 = 1.15;
/// `leaning`, and unclassified-but-ledger-cited.
pub const AUTHORITY_LEANING: f64 = 1.05;
/// `exploration`, unclassified, and all non-vault (code) chunks.
pub const AUTHORITY_NEUTRAL: f64 = 1.0;
/// `deprecated` — still searchable, deliberately demoted.
pub const AUTHORITY_DEPRECATED: f64 = 0.7;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("unknown project `{0}`")]
    UnknownProject(String),
    #[error(
        "unknown design status `{0}` (expected exploration, leaning, decided, deprecated or unclassified)"
    )]
    UnknownStatus(String),
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
    let names: HashMap<ProjectId, String> =
        projects.iter().map(|p| (p.id, p.name.clone())).collect();

    let mut filter = SearchFilter::default();
    if let Some(key) = &request.project {
        let project = super::resolve_project(&projects, key)
            .ok_or_else(|| SearchError::UnknownProject(key.clone()))?;
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

    let limit = request.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT) as usize;

    // Ask each arm for a candidate pool at least as deep as the page the
    // caller wants; otherwise `limit = 100` would silently top out at 50.
    let lexical = store.lexical_search(&request.query, &filter, limit.max(LEXICAL_CANDIDATES))?;

    // A vector-arm failure degrades this request instead of failing it: the
    // usual cause is a fingerprint that moved under a live query (dimension
    // mismatch), which the worker is already fixing by re-embedding.
    let vector = match query_vector {
        Some(query_vector) => {
            match store.vector_search(query_vector, &filter, limit.max(VECTOR_CANDIDATES)) {
                Ok(hits) => hits,
                Err(err) => {
                    tracing::debug!(error = %err, "vector arm failed; falling back to lexical-only");
                    Vec::new()
                }
            }
        }
        None => Vec::new(),
    };

    // Honest by construction: an embedded query over a corpus with no vectors
    // in the filtered scope contributed nothing, and says so.
    let lexical_only = vector.is_empty();
    let hits = fuse(vec![lexical, vector], limit);

    let results = hits.into_iter().map(|hit| to_result(&names, hit)).collect();
    Ok(SearchResponse {
        results,
        lexical_only,
    })
}

/// One chunk's accumulating fusion state.
struct Candidate {
    project: ProjectId,
    chunk: Chunk,
    /// Σ over the lists this chunk appears in.
    rrf: f64,
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
fn fuse(lists: Vec<Vec<SearchHit>>, limit: usize) -> Vec<SearchHit> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen: HashMap<(ProjectId, String), usize> = HashMap::new();

    for list in lists {
        for (rank, hit) in list.into_iter().enumerate() {
            let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
            let key = (hit.project, hit.chunk.id.0.clone());
            match seen.get(&key) {
                Some(&index) => candidates[index].rrf += contribution,
                None => {
                    seen.insert(key, candidates.len());
                    candidates.push(Candidate {
                        project: hit.project,
                        chunk: hit.chunk,
                        rrf: contribution,
                    });
                }
            }
        }
    }

    let mut scored: Vec<(f64, Candidate)> = candidates
        .into_iter()
        .map(|candidate| {
            (
                candidate.rrf * authority_weight(&candidate.chunk),
                candidate,
            )
        })
        .collect();
    // Chunk id breaks ties so identical scores rank identically across runs;
    // an unstable top-N would make every snapshot and agent flaky.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.chunk.id.0.cmp(&b.1.chunk.id.0))
    });

    let mut kept: HashSet<(ProjectId, camino::Utf8PathBuf, String)> = HashSet::new();
    let mut out = Vec::with_capacity(limit.min(scored.len()));
    for (score, candidate) in scored {
        if out.len() >= limit {
            break;
        }
        let key = (
            candidate.project,
            candidate.chunk.path.clone(),
            collapse_anchor(&candidate.chunk),
        );
        // Window collapse: the chunker splits an oversized symbol or section
        // into overlapping `#w0/#w1/…` windows, so a query matching the
        // overlap hits several of them. They are one place in one file; the
        // best-scoring window represents them and the rest are noise the
        // caller would have to deduplicate itself.
        if !kept.insert(key) {
            continue;
        }
        out.push(SearchHit {
            project: candidate.project,
            chunk: candidate.chunk,
            score: score as f32,
        });
    }
    out
}

/// Identity of a chunk's *location*, ignoring which oversize window it is.
///
/// Only `#w` is stripped. `#s<n>` distinguishes genuinely different runs of
/// unnamed statements and `#d<n>` distinguishes real duplicate spans; folding
/// either would hide results rather than deduplicate them. Text-window chunks
/// (`ChunkKind::Window`) keep their ordinal for the same reason — consecutive
/// windows of a log file are different content, not one split symbol.
fn collapse_anchor(chunk: &Chunk) -> String {
    match &chunk.kind {
        ChunkKind::Code { symbol_path, .. } => {
            format!("code:{}", strip_discriminators(symbol_path, &WINDOW_MARKER))
        }
        ChunkKind::Section { heading_path, .. } => {
            let titles: Vec<&str> = heading_path
                .iter()
                .filter(|title| !is_discriminator(title, &WINDOW_MARKER))
                .map(String::as_str)
                .collect();
            format!("md:{}", titles.join(" > "))
        }
        ChunkKind::Window { index } => format!("win:{index}"),
    }
}

/// The 3.1 vault-status modifier.
///
/// One deliberate refinement: an *unclassified* chunk that cites decisions is
/// weighted as `leaning` rather than as unclassified. 3.1 ranks
/// "`decided`/ledger-cited" together, and a document quoting D-0007 is
/// answering from the ledger whether or not anyone wrote frontmatter on it.
/// Code chunks have no vault metadata at all and stay neutral.
fn authority_weight(chunk: &Chunk) -> f64 {
    let status = chunk.vault.as_ref().and_then(|vault| vault.design_status);
    if status.is_none() && cites_decisions(chunk) {
        return AUTHORITY_LEANING;
    }
    match authority_tier(status) {
        3 => AUTHORITY_DECIDED,
        2 => AUTHORITY_LEANING,
        0 => AUTHORITY_DEPRECATED,
        _ => AUTHORITY_NEUTRAL,
    }
}

fn cites_decisions(chunk: &Chunk) -> bool {
    chunk.vault.as_ref().is_some_and(|vault| {
        !vault.decision_refs.is_empty() || !vault.body_decision_refs.is_empty()
    })
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

fn to_result(names: &HashMap<ProjectId, String>, hit: SearchHit) -> SearchResult {
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

    SearchResult {
        chunk_id: chunk.id.0,
        project: names
            .get(&hit.project)
            .cloned()
            .unwrap_or_else(|| hit.project.to_string()),
        path: chunk.path.into_string(),
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        language: chunk.language,
        symbol_path,
        heading_path,
        design_status: design_status.map(str::to_string),
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
    use crate::types::{Chunk, ChunkId, VaultMeta};
    use camino::Utf8PathBuf;

    /// `id` is what ties a chunk together across the two ranked lists, so the
    /// helper takes it explicitly.
    fn hit(id: &str, path: &str, kind: ChunkKind, vault: Option<VaultMeta>) -> SearchHit {
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
    fn authority_orders_equal_ranks_by_vault_status() {
        use DesignStatus::*;
        // Every chunk is rank 1 of its own list, so fusion scores are equal
        // and only the authority multiplier can separate them.
        let lists: Vec<Vec<SearchHit>> = [
            ("deprecated", vault(Some(Deprecated), &[])),
            ("exploration", vault(Some(Exploration), &[])),
            ("decided", vault(Some(Decided), &[])),
            ("leaning", vault(Some(Leaning), &[])),
            ("code", None),
            ("cited", vault(None, &["D-0007"])),
        ]
        .into_iter()
        .map(|(id, meta)| vec![hit(id, &format!("{id}.md"), section(id), meta)])
        .collect();

        let fused = fuse(lists, 10);
        let order = ids(&fused);
        let rank = |id: &str| order.iter().position(|found| *found == id).unwrap();

        assert_eq!(order[0], "decided", "{order:?}");
        assert!(rank("leaning") < rank("exploration"), "{order:?}");
        assert!(rank("exploration") < rank("deprecated"), "{order:?}");
        assert_eq!(*order.last().unwrap(), "deprecated", "{order:?}");
        // Unclassified-but-ledger-cited ranks with `leaning`, above plain
        // exploration; a code chunk with no vault metadata stays neutral.
        assert!(rank("cited") < rank("exploration"), "{order:?}");
        assert!(rank("code") < rank("deprecated"), "{order:?}");
        assert!(rank("leaning") < rank("code"), "{order:?}");
    }

    #[test]
    fn authority_weights_match_the_documented_tiers() {
        use DesignStatus::*;
        let weight = |meta| authority_weight(&hit("x", "x.md", section("X"), meta).chunk);
        assert_eq!(weight(vault(Some(Decided), &[])), AUTHORITY_DECIDED);
        assert_eq!(weight(vault(Some(Leaning), &[])), AUTHORITY_LEANING);
        assert_eq!(weight(vault(Some(Exploration), &[])), AUTHORITY_NEUTRAL);
        assert_eq!(weight(vault(Some(Deprecated), &[])), AUTHORITY_DEPRECATED);
        assert_eq!(weight(None), AUTHORITY_NEUTRAL);
        assert_eq!(weight(vault(None, &[])), AUTHORITY_NEUTRAL);
        assert_eq!(weight(vault(None, &["D-0003"])), AUTHORITY_LEANING);
        // A cited *deprecated* document is still deprecated.
        assert_eq!(
            weight(vault(Some(Deprecated), &["D-0003"])),
            AUTHORITY_DEPRECATED
        );
    }

    #[test]
    fn overlapping_windows_of_one_span_collapse_to_the_best_one() {
        let code = |suffix: &str| ChunkKind::Code {
            symbol_kind: "method_declaration".into(),
            symbol_path: format!("Board.Update{suffix}"),
            window: None,
        };
        // Both windows of `Board.Update` match; only the better one survives.
        let lexical = vec![
            hit("w0", "Board.cs", code("#w0"), None),
            hit("w1", "Board.cs", code("#w1"), None),
            hit("other", "Board.cs", code("Other"), None),
        ];
        let fused = fuse(vec![lexical, Vec::new()], 10);
        assert_eq!(ids(&fused), ["w0", "other"]);

        // Same anchor in a *different file* is a different place.
        let cross_file = vec![
            hit("a", "Board.cs", code("#w0"), None),
            hit("b", "Other.cs", code("#w0"), None),
        ];
        assert_eq!(ids(&fuse(vec![cross_file], 10)).len(), 2);
    }

    #[test]
    fn section_windows_collapse_but_distinct_headings_do_not() {
        let windowed = |suffix: Option<&str>| ChunkKind::Section {
            heading_path: match suffix {
                Some(s) => vec!["Ranking".into(), s.to_string()],
                None => vec!["Ranking".into()],
            },
            window: None,
        };
        let hits = vec![
            hit("s0", "3.1.md", windowed(Some("#w0")), None),
            hit("s1", "3.1.md", windowed(Some("#w1")), None),
            hit("whole", "3.1.md", windowed(None), None),
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
        // s0/s1/whole are one location; `#d1` is a real duplicate span.
        assert_eq!(ids(&fuse(vec![hits], 10)), ["s0", "dup"]);
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

    #[test]
    fn decision_refs_merge_without_duplicates() {
        let merged = merge_refs(
            &["D-0003".into(), "D-0004".into()],
            &["D-0004".into(), "D-0007".into()],
        );
        assert_eq!(merged, ["D-0003", "D-0004", "D-0007"]);
    }
}
