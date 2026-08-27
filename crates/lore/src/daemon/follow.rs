//! Symbol following: prose is the signpost, the implementation rides along.
//!
//! Natural-language queries match natural language. Both of lore's ranking arms
//! reward that — BM25 scores word overlap, the vector arm scores similarity
//! between a prose question and a prose chunk — so a README paragraph that
//! *describes* the retry policy looks more like "how does the retry policy
//! work" than the `RetryPolicy` class that **is** it. Measured on the RCB
//! corpus, span recall for primary implementing source sat at 0.15–0.21 while
//! sample and doc evidence sat at 0.47–0.58, with coverage perfect: the
//! implementation was always in the index, ranked below the prose that names
//! it.
//!
//! Following that pointer is cheap, exact, and needs no new understanding of
//! the query. This module does exactly that and nothing more:
//!
//! 1. look at the top [`FOLLOW_TOP_HITS`] hits and keep the prose-adjacent ones
//!    (a doc chunk, or code under a `samples/`-shaped path);
//! 2. pull identifier-shaped references out of their text under a strict
//!    specificity rule — never a bare lowercase word;
//! 3. resolve each name against the **existing** `chunks_fts.anchor` index by
//!    exact symbol-path tail match ([`Store::symbol_anchor_candidates`]);
//! 4. hand the winners back as ordinary [`SearchResult`]s, each labelled with
//!    the span that named it.
//!
//! It knows nothing about bundles. Rendering, budgeting and the honesty rules
//! around a followed span live in [`super::bundle`]; the split is deliberate,
//! because *find and resolve a reference* is generic and *render and pay for
//! it* is not.
//!
//! # What this deliberately is not
//!
//! **Not query translation.** "the concurrent orchestrator" resolves to
//! nothing, forever. Turning an English description into a symbol is the
//! rewriting layer the bundle contract rules out, and exact-name-only is what
//! keeps this module from growing one.
//!
//! **Not part of `search`.** Every field on a `search` result, `score`
//! included, means "this is where fusion put it", and a followed definition has
//! no fused score. Appending one past the page breaks `limit`; interleaving one
//! breaks order. Either way a consumer reading `results[0..n]` as *the ranking*
//! would be reading something else. `search`'s wire bytes are unchanged.
//!
//! **Not evidence that the retrieval succeeded.** A definition lore chose to
//! include because a doc mentioned it never feeds the bundle's coverage or
//! verdict — see [`super::bundle::assemble`].
//!
//! # Cost
//!
//! A pure string scan over at most five 2000-char excerpts, then **one** FTS5
//! statement with a narrow projection and a hard row cap, then at most
//! [`FOLLOW_MAX_TOTAL`] point lookups. The one statement runs inside the store
//! lock, which is why it is batched rather than one query per symbol.

use std::collections::{HashMap, HashSet};

use lore_core::{BundleVia, SearchResult};

use crate::store::{AnchorCandidate, Project, ProjectId, SearchFilter, SearchHit, Store};
use crate::types::ChunkKind;

use super::bundle::{case_parts, is_stopword};
use super::search;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------
//
// Constants, not configuration, for the same reason `bundle`'s are: they were
// chosen against one corpus, and a knob nobody can set correctly is worse than
// a number in the source.

/// Ranked hits examined for references. A rank-20 prose hit is a weak signpost
/// and costs exactly as much to follow as a good one.
pub const FOLLOW_TOP_HITS: usize = 5;

/// Distinct references taken from one hit, in document order. An API-reference
/// page with two hundred backticked identifiers must not become two hundred
/// lookups.
pub const FOLLOW_MAX_CANDIDATES_PER_HIT: usize = 12;

/// Names in the one batched MATCH expression. Mirrors `query::MAX_TERMS`.
pub const FOLLOW_MAX_TERMS: usize = 32;

/// Rows the anchor scan may return. A hot token (`Client` as a segment of a
/// thousand symbol paths) costs a missed follow-in here, never a slow query.
pub const FOLLOW_SCAN_ROWS: usize = 400;

/// Definitions shown for one name. Two overloads really are both the
/// definition; showing one and hiding the other would be a lie by omission.
pub const FOLLOW_MAX_DEFS_PER_SYMBOL: usize = 2;

/// Definitions attributed to one referring hit.
pub const FOLLOW_MAX_PER_HIT: usize = 3;

/// Definitions in one bundle.
pub const FOLLOW_MAX_TOTAL: usize = 6;

/// Past this many definitions of one name, the symbol is skipped in silence. A
/// name that means five things is not a signpost, and there is nothing useful
/// to say about it.
pub const FOLLOW_AMBIGUITY_CAP: usize = 4;

/// Shortest reference worth resolving, in characters.
pub const FOLLOW_MIN_CHARS: usize = 4;

/// Extra rendered text a bundle may carry for followed definitions, as a
/// fraction of the caller's budget — **on top of** it, not carved out of it.
///
/// Carving it out was rejected: the eval already shows the 4000-token budget
/// demoting gold evidence to further reading, so taking tokens away from ranked
/// spans to make room for definitions would trade one recall loss for another
/// and muddy the measurement. This is the one place the design knowingly costs
/// the caller tokens, so the bundle header says how many.
pub const FOLLOW_BUDGET_SHARE: f64 = 0.35;

/// Language tags whose chunks are prose. `None` (unknown text) counts too.
const DOC_LANGUAGES: &[&str] = &["markdown", "mdx", "rst"];

/// Path segments that make a *code* chunk prose-adjacent.
///
/// `tests` and `benchmarks` are deliberately absent in v1: test directories are
/// dense with symbol references and would dominate the candidate set on every
/// query, and test files are frequently the gold evidence themselves — so they
/// are already being ranked on their own merits.
const SAMPLE_SEGMENTS: &[&str] = &[
    "cookbook",
    "demo",
    "demos",
    "doc",
    "docs",
    "example",
    "examples",
    "getting-started",
    "sample",
    "samples",
    "snippets",
    "tutorial",
    "tutorials",
];

// ---------------------------------------------------------------------------
// What comes out
// ---------------------------------------------------------------------------

/// A definition pulled in because something above it said its name.
#[derive(Debug, Clone)]
pub struct Followed {
    /// The definition, shaped exactly like any other search result so the
    /// assembler can verify it with no special case. Its `score` is 0: it was
    /// not ranked, and pretending otherwise would put a fused score on the wire
    /// that fusion never computed.
    pub hit: SearchResult,
    /// The span that named it, and the reference that resolved.
    pub via: BundleVia,
    /// `1 of 3 definitions`, when the name resolved to more than one place.
    /// `None` when it resolved to exactly one, because there is nothing to
    /// disclose.
    pub note: Option<String>,
    /// Rank index of the referring hit, so the assembler can render this
    /// immediately after it rather than at the tail.
    pub origin: usize,
}

/// One identifier-shaped reference found in a hit's text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reference {
    /// As written: `AgentThread.RunAsync`, `store::vector_search`, `RunAsync`.
    text: String,
    /// Its dotted / `::` segments. One entry for a bare identifier.
    chain: Vec<String>,
    /// Rank index of the hit it was found in.
    origin: usize,
}

impl Reference {
    /// The name to resolve: the last segment of the chain.
    fn name(&self) -> &str {
        self.chain.last().map(String::as_str).unwrap_or_default()
    }
}

/// One winner, before its text has been read out of the store.
struct Pick {
    candidate: AnchorCandidate,
    via: BundleVia,
    note: Option<String>,
    origin: usize,
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// The whole follow pass: references out of the top hits, resolved against the
/// anchor index, hydrated into results.
///
/// Empty when disabled, when no hit is prose-adjacent, when nothing extracted
/// clears the specificity floor, or when nothing resolves. A store failure is
/// logged and degrades to empty for the same reason the vector arm does: a
/// bundle without its follow-ins is a smaller bundle, not a failed request.
pub fn resolve(
    store: &Store,
    project: ProjectId,
    hits: &[SearchResult],
    enabled: bool,
) -> Vec<Followed> {
    if !enabled || hits.is_empty() {
        return Vec::new();
    }

    let mut references: Vec<Reference> = Vec::new();
    for (index, hit) in hits.iter().take(FOLLOW_TOP_HITS).enumerate() {
        if is_prose_adjacent(hit) {
            references.extend(candidates(hit, index));
        }
    }
    if references.is_empty() {
        return Vec::new();
    }

    // One deduplicated, capped name list for the one statement. The cap is on
    // *names*, not references: two docs naming `RunAsync` ask one question.
    let mut names: Vec<String> = Vec::new();
    for reference in &references {
        let name = reference.name().to_string();
        if !name.is_empty() && !names.contains(&name) && names.len() < FOLLOW_MAX_TERMS {
            names.push(name);
        }
    }

    let rows = match store.symbol_anchor_candidates(
        &SearchFilter::project(project),
        &names,
        FOLLOW_SCAN_ROWS,
    ) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(error = %err, "symbol following: the anchor lookup failed");
            return Vec::new();
        }
    };
    if rows.is_empty() {
        return Vec::new();
    }

    hydrate(store, project, pick(&references, &names, &rows, hits))
}

/// Is this hit the kind of thing that *points at* code rather than being it?
fn is_prose_adjacent(hit: &SearchResult) -> bool {
    is_doc(hit) || is_sample_path(&hit.path)
}

/// A Markdown section, a doc language, or unknown text.
fn is_doc(hit: &SearchResult) -> bool {
    if hit.heading_path.is_some() {
        return true;
    }
    match hit.language.as_deref() {
        None => true,
        Some(language) => DOC_LANGUAGES.contains(&language.to_ascii_lowercase().as_str()),
    }
}

fn is_sample_path(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|segment| SAMPLE_SEGMENTS.contains(&segment.to_ascii_lowercase().as_str()))
}

/// Identifier-shaped references in one hit's excerpt, in document order.
///
/// Two extraction modes, because the two hit kinds carry different signal.
///
/// **Doc chunks** only yield candidates from places the author marked as code —
/// inline backtick spans and fenced blocks — plus dotted / `::` chains anywhere
/// in the running text. Prose that merely capitalizes a word ("the Agent loop",
/// "our Store") is not a reference, and the backtick rule excludes it for free
/// at almost no cost in recall: an author who names a symbol in a doc nearly
/// always marks it up.
///
/// **Sample code chunks** are code throughout, so backticks do not apply.
/// Tokens are taken in *reference position*: immediately followed by `(` or
/// `<`, immediately preceded by `new `, or part of a chain. Declarations and
/// locals are skipped by construction.
fn candidates(hit: &SearchResult, origin: usize) -> Vec<Reference> {
    let text = hit.excerpt.as_str();
    let doc = is_doc(hit);
    let marked = if doc { code_regions(text) } else { Vec::new() };

    let mut out: Vec<Reference> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (start, end, chain) in chains(text) {
        let interesting = chain.len() >= 2
            || if doc {
                marked.iter().any(|(from, to)| start >= *from && end <= *to)
            } else {
                in_reference_position(text, start, end)
            };
        if !interesting {
            continue;
        }
        let written = &text[start..end];
        if !is_specific(written, &chain) || !seen.insert(written.to_string()) {
            continue;
        }
        out.push(Reference {
            text: written.to_string(),
            chain,
            origin,
        });
        if out.len() >= FOLLOW_MAX_CANDIDATES_PER_HIT {
            break;
        }
    }
    out
}

/// The specificity floor, applied to the reference **as written**.
///
/// Three rules, and the second is the one that does the work: a candidate must
/// be *multi-part*. `run`, `main`, `get`, `parse` and `send` are single
/// lowercase words and are rejected without anyone maintaining a list of
/// forbidden names — which matters, because such a list is unmaintainable
/// across languages. `Board.run` survives, because the chain makes it specific.
fn is_specific(written: &str, chain: &[String]) -> bool {
    written.chars().count() >= FOLLOW_MIN_CHARS
        && (chain.len() >= 2 || written.contains('_') || case_parts(written).len() >= 2)
        && !is_stopword(&written.to_ascii_lowercase())
}

/// Byte ranges of `text` the author marked as code: everything between
/// matching runs of backticks, inline spans and fenced blocks alike.
///
/// A fence's info string (` ```csharp `) falls inside the region and yields one
/// lowercase word, which the specificity floor drops. An unterminated run ends
/// the scan rather than swallowing the rest of the chunk.
fn code_regions(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let open = run_of_backticks(bytes, i);
        let width = open - i;
        let Some(close) = next_backtick_run(bytes, open, width) else {
            break;
        };
        out.push((open, close));
        i = run_of_backticks(bytes, close);
    }
    out
}

/// End of the backtick run starting at `from`.
fn run_of_backticks(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() && bytes[i] == b'`' {
        i += 1;
    }
    i
}

/// Start of the next run of at least `width` backticks at or after `from`.
fn next_backtick_run(bytes: &[u8], from: usize, width: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let end = run_of_backticks(bytes, i);
        if end - i >= width {
            return Some(i);
        }
        i = end;
    }
    None
}

/// Every dotted / `::` chain of ASCII identifiers in `text`, as
/// `(start, end, segments)` byte ranges.
///
/// A trailing separator that is not followed by an identifier ends the chain,
/// so `use \`Thread\`. Then` yields `Thread` rather than `Thread.Then`.
fn chains(text: &str) -> Vec<(usize, usize, Vec<String>)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let Some(first) = identifier_at(bytes, i) else {
            i += 1;
            continue;
        };
        let start = i;
        let mut end = first;
        let mut segments = vec![text[start..first].to_string()];
        loop {
            let separator = if bytes[end..].starts_with(b"::") {
                2
            } else if bytes[end..].starts_with(b".") {
                1
            } else {
                break;
            };
            let Some(next) = identifier_at(bytes, end + separator) else {
                break;
            };
            segments.push(text[end + separator..next].to_string());
            end = next;
        }
        out.push((start, end, segments));
        i = end;
    }
    out
}

/// End of the `[A-Za-z_][A-Za-z0-9_]*` starting exactly at `from`, if there is
/// one. A digit cannot start an identifier, so `2fa` is not one.
fn identifier_at(bytes: &[u8], from: usize) -> Option<usize> {
    if from >= bytes.len() || !(bytes[from].is_ascii_alphabetic() || bytes[from] == b'_') {
        return None;
    }
    let mut i = from;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    Some(i)
}

/// Is the token at `start..end` being *used* rather than merely spelled?
fn in_reference_position(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    matches!(bytes.get(end), Some(b'(' | b'<')) || text[..start].ends_with("new ")
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// References plus anchor rows → the definitions worth rendering.
///
/// Ordering is by reference (rank order of the referring hit, then document
/// order within it), and within one reference by `(path, line_start)` after the
/// chain-agreement preference, so two runs of one query pick the same
/// definitions in the same order.
fn pick(
    references: &[Reference],
    names: &[String],
    rows: &[AnchorCandidate],
    hits: &[SearchResult],
) -> Vec<Pick> {
    // A definition the ranking already returned is not a follow-in: rendering
    // the same chunk twice is exactly the highest-value case (a doc next to its
    // own implementation) going wrong.
    let ranked: HashSet<&str> = hits.iter().map(|hit| hit.chunk_id.as_str()).collect();
    let mut used: HashSet<String> = HashSet::new();
    let mut per_hit: HashMap<usize, usize> = HashMap::new();
    let mut out: Vec<Pick> = Vec::new();

    for reference in references {
        if out.len() >= FOLLOW_MAX_TOTAL {
            break;
        }
        // A name past the term cap was never asked about, so no row can be its
        // definition and pretending otherwise would silently match a homonym.
        if !names.iter().any(|name| name == reference.name()) {
            continue;
        }
        if per_hit.get(&reference.origin).copied().unwrap_or(0) >= FOLLOW_MAX_PER_HIT {
            continue;
        }
        let defs = definitions(reference, rows, &ranked);
        if defs.is_empty() || defs.len() > FOLLOW_AMBIGUITY_CAP {
            continue;
        }
        let total = defs.len();
        let Some(referrer) = hits.get(reference.origin) else {
            continue;
        };
        for (index, candidate) in defs
            .into_iter()
            .take(FOLLOW_MAX_DEFS_PER_SYMBOL)
            .enumerate()
        {
            if out.len() >= FOLLOW_MAX_TOTAL
                || per_hit.get(&reference.origin).copied().unwrap_or(0) >= FOLLOW_MAX_PER_HIT
            {
                break;
            }
            if !used.insert(candidate.chunk_id.0.clone()) {
                continue;
            }
            out.push(Pick {
                candidate,
                via: BundleVia {
                    path: referrer.path.replace('\\', "/"),
                    line_start: referrer.line_start,
                    line_end: referrer.line_end,
                    symbol: reference.text.clone(),
                },
                // Named only when there is ambiguity to disclose.
                note: (total > 1).then(|| format!("{} of {total} definitions", index + 1)),
                origin: reference.origin,
            });
            *per_hit.entry(reference.origin).or_default() += 1;
        }
    }
    out
}

/// The rows that really are a definition of `reference`, best first.
fn definitions(
    reference: &Reference,
    rows: &[AnchorCandidate],
    ranked: &HashSet<&str>,
) -> Vec<AnchorCandidate> {
    let name = reference.name();
    // Window index per (path, base symbol) family, so a definition the chunker
    // split arrives once — as window 0, which carries the signature.
    let mut best_window: HashMap<(String, String, u32), usize> = HashMap::new();
    let mut kept: Vec<AnchorCandidate> = Vec::new();

    for row in rows {
        let ChunkKind::Code {
            symbol_kind,
            symbol_path,
            window,
        } = &row.kind
        else {
            continue;
        };
        // The chunker's filler spans name nothing a reader can use, and
        // `bundle::label` already refuses to print them. A merged run of tiny
        // members (`group`) is *not* filler: it keeps its first member's symbol
        // path, so it genuinely is that symbol's definition.
        if symbol_kind == "statements" {
            continue;
        }
        let base = strip_window_suffix(symbol_path);
        let Some(tail) = base.rsplit('.').next() else {
            continue;
        };
        // Case-sensitively: FTS folded case to *find* the row, and the author's
        // spelling decides whether it is really a reference to this name.
        if tail != name || tail.starts_with('#') {
            continue;
        }
        if ranked.contains(row.chunk_id.0.as_str()) {
            continue;
        }
        match window {
            Some(family) => {
                let key = (row.path.to_string(), base.to_string(), family.family);
                match best_window.get(&key) {
                    Some(&at) => {
                        let incumbent = window_index(&kept[at]);
                        if family.index < incumbent {
                            kept[at] = row.clone();
                        }
                    }
                    None => {
                        best_window.insert(key, kept.len());
                        kept.push(row.clone());
                    }
                }
            }
            // Two unsplit overloads share a symbol path and are two
            // definitions, so unwindowed rows are never folded together.
            None => kept.push(row.clone()),
        }
    }

    kept.sort_by(|a, b| {
        agreement(reference, b)
            .cmp(&agreement(reference, a))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line_start.cmp(&b.line_start))
            .then_with(|| a.chunk_id.0.cmp(&b.chunk_id.0))
    });
    kept
}

/// How many trailing segments of the reference the candidate's symbol path
/// agrees with. `Board.Update` prefers `Lexomancy.Board.Update` (2) over
/// `Timer.Update` (1).
fn agreement(reference: &Reference, candidate: &AnchorCandidate) -> usize {
    let ChunkKind::Code { symbol_path, .. } = &candidate.kind else {
        return 0;
    };
    let base = strip_window_suffix(symbol_path);
    let mut theirs = base.rsplit('.');
    let mut matched = 0;
    for ours in reference.chain.iter().rev() {
        match theirs.next() {
            Some(segment) if segment == ours => matched += 1,
            _ => break,
        }
    }
    matched
}

fn window_index(candidate: &AnchorCandidate) -> u32 {
    match &candidate.kind {
        ChunkKind::Code { window, .. } | ChunkKind::Section { window, .. } => {
            window.map(|family| family.index).unwrap_or(0)
        }
        ChunkKind::Window { index } => *index,
    }
}

/// `Board.Update#w1` → `Board.Update`. Bookkeeping, not part of the name.
fn strip_window_suffix(symbol: &str) -> &str {
    match symbol.rsplit_once("#w") {
        Some((base, ordinal))
            if !ordinal.is_empty() && ordinal.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => symbol,
    }
}

/// Read the winners' text and shape them as search results.
fn hydrate(store: &Store, project: ProjectId, picks: Vec<Pick>) -> Vec<Followed> {
    if picks.is_empty() {
        return Vec::new();
    }
    let sources: HashMap<ProjectId, Project> = match store.list_projects() {
        Ok(projects) => projects.into_iter().map(|p| (p.id, p)).collect(),
        Err(err) => {
            tracing::debug!(error = %err, "symbol following: could not list projects");
            return Vec::new();
        }
    };

    let mut out = Vec::with_capacity(picks.len());
    for pick in picks {
        let chunk = match store.get_chunk(project, &pick.candidate.chunk_id) {
            Ok(Some(chunk)) => chunk,
            // The row was there a statement ago; if it is not now, the index
            // moved under us and the definition simply does not appear.
            Ok(None) => continue,
            Err(err) => {
                tracing::debug!(error = %err, "symbol following: could not read a definition");
                continue;
            }
        };
        out.push(Followed {
            hit: search::to_result(
                &sources,
                SearchHit {
                    project,
                    chunk,
                    authority: pick.candidate.authority,
                    score: 0.0,
                },
            ),
            via: pick.via,
            note: pick.note,
            origin: pick.origin,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, language: Option<&str>, excerpt: &str) -> SearchResult {
        SearchResult {
            chunk_id: format!("id-{path}"),
            project: "fixture".into(),
            project_key: "fixture-0000".into(),
            path: path.into(),
            line_start: 1,
            line_end: 20,
            language: language.map(str::to_string),
            symbol_path: None,
            heading_path: None,
            design_status: None,
            effective_authority: None,
            authority_note: None,
            decision_refs: Vec::new(),
            score: 0.03,
            excerpt: excerpt.into(),
            excerpt_truncated: false,
        }
    }

    fn doc(excerpt: &str) -> SearchResult {
        let mut hit = hit("docs/guide.md", Some("markdown"), excerpt);
        hit.heading_path = Some(vec!["Guide".into()]);
        hit
    }

    fn sample(excerpt: &str) -> SearchResult {
        let mut hit = hit("samples/Program.cs", Some("csharp"), excerpt);
        hit.symbol_path = Some("Program.Main".into());
        hit
    }

    fn found(hit: &SearchResult) -> Vec<String> {
        candidates(hit, 0)
            .into_iter()
            .map(|reference| reference.text)
            .collect()
    }

    fn names(hit: &SearchResult) -> Vec<String> {
        candidates(hit, 0)
            .into_iter()
            .map(|reference| reference.name().to_string())
            .collect()
    }

    // -- extraction ---------------------------------------------------------

    #[test]
    fn camel_snake_and_dotted_references_are_taken_and_ordinary_words_are_not() {
        assert_eq!(
            found(&doc(
                "call `RunAsync` on `content_hash` via `AgentThread.RunAsync`"
            )),
            ["RunAsync", "content_hash", "AgentThread.RunAsync"]
        );
        // Single lowercase words: rejected by the multi-part rule, with no
        // hand-maintained list of forbidden names anywhere.
        assert!(found(&doc("`run` `get` `main` `code` `set`")).is_empty());
        // ...and `run` is not resurrected by being long enough, either.
        assert!(found(&doc("`parse` `send` `handler`")).is_empty());
    }

    #[test]
    fn a_doc_takes_backticked_and_fenced_names_but_not_capitalized_prose() {
        let hit = doc(
            "The Agent loop drives our Store, which is Nothing Special.\n\
             Use `RunAsync` to start it.\n\
             ```csharp\n\
             var thread = new AgentThread();\n\
             ```\n",
        );
        // `Agent`, `Store` and `Special` are prose the author never marked up;
        // `csharp` is a fence's info string and one lowercase word.
        assert_eq!(found(&hit), ["RunAsync", "AgentThread"]);
    }

    #[test]
    fn a_dotted_chain_in_running_prose_needs_no_backticks() {
        assert_eq!(
            found(&doc(
                "see AgentThread.RunAsync and store::vector_search for it"
            )),
            ["AgentThread.RunAsync", "store::vector_search"]
        );
        // The name is the tail of the chain, whichever separator was used.
        assert_eq!(
            names(&doc("see AgentThread.RunAsync and store::vector_search")),
            ["RunAsync", "vector_search"]
        );
    }

    #[test]
    fn a_sentence_full_stop_does_not_extend_a_chain() {
        assert_eq!(
            found(&doc("use `AgentThread`. Then wait.")),
            ["AgentThread"]
        );
    }

    #[test]
    fn a_sample_takes_call_and_construction_sites_but_not_bare_locals() {
        let hit = sample(
            "var myBuffer = 1;\n\
             var thread = new AgentThread();\n\
             ChatClient.Send(thread);\n\
             var list = ReadAll<Widget>();\n",
        );
        // `myBuffer` is multi-part but never used as a reference, so it is not
        // one; `Widget` is a type argument, not a call, and is skipped too.
        assert_eq!(found(&hit), ["AgentThread", "ChatClient.Send", "ReadAll"]);
    }

    #[test]
    fn a_chain_survives_the_floor_where_its_tail_alone_would_not() {
        // `run` is a stopword, three characters, and one lowercase word; the
        // chain is none of those, and the name it resolves is still `run`.
        assert_eq!(found(&doc("call Board.run now")), ["Board.run"]);
        assert_eq!(names(&doc("call Board.run now")), ["run"]);
    }

    #[test]
    fn the_splitter_is_the_bundles_own() {
        // Guards against a second, divergent splitter creeping in here: the
        // multi-part rule is `bundle::case_parts` and nothing else.
        assert_eq!(case_parts("HTTPServer"), ["HTTP", "Server"]);
        assert_eq!(found(&doc("`HTTPServer` and `httpserver`")), ["HTTPServer"]);
    }

    #[test]
    fn a_hit_yields_at_most_twelve_references() {
        let body: String = (0..40)
            .map(|i| format!("`WidgetFactory{i}` "))
            .collect::<Vec<_>>()
            .concat();
        let taken = found(&doc(&body));
        assert_eq!(taken.len(), FOLLOW_MAX_CANDIDATES_PER_HIT);
        assert_eq!(taken[0], "WidgetFactory0", "document order, not hash order");
        // Repeats of one name are one candidate, so a doc that says `RunAsync`
        // twenty times does not spend twenty of the twelve slots.
        assert_eq!(found(&doc(&"`RunAsync` ".repeat(20))), ["RunAsync"]);
    }

    // -- which hits are scanned ---------------------------------------------

    #[test]
    fn prose_adjacency_covers_docs_unknown_text_and_sample_paths() {
        assert!(is_prose_adjacent(&doc("x")));
        assert!(is_prose_adjacent(&hit("notes.txt", None, "x")));
        assert!(is_prose_adjacent(&hit(
            "Samples/Demo/Program.cs",
            Some("csharp"),
            "x"
        )));
        assert!(is_prose_adjacent(&hit(
            r"docs\api\Program.cs",
            Some("csharp"),
            "x"
        )));
        assert!(!is_prose_adjacent(&hit(
            "src/Agents/Thread.cs",
            Some("csharp"),
            "x"
        )));
        // v1 deliberately leaves tests and benchmarks out of the trigger set.
        assert!(!is_prose_adjacent(&hit(
            "tests/ThreadTests.cs",
            Some("csharp"),
            "x"
        )));
        assert!(!is_prose_adjacent(&hit(
            "benchmarks/Bench.cs",
            Some("csharp"),
            "x"
        )));
    }

    /// Assemble the references `resolve` would ask about, without a store.
    fn scanned(hits: &[SearchResult]) -> Vec<String> {
        let mut out = Vec::new();
        for (index, hit) in hits.iter().take(FOLLOW_TOP_HITS).enumerate() {
            if is_prose_adjacent(hit) {
                out.extend(candidates(hit, index).into_iter().map(|r| r.text));
            }
        }
        out
    }

    #[test]
    fn only_the_top_five_hits_and_only_prose_adjacent_ones_are_scanned() {
        let mut hits: Vec<SearchResult> = (0..8)
            .map(|i| {
                let mut hit = doc(&format!("`WidgetFactory{i}`"));
                hit.path = format!("docs/g{i}.md");
                hit
            })
            .collect();
        // Rank 2 is ordinary source: it points at nothing, so it is not read.
        hits[2] = hit("src/Thread.cs", Some("csharp"), "`WidgetFactoryX`");
        assert_eq!(
            scanned(&hits),
            [
                "WidgetFactory0",
                "WidgetFactory1",
                "WidgetFactory3",
                "WidgetFactory4"
            ]
        );
    }

    // -- resolution ---------------------------------------------------------

    fn code(path: &str, symbol: &str, kind: &str, line: u32) -> AnchorCandidate {
        AnchorCandidate {
            chunk_id: crate::types::ChunkId(format!("{path}:{symbol}:{line}")),
            path: camino::Utf8PathBuf::from(path),
            kind: ChunkKind::Code {
                symbol_kind: kind.into(),
                symbol_path: symbol.into(),
                window: None,
            },
            line_start: line,
            line_end: line + 10,
            authority: crate::authority::Authority {
                tier: 1,
                demotion: None,
            },
        }
    }

    fn windowed(path: &str, symbol: &str, family: u32, index: u32) -> AnchorCandidate {
        let mut candidate = code(
            path,
            &format!("{symbol}#w{index}"),
            "method",
            1 + index * 40,
        );
        candidate.kind = ChunkKind::Code {
            symbol_kind: "method".into(),
            symbol_path: format!("{symbol}#w{index}"),
            window: Some(crate::types::WindowFamily { family, index }),
        };
        candidate
    }

    fn reference(text: &str) -> Reference {
        Reference {
            text: text.to_string(),
            chain: text
                .split(['.'])
                .flat_map(|part| part.split("::"))
                .map(str::to_string)
                .collect(),
            origin: 0,
        }
    }

    fn resolved(text: &str, rows: &[AnchorCandidate]) -> Vec<String> {
        definitions(&reference(text), rows, &HashSet::new())
            .into_iter()
            .map(|candidate| candidate.chunk_id.0)
            .collect()
    }

    #[test]
    fn the_tail_must_match_the_reference_exactly_and_in_case() {
        let rows = [
            code("a.cs", "Agents.Thread.RunAsync", "method", 10),
            code("b.cs", "Other.runasync", "method", 10),
            code("c.cs", "RunAsyncHelper", "method", 10),
            code("d.cs", "Runner.Async", "method", 10),
        ];
        assert_eq!(
            resolved("RunAsync", &rows),
            ["a.cs:Agents.Thread.RunAsync:10"]
        );
    }

    #[test]
    fn a_longer_chain_agreement_wins_over_alphabetical_order() {
        let rows = [
            code("a.cs", "Timer.Update", "method", 10),
            code("z.cs", "Lexomancy.Board.Update", "method", 10),
        ];
        assert_eq!(
            resolved("Board.Update", &rows),
            ["z.cs:Lexomancy.Board.Update:10", "a.cs:Timer.Update:10"]
        );
        // With no chain to prefer by, the order is `(path, line_start)`.
        assert_eq!(
            resolved("Update", &rows),
            ["a.cs:Timer.Update:10", "z.cs:Lexomancy.Board.Update:10"]
        );
    }

    #[test]
    fn filler_spans_never_resolve_but_a_merged_group_does() {
        let rows = [
            code("a.cs", "Board.#s0", "statements", 10),
            code("b.cs", "Board.Update", "statements", 10),
            code("c.cs", "Board.Update", "group", 10),
        ];
        assert_eq!(resolved("Update", &rows), ["c.cs:Board.Update:10"]);
    }

    #[test]
    fn a_split_definition_arrives_once_as_its_first_window() {
        let rows = [
            windowed("a.cs", "Board.Update", 0, 1),
            windowed("a.cs", "Board.Update", 0, 0),
            // A second oversized overload is a second family and a second
            // definition, however identically its anchors are spelled.
            windowed("a.cs", "Board.Update", 1, 0),
        ];
        let picked = resolved("Update", &rows);
        assert_eq!(picked.len(), 2, "{picked:?}");
        assert!(picked.iter().all(|id| id.contains("#w0")), "{picked:?}");
    }

    #[test]
    fn two_unsplit_overloads_are_two_definitions() {
        let rows = [
            code("Parser.cs", "Parser.Parse", "method", 10),
            code("Parser.cs", "Parser.Parse", "method", 90),
        ];
        assert_eq!(resolved("Parse", &rows).len(), 2);
    }

    #[test]
    fn a_definition_the_ranking_already_returned_is_not_followed() {
        let rows = [code("a.cs", "Board.Update", "method", 10)];
        let ranked: HashSet<&str> = ["a.cs:Board.Update:10"].into_iter().collect();
        assert!(definitions(&reference("Update"), &rows, &ranked).is_empty());
    }

    #[test]
    fn an_unknown_name_and_a_non_code_row_resolve_to_nothing() {
        let mut section = code("a.md", "unused", "method", 1);
        section.kind = ChunkKind::Section {
            heading_path: vec!["Update".into()],
            window: None,
        };
        assert!(resolved("Update", &[section]).is_empty());
        assert!(resolved("Nowhere", &[code("a.cs", "Board.Update", "method", 1)]).is_empty());
    }

    // -- picking ------------------------------------------------------------

    fn picks(
        references: &[Reference],
        rows: &[AnchorCandidate],
        hits: &[SearchResult],
    ) -> Vec<Pick> {
        let names: Vec<String> = references
            .iter()
            .map(|reference| reference.name().to_string())
            .collect();
        pick(references, &names, rows, hits)
    }

    #[test]
    fn a_name_with_too_many_definitions_is_skipped_in_silence() {
        let rows: Vec<AnchorCandidate> = (0..FOLLOW_AMBIGUITY_CAP + 1)
            .map(|i| code(&format!("f{i}.cs"), "Thing.Update", "method", 10))
            .collect();
        assert!(picks(&[reference("Update")], &rows, &[doc("x")]).is_empty());

        // One under the cap: two are shown, and each says which of how many.
        let rows: Vec<AnchorCandidate> = (0..3)
            .map(|i| code(&format!("f{i}.cs"), "Thing.Update", "method", 10))
            .collect();
        let picked = picks(&[reference("Update")], &rows, &[doc("x")]);
        assert_eq!(picked.len(), FOLLOW_MAX_DEFS_PER_SYMBOL);
        assert_eq!(picked[0].note.as_deref(), Some("1 of 3 definitions"));
        assert_eq!(picked[1].note.as_deref(), Some("2 of 3 definitions"));
        // A name that resolves once has nothing to disclose.
        let single = picks(
            &[reference("Update")],
            &[code("f.cs", "Thing.Update", "method", 10)],
            &[doc("x")],
        );
        assert_eq!(single[0].note, None);
    }

    #[test]
    fn the_per_hit_and_per_bundle_caps_hold() {
        let references: Vec<Reference> = (0..10)
            .map(|i| Reference {
                origin: i / 5,
                ..reference(&format!("Thing.Update{i}"))
            })
            .collect();
        let rows: Vec<AnchorCandidate> = (0..10)
            .map(|i| {
                code(
                    &format!("f{i}.cs"),
                    &format!("Thing.Update{i}"),
                    "method",
                    1,
                )
            })
            .collect();
        let hits = [doc("a"), doc("b")];
        let picked = picks(&references, &rows, &hits);
        // Three per referring hit, over two hits, under the six-per-bundle cap.
        assert_eq!(picked.len(), 2 * FOLLOW_MAX_PER_HIT);
        assert_eq!(picked.iter().filter(|p| p.origin == 0).count(), 3);
        assert_eq!(picked.iter().filter(|p| p.origin == 1).count(), 3);
    }

    #[test]
    fn the_via_names_the_span_that_referred_and_the_reference_it_wrote() {
        let mut referrer = doc("x");
        referrer.path = r"docs\guide.md".into();
        referrer.line_start = 12;
        referrer.line_end = 40;
        let picked = picks(
            &[reference("AgentThread.RunAsync")],
            &[code(
                "src/Thread.cs",
                "Agents.AgentThread.RunAsync",
                "method",
                88,
            )],
            &[referrer],
        );
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].via.path, "docs/guide.md", "slashes normalized");
        assert_eq!((picked[0].via.line_start, picked[0].via.line_end), (12, 40));
        assert_eq!(picked[0].via.symbol, "AgentThread.RunAsync");
    }

    #[test]
    fn one_definition_is_picked_once_however_many_docs_name_it() {
        let references = [
            Reference {
                origin: 0,
                ..reference("Board.Update")
            },
            Reference {
                origin: 1,
                ..reference("Update")
            },
        ];
        let rows = [code("a.cs", "Board.Update", "method", 10)];
        assert_eq!(picks(&references, &rows, &[doc("a"), doc("b")]).len(), 1);
    }

    #[test]
    fn a_name_past_the_term_cap_is_never_matched() {
        // `pick` is told which names actually reached the statement; one that
        // did not must not quietly match a row fetched for a different name.
        let rows = [code("a.cs", "Board.Update", "method", 10)];
        assert!(pick(&[reference("Update")], &[], &rows, &[doc("x")]).is_empty());
    }
}
