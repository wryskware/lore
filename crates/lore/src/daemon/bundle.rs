//! `POST /v1/bundle` — a query in, a finished evidence bundle out.
//!
//! `search` hands an agent pointers and expects it to do the rest: open the
//! files, judge whether the hits answer the question, decide when to stop. This
//! route does that work here instead, and hands back a small block of verified,
//! line-numbered source with an honest verdict on top. It is a **port of the
//! validated bench prototype** (`bench/rcb/sandbox/lore_pkg.py`, whose constants
//! were calibrated over 20 judged cells); the pipeline, the thresholds and the
//! two integrity traps below are that prototype's, not new inventions.
//!
//! Assembly is daemon-side because only the daemon owns index state *and* the
//! corpus on disk (D-0003). A client assembling its own bundle would be
//! re-reading files the indexer is writing, and asserting a verification it is
//! not in a position to perform.
//!
//! Pipeline: search → verify → widen → merge → budget → verdict.
//!
//! Three properties are the point of it, and each is load-bearing:
//!
//! 1. **The text comes from disk, never from the index.** A hit gives a path
//!    and a 1-based inclusive span, so nothing is guessed: the path resolves
//!    through the project's declared extent and is then checked, realpath
//!    against realpath, to be inside the source root that claimed it; the span
//!    is checked against the file's real line count; the rendered lines are read
//!    at render time. What the agent is told to trust is therefore what is in
//!    the working tree, by construction. The stored excerpt is still *compared*
//!    against those lines, because a stale pointer is a different failure from a
//!    bad one — a span whose excerpt no longer matches is demoted to further
//!    reading as `stale` rather than shown under a claim of verification.
//!
//! 2. **The verdict is not computed from the score.** Fusion is RRF, so a score
//!    is `Σ 1/(60 + rank)`: a pure function of rank, carrying no corpus
//!    statistics and no similarity magnitude, which means it does not fall when
//!    a query matches nothing. Measured on the bench corpus, a nonsense query's
//!    top hit outscored the *second* hit of a query the corpus answers well, so
//!    any score threshold manufactures a confident `found` on an empty result.
//!    The verdict measures **term coverage** instead — how many of the query's
//!    content words appear in what came back — which is a claim about the
//!    returned evidence rather than about the retriever's confidence. Uncovered
//!    terms are always named, which is how a multi-part query reports that one
//!    part found nothing.
//!
//! 3. **Spans are widened and merged before budgeting.** A three-line Markdown
//!    chunk is a pointer wearing a span's clothes. Anything under
//!    [`MIN_SPAN_LINES`] is widened against the file on disk, and same-file
//!    spans that touch are merged, so the answer is a few readable blocks rather
//!    than twenty fragments of one README.
//!
//! Nothing here judges relevance and no model is called; the order search
//! ranked in is preserved throughout.
//!
//! # Followed definitions
//!
//! [`super::follow`] may hand over definitions the ranking did not return,
//! because a doc or sample near the top of it *named* them. They are the one
//! thing in a bundle that is not ranked: each is placed immediately after the
//! span that referred to it — so the bundle reads signpost → implementation —
//! and labelled `via <that span>`, which makes them interleaved by provenance
//! rather than by score. Three fences keep them from changing what a bundle
//! means:
//!
//! - **Strictly additive.** Ranked spans are widened, merged and budgeted
//!   exactly as they are with following off; a definition is never merged into
//!   one, and one that lands on top of a ranked span is dropped rather than
//!   shown twice. Nothing that would have rendered loses its slot.
//! - **Paid for separately.** Follow-ins come out of an allowance *on top of*
//!   `budget_tokens` ([`super::follow::FOLLOW_BUDGET_SHARE`]), disclosed in the
//!   header, so the caller can see the extra tokens it did not ask for.
//! - **Never evidence.** Coverage, and therefore the verdict, is computed from
//!   the ranked spans alone. The thresholds below were calibrated on twenty
//!   judged cells with no follow-ins in them, and a bundle that pulls in extra
//!   text and then grades itself on that text can talk a `none` into a `weak`.
//!
//! # The two normalization traps
//!
//! Both were found by the prototype and both are requirements, not tidiness:
//! the code chunker **dedents** what it stores (so the excerpt/disk comparison
//! is whitespace-insensitive while the rendered text keeps the file's real
//! indentation), and the corpus contains BOM'd files (so the mark is stripped
//! before either comparison or rendering). Getting either wrong marked half the
//! hits on a real query stale.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use lore_core::{
    BundleDropped, BundleResponse, BundleSpan, BundleSpanRef, BundleVia, SearchResponse,
    SearchResult,
};

use crate::sources::Sources;

use super::follow::{FOLLOW_BUDGET_SHARE, Followed};
use super::paths;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------
//
// Constants, not configuration. They were calibrated against one corpus and one
// agent population, so exposing them in `.lore.toml` before a second corpus has
// been measured would publish a knob nobody can set correctly — that decision
// waits for the evidence.

/// Rendered source a bundle may carry, when the caller does not say.
pub const DEFAULT_BUDGET_TOKENS: u32 = 4000;

/// The usual chars/4 approximation. Good enough to budget with; the bundle is
/// bounded by whole spans either way.
pub const CHARS_PER_TOKEN: usize = 4;

/// Ranked chunks considered, when the caller does not say. Well above what the
/// budget can render: widening and merging collapse several hits into one span,
/// and the surplus becomes further reading rather than being thrown away.
pub const DEFAULT_LIMIT: u32 = 24;

/// A chunk shorter than this is widened; [`EXPAND_TO_LINES`] is the target
/// width afterwards. Both in lines.
pub const MIN_SPAN_LINES: u32 = 16;
pub const EXPAND_TO_LINES: u32 = 28;

/// Same-file spans separated by at most this many lines merge into one, up to
/// [`MERGE_CAP_LINES`] so one busy file cannot eat the budget.
pub const MERGE_GAP: i64 = 4;
pub const MERGE_CAP_LINES: u32 = 140;

/// A single span longer than this is not truncated — it goes to further
/// reading. Half a block is worse than a pointer, because the reader cannot
/// tell which half arrived.
pub const MAX_SPAN_LINES: u32 = 160;

/// Verdict cuts on term coverage.
///
/// Calibrated twice on the bench corpus, and the second pass is the one that
/// counts. Ten hand-written queries scored 0.94–1.00 (real), 0.50
/// (half-answerable) and 0.33–0.44 (nonsense), which put the cuts at 0.75 and
/// 0.45. Then the ten briefs the probe *recorded* — the agent writes an
/// issue-style brief, not the task question — scored 0.72–0.93, a population
/// 0.75 sat inside and split arbitrarily. So `found` moved to 0.65: below every
/// recorded brief and still 0.15 clear of the half-answerable ceiling, the same
/// margin the first pass left.
pub const COVERAGE_FOUND: f64 = 0.65;
pub const COVERAGE_WEAK: f64 = 0.45;

/// A `none` bundle still shows its closest matches — the reader can judge what
/// the index thought was nearest — but not at full price. Spending the whole
/// budget on evidence the bundle has just disclaimed is the waste this route
/// exists to remove.
pub const NONE_BUDGET_TOKENS: u32 = 1200;

/// How many distinct paths a `DROPPED` line names before it stops being an
/// error line and starts being a listing.
const MAX_DROPPED_PATHS: usize = 8;

/// How many pointers `FURTHER READING` carries. Past this the tail is noise.
const MAX_FURTHER_READING: usize = 20;

/// Words that carry no claim about what a repository contains: ordinary English
/// glue, plus the vocabulary of a *retrieval brief* ("identify", "exact
/// evidence", "show the source locations", "any usage examples"). The agent
/// does not type the task question in; it writes instructions to the retriever,
/// and counting those as uncovered terms measures its phrasing rather than the
/// retrieval — worth 0.15–0.25 of coverage per call on the recorded briefs, and
/// worth real file reads afterwards, because an uncovered term is printed and
/// the reader is told to go find whatever it names.
///
/// Words a repository plausibly contains are deliberately absent even when a
/// brief also uses them as meta-language: `implementation`, `documentation`,
/// `public`, `behavior` and `answer` stay countable, because a corpus that
/// genuinely never mentions them is telling the reader something.
///
/// Sorted, because lookup is a binary search (`stopword_table_is_sorted`).
const STOPWORDS: &[&str] = &[
    "about",
    "after",
    "all",
    "along",
    "also",
    "and",
    "and/or",
    "another",
    "any",
    "anything",
    "apis",
    "app",
    "apps",
    "are",
    "aspect",
    "aspects",
    "back",
    "been",
    "before",
    "being",
    "both",
    "but",
    "call",
    "calls",
    "can",
    "caveat",
    "caveats",
    "cite",
    "cites",
    "citing",
    "code",
    "concerning",
    "concrete",
    "could",
    "cover",
    "covering",
    "covers",
    "describe",
    "describes",
    "detail",
    "details",
    "did",
    "does",
    "doesn",
    "doing",
    "done",
    "during",
    "each",
    "etc",
    "everything",
    "evidence",
    "exact",
    "exactly",
    "example",
    "examples",
    "explain",
    "explains",
    "find",
    "focus",
    "focusing",
    "for",
    "from",
    "get",
    "give",
    "got",
    "had",
    "has",
    "have",
    "help",
    "here",
    "how",
    "identify",
    "identifying",
    "include",
    "includes",
    "including",
    "into",
    "it",
    "its",
    "just",
    "keep",
    "keeps",
    "know",
    "later",
    "like",
    "limitations",
    "location",
    "locations",
    "long",
    "look",
    "made",
    "make",
    "many",
    "may",
    "mid",
    "might",
    "missing",
    "more",
    "most",
    "much",
    "must",
    "need",
    "needs",
    "new",
    "not",
    "note",
    "notes",
    "numbered",
    "off",
    "old",
    "one",
    "only",
    "onto",
    "other",
    "others",
    "our",
    "out",
    "over",
    "overview",
    "own",
    "part",
    "particular",
    "particularly",
    "parts",
    "please",
    "plus",
    "point",
    "points",
    "project",
    "provide",
    "provides",
    "question",
    "quote",
    "quotes",
    "quoting",
    "regarding",
    "relevant",
    "repo",
    "repository",
    "run",
    "runs",
    "same",
    "section",
    "sections",
    "see",
    "set",
    "sets",
    "several",
    "shall",
    "should",
    "show",
    "showing",
    "shows",
    "snippet",
    "snippets",
    "some",
    "something",
    "source",
    "sources",
    "span",
    "spans",
    "specifically",
    "start",
    "starts",
    "step",
    "steps",
    "still",
    "such",
    "summarize",
    "summary",
    "take",
    "task",
    "tell",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "think",
    "this",
    "those",
    "trace",
    "tracing",
    "two",
    "under",
    "usage",
    "use",
    "used",
    "using",
    "via",
    "want",
    "was",
    "way",
    "ways",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "within",
    "without",
    "work",
    "works",
    "would",
    "you",
    "your",
    "yourself",
];

/// `pub(crate)` for [`super::follow`], which applies the same floor to the
/// identifiers it extracts from a doc — one table, so `run` and `get` cannot be
/// glue on one side of the module boundary and a symbol worth chasing on the
/// other.
pub(crate) fn is_stopword(word: &str) -> bool {
    STOPWORDS.binary_search(&word).is_ok()
}

// ---------------------------------------------------------------------------
// Query terms and coverage
// ---------------------------------------------------------------------------

/// Content words of the query: lowercased, deduplicated, order preserved.
///
/// `snake_case` and `CamelCase` are split *as well as* kept whole, because the
/// agent writes prose but reaches for an identifier when it knows one, and both
/// halves of `CheckpointStorage` are worth asking about.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut add = |word: &str| {
        let word = word.to_ascii_lowercase();
        if word.len() < 3 || is_stopword(&word) || !seen.insert(word.clone()) {
            return;
        }
        terms.push(word);
    };

    for raw in words(query) {
        add(raw);
        // Underscores separate first, then each piece is split on case, which
        // is what makes `parse_HTTPHeader` yield `parse`, `HTTP` and `Header`.
        let parts: Vec<&str> = raw.split('_').flat_map(case_parts).collect();
        if parts.len() > 1 {
            for part in parts {
                add(part);
            }
        }
    }
    terms
}

/// `[A-Za-z][A-Za-z0-9_]*` over `text` — ASCII-only, deliberately: an
/// identifier is ASCII, and prose words that are not still tokenize on the
/// ASCII runs inside them.
fn words(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        out.push(&text[start..i]);
    }
    out
}

/// One `CamelCase`/`ACRONYMWord` chunk split into its case runs.
///
/// The rule is the prototype's regex `[A-Z]+(?![a-z]) | [A-Z][a-z0-9]* |
/// [a-z0-9]+`: a run of capitals is its own word except for the last capital
/// when a lowercase letter follows it, which belongs to the word starting
/// there. `HTTPServer` is therefore `HTTP` + `Server`, not `HTTPS` + `erver`.
///
/// `pub(crate)` for [`super::follow`]'s specificity rule ("is this token
/// multi-part, or one ordinary word?"). A second splitter would be a second
/// answer to the same question.
pub(crate) fn case_parts(chunk: &str) -> Vec<&str> {
    let bytes = chunk.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i].is_ascii_uppercase() {
            let mut run = i;
            while run < bytes.len() && bytes[run].is_ascii_uppercase() {
                run += 1;
            }
            let followed_by_lower = run < bytes.len() && bytes[run].is_ascii_lowercase();
            if followed_by_lower && run - 1 > start {
                // The acronym, minus the capital that starts the next word.
                i = run - 1;
            } else if followed_by_lower {
                // A single capital before lowercase: one ordinary word.
                i = run;
                while i < bytes.len()
                    && (bytes[i].is_ascii_lowercase() || bytes[i].is_ascii_digit())
                {
                    i += 1;
                }
            } else {
                i = run;
            }
        } else if bytes[i].is_ascii_lowercase() || bytes[i].is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_lowercase() || bytes[i].is_ascii_digit()) {
                i += 1;
            }
        } else {
            i += 1;
            continue;
        }
        out.push(&chunk[start..i]);
    }
    out
}

/// The term, then a few suffix-stripped prefixes of it.
///
/// Not a stemmer: enough that `checkpointing` matches `checkpoint` and `agents`
/// matches `agent`. Never shorter than four characters, because three-character
/// prefixes match everything.
fn stems(term: &str) -> Vec<&str> {
    let mut out = vec![term];
    for cut in 1..=3 {
        if term.len() >= 4 + cut {
            out.push(&term[..term.len() - cut]);
        }
    }
    out
}

/// Split `terms` into (covered, uncovered) against an already-lowercased
/// haystack.
pub fn coverage(terms: &[String], blob: &str) -> (Vec<String>, Vec<String>) {
    let mut covered = Vec::new();
    let mut uncovered = Vec::new();
    for term in terms {
        if stems(term).iter().any(|stem| blob.contains(stem)) {
            covered.push(term.clone());
        } else {
            uncovered.push(term.clone());
        }
    }
    (covered, uncovered)
}

// ---------------------------------------------------------------------------
// Verification against the corpus on disk
// ---------------------------------------------------------------------------

/// A hit that survived verification, carrying everything rendering needs.
#[derive(Debug, Clone, PartialEq)]
struct Span {
    /// Logical (project-relative) path, forward slashes.
    path: String,
    /// The physical file the logical path resolved to.
    full: Utf8PathBuf,
    start: u32,
    end: u32,
    file_lines: u32,
    label: String,
    score: f64,
    chunk_id: String,
    /// Ranked hits folded into this span, 1 before any merge.
    merged: u32,
    /// Rank index of the hit this span came from — for a follow-in, of the hit
    /// that *named* it. Merging keeps the lowest, so the value stays the
    /// span's own position in the ranking and placement can use it directly.
    origin: usize,
    /// `Some` exactly on a followed definition: the span that named it.
    via: Option<BundleVia>,
    /// `1 of 3 definitions`, printed beside the `via` when the name was
    /// ambiguous. Not on the wire — ambiguity is a rendering disclosure, and
    /// [`BundleVia`] is a pointer.
    via_note: Option<String>,
}

impl Span {
    fn width(&self) -> u32 {
        self.end - self.start + 1
    }
}

/// Why a hit did not become a span. All four are mechanical, and each is
/// counted separately so the header can say which failure happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The path does not resolve to a file inside the project's extent.
    Missing,
    /// It does, and cannot be read.
    Unreadable,
    /// The line span is not 1-based, ordered and inside the file.
    Range,
    /// The span is valid but the index excerpt is not what is there now.
    Stale,
}

impl Refusal {
    fn as_str(self) -> &'static str {
        match self {
            Refusal::Missing => "missing",
            Refusal::Unreadable => "unreadable",
            Refusal::Range => "range",
            Refusal::Stale => "stale",
        }
    }
}

/// The file's lines, with any byte-order mark removed.
///
/// The BOM matters twice. Lore strips it before chunking, so leaving it in
/// makes the first line of every BOM'd file compare unequal to the index and
/// the whole chunk look stale — a real hit was lost to exactly that. And
/// dropping it here also keeps the mark out of the rendered line 1.
///
/// Invalid UTF-8 is replaced rather than refused, matching how the file would
/// have been read anywhere else in this pipeline; a file that is not text at
/// all fails the excerpt comparison instead.
fn read_lines(path: &Utf8Path) -> Option<Vec<String>> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let body = text.strip_prefix('\u{feff}').unwrap_or(&*text);
    Some(body.lines().map(str::to_string).collect())
}

/// Logical path → the physical file it names, or `None` when it escapes.
///
/// Two checks, not one: the project's declared extent decides which root owns
/// the path at all, and then realpath-against-realpath decides whether the
/// result is still inside that root. The second is what stops a `..` in an
/// indexed path, or a link out of the tree, from becoming a file the bundle
/// quotes. This is the only place a path from the index becomes a path on disk.
fn resolve(sources: &Sources, logical: &str) -> Option<Utf8PathBuf> {
    let logical = logical.replace('\\', "/");
    let logical = logical.trim_start_matches('/');
    if logical.is_empty() {
        return None;
    }
    let (source, joined) = sources.resolve(Utf8Path::new(logical))?;
    let full = paths::canonicalize_root(joined.as_std_path()).ok()?;
    if !paths::is_within(&source.root, &full) || !full.is_file() {
        return None;
    }
    Some(full)
}

/// Collapse whitespace for the excerpt/disk comparison.
///
/// Indentation is deliberately not compared. The code chunker **dedents** what
/// it stores — a method chunk begins `def foo(` where the file has
/// `    def foo(` — so a whitespace-exact comparison calls every indented chunk
/// stale, which measured 12 of 24 hits on one real query and would have thrown
/// away the best evidence in the bundle. Stripping each line and dropping
/// blanks still catches "these lines are now different code", which is the only
/// thing this check is for; the rendered text keeps the file's real indentation
/// regardless, because it is read from disk.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

/// One hit → a checked span, or the reason it was refused.
fn verify(hit: &SearchResult, sources: &Sources) -> Result<Span, (Refusal, String)> {
    let path = hit.path.replace('\\', "/");
    let Some(full) = resolve(sources, &path) else {
        return Err((Refusal::Missing, path));
    };
    let Some(lines) = read_lines(&full) else {
        return Err((Refusal::Unreadable, path));
    };

    let file_lines = lines.len() as u32;
    let (start, end) = (hit.line_start, hit.line_end);
    if !(start >= 1 && start <= end && end <= file_lines) {
        return Err((Refusal::Range, path));
    }

    // A truncated excerpt is not evidence of anything: it would differ from the
    // file by construction. Those hits are trusted on the range check alone.
    if !hit.excerpt.is_empty() && !hit.excerpt_truncated {
        let disk = lines[(start - 1) as usize..end as usize].join("\n");
        if normalize(&hit.excerpt) != normalize(&disk) {
            return Err((Refusal::Stale, format!("{path}:{start}-{end}")));
        }
    }

    Ok(Span {
        path,
        full,
        start,
        end,
        file_lines,
        label: label(hit),
        score: hit.score,
        chunk_id: hit.chunk_id.clone(),
        merged: 1,
        origin: 0,
        via: None,
        via_note: None,
    })
}

/// What the span is, as the index itself named it — never as this module
/// guessed.
///
/// A code chunk carries a dotted symbol path; a Markdown chunk carries its
/// root-to-leaf heading trail. Either is printed verbatim and neither is
/// synthesized, so the header cannot assert a symbol the file does not have.
/// `#s0`-style synthetic ids (the chunker's name for "a slice with no symbol")
/// are dropped rather than shown, because they name nothing a reader can use.
fn label(hit: &SearchResult) -> String {
    if let Some(symbol) = hit.symbol_path.as_deref().map(str::trim)
        && !symbol.is_empty()
        && !symbol.starts_with('#')
    {
        return symbol.to_string();
    }
    hit.heading_path
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|heading| !heading.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" > ")
}

// ---------------------------------------------------------------------------
// Widening and merging
// ---------------------------------------------------------------------------

/// Grow every span narrower than [`MIN_SPAN_LINES`] toward
/// [`EXPAND_TO_LINES`], in place. Returns how many actually moved.
///
/// Done straight against the file rather than through `POST /v1/expand`, which
/// is what the prototype had to do from outside the daemon. In here the line
/// count is already known from verification and the arithmetic is `expand`'s
/// own, so the round trip would buy nothing but latency (measured at ~0.6 s of
/// the prototype's ~1.5 s assembly) and one failure mode — a chunk-id prefix
/// resolving in a different file — that not making the call removes entirely.
fn widen(spans: &mut [Span]) -> u32 {
    let mut widened = 0;
    for span in spans.iter_mut() {
        let width = span.width();
        if width >= MIN_SPAN_LINES {
            continue;
        }
        let context = EXPAND_TO_LINES.saturating_sub(width).div_ceil(2).max(1);
        let start = span.start.saturating_sub(context).max(1);
        let end = span.end.saturating_add(context).min(span.file_lines);
        if (start, end) != (span.start, span.end) {
            span.start = start;
            span.end = end;
            widened += 1;
        }
    }
    widened
}

/// Fold same-file spans that touch or nearly touch, keeping rank order.
///
/// The merged span inherits the position, label and chunk id of its
/// highest-ranked member, so the bundle still reads best-first and the id it
/// prints is one the reader would have been given anyway.
fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    'next: for span in spans {
        for existing in out.iter_mut() {
            if existing.path != span.path {
                continue;
            }
            let low = existing.start.min(span.start);
            let high = existing.end.max(span.end);
            // Signed on purpose: overlapping spans give a negative gap, which
            // is exactly the case that should always merge.
            let gap = (span.start as i64 - existing.end as i64)
                .max(existing.start as i64 - span.end as i64);
            if gap > MERGE_GAP || (high - low + 1) > MERGE_CAP_LINES {
                continue;
            }
            existing.start = low;
            existing.end = high;
            existing.merged += 1;
            // The earlier member is the higher-ranked one, so the merged span
            // keeps its position: placement reads `origin` to decide where a
            // followed definition goes, and it must be the rank the reader
            // sees this block at.
            existing.origin = existing.origin.min(span.origin);
            continue 'next;
        }
        out.push(span);
    }
    out
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The span's real lines, numbered, under a `path:start-end [label]` header —
/// plus, on a followed definition, ` (via <the span that named it>)`.
///
/// The `via` is the honesty requirement: a reader must be able to tell a span
/// the query ranked from one lore chose to add, and be told exactly why it was
/// added, without parsing the JSON.
///
/// Read from disk *again*, at render time. `None` when the file became
/// unreadable or shrank since verification — the one case where a span that
/// passed verification must still not be printed.
fn render_span(span: &Span) -> Option<String> {
    let lines = read_lines(&span.full)?;
    if span.end as usize > lines.len() {
        return None;
    }
    let width = span.end.to_string().len();
    let mut out = String::new();
    out.push_str("=== ");
    out.push_str(&span.path);
    out.push_str(&format!(":{}-{}", span.start, span.end));
    if !span.label.is_empty() {
        out.push_str(&format!(" [{}]", span.label));
    }
    if let Some(via) = &span.via {
        out.push_str(&format!(
            " (via {}:{}-{}",
            via.path, via.line_start, via.line_end
        ));
        if let Some(note) = &span.via_note {
            out.push_str(&format!(", {note}"));
        }
        out.push(')');
    }
    out.push_str(" ===");
    for (offset, line) in lines[(span.start - 1) as usize..span.end as usize]
        .iter()
        .enumerate()
    {
        out.push_str(&format!(
            "\n{:>width$}  {line}",
            span.start as usize + offset
        ));
    }
    Some(out)
}

/// Search → verify → widen → merge → budget → verdict, over results the caller
/// has already fetched.
///
/// Split from the HTTP handler so the store lock is not held across the file
/// reads, and so every rule above is testable against a temporary directory
/// with no daemon in the picture.
pub fn assemble(
    query: &str,
    results: &SearchResponse,
    followed: &[Followed],
    sources: &Sources,
    budget_tokens: u32,
    limit: u32,
) -> BundleResponse {
    let mut good: Vec<Span> = Vec::new();
    // Ordered so the `DROPPED` lines come out in a stable order rather than a
    // hash order that changes between runs.
    let mut refused: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // The prototype names a nameless hit `?` rather than printing a `DROPPED`
    // line that ends at the colon.
    let named = |where_: String| {
        if where_.is_empty() {
            "?".into()
        } else {
            where_
        }
    };
    for (rank, hit) in results.results.iter().enumerate() {
        match verify(hit, sources) {
            Ok(mut span) => {
                span.origin = rank;
                good.push(span);
            }
            Err((reason, where_)) => refused
                .entry(reason.as_str().to_string())
                .or_default()
                .push(named(where_)),
        }
    }

    // A followed definition goes through the very same `verify`: the bundle's
    // core guarantee — rendered text came from disk — applies to it with no new
    // code. Its refusals are tallied under a `follow:`-prefixed reason so the
    // ranked-hit DROPPED tally stays readable.
    let mut follow_verified: Vec<Span> = Vec::new();
    let mut followed_dropped = 0u32;
    for definition in followed {
        match verify(&definition.hit, sources) {
            Ok(mut span) => {
                span.origin = definition.origin;
                span.via = Some(definition.via.clone());
                span.via_note = definition.note.clone();
                follow_verified.push(span);
            }
            Err((reason, where_)) => {
                followed_dropped += 1;
                refused
                    .entry(format!("follow:{}", reason.as_str()))
                    .or_default()
                    .push(named(where_));
            }
        }
    }

    let top_score = good.first().map(|span| round6(span.score));
    let hits_verified = good.len() as u32;
    let spans_widened = widen(&mut good);
    let merged = merge(good);
    let spans_after_merge = merged.len() as u32;

    // Oversized spans are pointers, not evidence; they never enter the budget.
    let (spans, oversized): (Vec<Span>, Vec<Span>) = merged
        .into_iter()
        .partition(|span| span.width() <= MAX_SPAN_LINES);
    let spans_oversized = oversized.len() as u32;

    // Follow-ins widen and merge among *themselves*. Merging one into a ranked
    // span would move a block that would have rendered anyway, and strict
    // additivity is the guarantee this feature is fenced by; merging them with
    // each other is still wanted, because two windows of one split definition
    // should arrive as one readable block.
    widen(&mut follow_verified);
    let (follow_spans, follow_oversized): (Vec<Span>, Vec<Span>) = merge(follow_verified)
        .into_iter()
        // A definition that lands on top of a ranked span is dropped outright:
        // the highest-value case (a doc next to its own implementation) is
        // exactly the one that would otherwise render twice.
        .filter(|definition| {
            !spans
                .iter()
                .chain(oversized.iter())
                .any(|span| overlaps(span, definition))
        })
        .partition(|span| span.width() <= MAX_SPAN_LINES);

    let budget_chars = budget_tokens as usize * CHARS_PER_TOKEN;
    let mut rendered: Vec<(Span, String)> = Vec::new();
    let mut overflow: Vec<Span> = oversized.clone();
    let mut used = 0usize;
    for span in spans {
        let Some(block) = render_span(&span) else {
            refused
                .entry(Refusal::Unreadable.as_str().to_string())
                .or_default()
                .push(span.path.clone());
            continue;
        };
        let cost = block.chars().count() + 2;
        if !rendered.is_empty() && used + cost > budget_chars {
            overflow.push(span);
            continue;
        }
        used += cost;
        rendered.push((span, block));
    }

    // Coverage is measured on what was RENDERED, plus the paths of everything
    // that came back: a term that only appears in an overflowed path is
    // honestly "we found something, it did not fit", not "covered".
    //
    // **Followed definitions are deliberately absent from this blob, and from
    // everything computed out of it.** This is not an oversight to tidy up: the
    // 0.65/0.45 cuts were calibrated over twenty judged cells that contained no
    // follow-ins, and letting text lore chose to add count towards coverage
    // would let a bundle talk its own `none` into a `weak`. The verdict is a
    // claim about what the *retrieval* found.
    let mut blob = rendered
        .iter()
        .map(|(_, block)| block.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    blob.push('\n');
    for span in rendered.iter().map(|(span, _)| span).chain(overflow.iter()) {
        blob.push_str(&span.path.to_lowercase());
        blob.push('\n');
    }
    let terms = query_terms(query);
    let (covered, uncovered) = coverage(&terms, &blob);
    let ratio = if terms.is_empty() {
        1.0
    } else {
        covered.len() as f64 / terms.len() as f64
    };

    let files = rendered
        .iter()
        .map(|(span, _)| span.path.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let (verdict, detail) = if rendered.is_empty() {
        (
            "none",
            format!("nothing relevant found for: {}", clip(query.trim(), 200)),
        )
    } else if ratio >= COVERAGE_FOUND {
        (
            "found",
            format!("{} verified span(s) from {files} file(s)", rendered.len()),
        )
    } else if ratio >= COVERAGE_WEAK {
        (
            "weak",
            format!(
                "{} verified span(s), but only {} of {} query terms appear in them -- treat as \
                 partial, and find the rest yourself",
                rendered.len(),
                covered.len(),
                terms.len()
            ),
        )
    } else {
        // Spans are still shown, so the wording must not claim the bundle is
        // empty when it is not. What it claims is the true thing: nothing here
        // matches, and these are only the nearest misses.
        (
            "none",
            format!(
                "nothing relevant found for: {} -- only {} of {} query terms appear anywhere in \
                 what came back; the span(s) below are the index's closest matches and may be \
                 irrelevant",
                clip(query.trim(), 200),
                covered.len(),
                terms.len()
            ),
        )
    };

    if verdict == "none" && !rendered.is_empty() {
        // Trim, do not re-measure: coverage is a property of what the query
        // retrieved, not of how much of it survived the budget, and recomputing
        // it here could flip the verdict that caused the trim.
        let trimmed_budget = NONE_BUDGET_TOKENS as usize * CHARS_PER_TOKEN;
        let mut keep: Vec<(Span, String)> = Vec::new();
        let mut used = 0usize;
        for (span, block) in rendered {
            let cost = block.chars().count() + 2;
            if !keep.is_empty() && used + cost > trimmed_budget {
                overflow.push(span);
                continue;
            }
            used += cost;
            keep.push((span, block));
        }
        rendered = keep;
    }

    // Only now, with every ranked span placed, do the follow-ins get their
    // turn — out of a pot of their own, so a span that would have rendered
    // never loses its slot to a definition. A `none` bundle spends nothing on
    // them: it has just disclaimed its own evidence, and paying 35% more to
    // chase names out of disclaimed prose is the waste this route exists to
    // remove.
    let follow_budget = if verdict == "none" {
        0
    } else {
        (budget_tokens as f64 * FOLLOW_BUDGET_SHARE) as usize * CHARS_PER_TOKEN
    };
    let mut follow_rendered: Vec<(Span, String)> = Vec::new();
    let mut follow_overflow: Vec<Span> = Vec::new();
    let mut follow_used = 0usize;
    for mut span in follow_spans.into_iter().chain(follow_oversized) {
        // The printed pointer must name the block the reader can actually see
        // above, not the pre-widening hit that produced it.
        retarget_via(&mut span, &rendered);
        if span.width() > MAX_SPAN_LINES {
            follow_overflow.push(span);
            continue;
        }
        let Some(block) = render_span(&span) else {
            followed_dropped += 1;
            refused
                .entry(format!("follow:{}", Refusal::Unreadable.as_str()))
                .or_default()
                .push(span.path.clone());
            continue;
        };
        let cost = block.chars().count() + 2;
        // No "the first one always renders" exemption here, unlike the ranked
        // budget: a follow-in is a bonus, and the caller was promised the
        // allowance is a ceiling.
        if follow_used + cost > follow_budget {
            follow_overflow.push(span);
            continue;
        }
        follow_used += cost;
        follow_rendered.push((span, block));
    }
    let followed_rendered = follow_rendered.len() as u32;
    overflow.extend(follow_overflow);

    // Ranked spans keep their order exactly; each definition is placed
    // immediately after the ranked span that named it. `origin` is the rank
    // index either way and is non-decreasing across `rendered`, so a stable
    // sort on `(origin, is a follow-in)` is that placement — and a definition
    // whose referring span did not survive lands after the last one that did.
    let mut placed: Vec<(usize, bool, Span, String)> = rendered
        .into_iter()
        .map(|(span, block)| (span.origin, false, span, block))
        .chain(
            follow_rendered
                .into_iter()
                .map(|(span, block)| (span.origin, true, span, block)),
        )
        .collect();
    placed.sort_by_key(|(origin, is_follow, _, _)| (*origin, *is_follow));

    let mut parts: Vec<String> = vec![format!("VERDICT: {verdict} ({detail})")];
    if !uncovered.is_empty() {
        parts.push(format!("NO MATCH FOR: {}", uncovered.join(", ")));
    }
    if results.lexical_only {
        parts.push(
            "NOTE: the index answered without its vector arm (lexical-only degradation); recall \
             may be lower than usual."
                .to_string(),
        );
    }
    let dropped: Vec<BundleDropped> = refused
        .into_iter()
        .map(|(reason, where_)| {
            let count = where_.len() as u32;
            let mut paths: Vec<String> = where_;
            paths.sort();
            paths.dedup();
            paths.truncate(MAX_DROPPED_PATHS);
            parts.push(format!("DROPPED ({reason}, {count}): {}", paths.join(", ")));
            BundleDropped {
                reason: reason.to_string(),
                count,
                paths,
            }
        })
        .collect();

    // The extra tokens are named, not merely spent: the allowance sits on top
    // of the caller's budget, and a cost nobody can see is a cost nobody
    // agreed to.
    if followed_rendered > 0 {
        parts.push(format!(
            "FOLLOWED: {followed_rendered} definition(s) pulled in because a doc or sample above \
             names them, costing {} tokens on top of the {budget_tokens}-token budget.",
            follow_used.div_ceil(CHARS_PER_TOKEN)
        ));
    }

    parts.extend(placed.iter().map(|(_, _, _, block)| block.clone()));
    if !overflow.is_empty() {
        parts.push(format!(
            "FURTHER READING: {}",
            overflow
                .iter()
                .take(MAX_FURTHER_READING)
                .map(|span| format!("{}:{}-{}", span.path, span.start, span.end))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let text = parts.join("\n") + "\n";
    let chars = text.chars().count();

    BundleResponse {
        query: query.to_string(),
        verdict: verdict.to_string(),
        verdict_detail: detail,
        coverage: round3(ratio),
        terms,
        terms_covered: covered,
        terms_uncovered: uncovered,
        lexical_only: results.lexical_only,
        hits_returned: results.results.len() as u32,
        hits_verified,
        hits_rejected: dropped.iter().map(|drop| drop.count).sum(),
        dropped,
        spans: placed
            .iter()
            .map(|(_, _, span, _)| BundleSpan {
                path: span.path.clone(),
                line_start: span.start,
                line_end: span.end,
                label: Some(span.label.clone()).filter(|label| !label.is_empty()),
                merged: span.merged,
                chunk_id: span.chunk_id.clone(),
                via: span.via.clone(),
            })
            .collect(),
        further_reading: overflow
            .iter()
            .map(|span| BundleSpanRef {
                path: span.path.clone(),
                line_start: span.start,
                line_end: span.end,
                via: span.via.clone(),
            })
            .collect(),
        spans_widened,
        spans_after_merge,
        spans_oversized,
        top_score,
        bundle_chars: chars as u32,
        bundle_tokens_est: chars.div_ceil(CHARS_PER_TOKEN) as u32,
        budget_tokens,
        limit,
        followed: followed_rendered,
        followed_dropped,
        text,
    }
}

/// Do two spans name overlapping lines of one file?
fn overlaps(a: &Span, b: &Span) -> bool {
    a.path == b.path && a.start <= b.end && b.start <= a.end
}

/// Point a follow-in's `via` at the *rendered* block that named it.
///
/// The reference was found in a search hit, but what the reader sees above is
/// that hit after widening and merging. Printing the hit's own span would hand
/// back a pointer to lines the bundle never showed.
fn retarget_via(definition: &mut Span, rendered: &[(Span, String)]) {
    let Some(via) = &mut definition.via else {
        return;
    };
    let referrer = rendered.iter().map(|(span, _)| span).find(|span| {
        span.path == via.path && span.start <= via.line_end && via.line_start <= span.end
    });
    if let Some(span) = referrer {
        via.line_start = span.start;
        via.line_end = span.end;
    }
}

/// The first `max` characters, on a character boundary.
fn clip(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// Round-half-to-even on the scaled value, matching Python's `round()` — the
/// prototype's reported figures are banker's-rounded, and `f64::round` (half
/// away from zero) disagrees exactly when the scaled value is a binary-exact
/// half (1/16 -> 0.062 there, 0.063 here). Verdicts never depend on this; the
/// JSON fields should still not drift from the prototype.
fn round_ties_even(scaled: f64) -> f64 {
    let floor = scaled.floor();
    if scaled - floor == 0.5 {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        scaled.round()
    }
}

fn round3(value: f64) -> f64 {
    round_ties_even(value * 1_000.0) / 1_000.0
}

fn round6(value: f64) -> f64 {
    round_ties_even(value * 1_000_000.0) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every assertion below this line predates symbol following, and every
    /// one of them must keep meaning what it meant: a bundle with nothing
    /// followed. Shadowing the real [`super::assemble`] with the no-follow
    /// arity says that once, here, instead of `&[]` forty times — and the
    /// tests that *are* about following call `super::assemble` outright.
    fn assemble(
        query: &str,
        results: &SearchResponse,
        sources: &Sources,
        budget_tokens: u32,
        limit: u32,
    ) -> BundleResponse {
        super::assemble(query, results, &[], sources, budget_tokens, limit)
    }

    #[test]
    fn stopword_table_is_sorted_for_binary_search() {
        let mut sorted = STOPWORDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, STOPWORDS, "STOPWORDS must be sorted and unique");
    }

    #[test]
    fn identifiers_are_kept_whole_and_split_on_case_and_underscore() {
        assert_eq!(
            query_terms("CheckpointStorage and parse_HTTPHeader"),
            [
                "checkpointstorage",
                "checkpoint",
                "storage",
                "parse_httpheader",
                "parse",
                "http",
                "header",
            ]
        );
        // The acronym rule in isolation: the capital that starts the next word
        // is not part of the run before it.
        assert_eq!(case_parts("HTTPServer"), ["HTTP", "Server"]);
        assert_eq!(case_parts("ABC"), ["ABC"]);
        assert_eq!(case_parts("getURL"), ["get", "URL"]);
        assert_eq!(case_parts("Foo2Bar"), ["Foo2", "Bar"]);
    }

    #[test]
    fn brief_meta_vocabulary_is_not_counted_as_repository_vocabulary() {
        // Everything here except `retries` is retrieval-brief phrasing; a
        // corpus is not failing to answer because it never says "identify".
        assert_eq!(
            query_terms(
                "Identify the exact source locations and cite concrete evidence showing any \
                 usage examples of retries"
            ),
            ["retries"]
        );
        // ...but words the repository plausibly contains stay countable, even
        // where a brief also uses them as meta-language.
        assert_eq!(
            query_terms("public implementation behavior answer"),
            ["public", "implementation", "behavior", "answer"]
        );
    }

    #[test]
    fn stems_stop_at_four_characters_and_cover_short_suffixes() {
        assert_eq!(stems("agents"), ["agents", "agent", "agen"]);
        assert_eq!(
            stems("orchestrator"),
            ["orchestrator", "orchestrato", "orchestrat", "orchestra"]
        );
        // Nothing below four characters, because a three-character prefix
        // matches everything.
        assert_eq!(stems("span"), ["span"]);

        let terms = query_terms("agents checkpointing zzz");
        let (covered, uncovered) = coverage(&terms, "an agent that checkpoints");
        assert_eq!(covered, ["agents", "checkpointing"]);
        assert_eq!(uncovered, ["zzz"]);
    }

    #[test]
    fn verdict_cuts_sit_where_the_calibration_put_them() {
        // The cuts, exercised through the assembler rather than asserted as
        // constants: 3 of 4 terms is 0.75 (found), 2 of 4 is 0.50 (weak), 1 of
        // 4 is 0.25 (none, with the nearest miss still shown).
        let fixture = corpus(&[("a.txt", "alpha bravo charlie delta\n")]);
        let hits = [hit("a.txt", 1, 1, "alpha bravo charlie delta")];

        let found = assemble(
            "alpha bravo charlie zulu",
            &response(&hits),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(found.verdict, "found");
        assert_eq!(found.coverage, 0.75);
        assert!(
            found
                .text
                .starts_with("VERDICT: found (1 verified span(s) from 1 file(s))"),
            "{}",
            found.text
        );

        let weak = assemble(
            "alpha bravo yankee zulu",
            &response(&hits),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(weak.verdict, "weak");
        assert_eq!(weak.coverage, 0.5);
        assert!(
            weak.text.contains("NO MATCH FOR: yankee, zulu"),
            "{}",
            weak.text
        );

        let none = assemble(
            "alpha xray yankee zulu",
            &response(&hits),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(none.verdict, "none");
        assert_eq!(none.coverage, 0.25);
        // A `none` verdict still shows its nearest miss, and says so.
        assert_eq!(none.spans.len(), 1);
        assert!(none.text.contains("closest matches"), "{}", none.text);
    }

    #[test]
    fn a_dedented_excerpt_is_not_stale_and_renders_with_its_real_indentation() {
        // The chunker stores code dedented; the file has it indented. Comparing
        // naively marks the hit stale and throws away the best evidence there
        // is -- and the rendered block must still carry the file's own leading
        // whitespace.
        let fixture = corpus(&[("m.py", "class C:\n    def foo(self):\n        return 1\n")]);
        let bundle = assemble(
            "foo",
            &response(&[hit("m.py", 2, 3, "def foo(self):\n    return 1")]),
            &fixture.1,
            4000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
        assert_eq!(bundle.spans.len(), 1);
        assert!(
            bundle.text.contains("2      def foo(self):"),
            "{}",
            bundle.text
        );
    }

    #[test]
    fn a_bom_is_stripped_before_comparing_and_before_rendering() {
        let fixture = corpus(&[("bom.md", "\u{feff}# Title\nbody\n")]);
        let bundle = assemble(
            "title",
            &response(&[hit("bom.md", 1, 2, "# Title\nbody")]),
            &fixture.1,
            4000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
        assert!(bundle.text.contains("1  # Title"), "{}", bundle.text);
        assert!(
            !bundle.text.contains('\u{feff}'),
            "the mark reached the render"
        );
    }

    #[test]
    fn text_that_moved_is_stale_rather_than_quoted() {
        let fixture = corpus(&[("m.py", "one\ntwo\nthree\n")]);
        let bundle = assemble(
            "two",
            &response(&[hit("m.py", 1, 2, "completely different content")]),
            &fixture.1,
            4000,
            24,
        );
        assert!(bundle.spans.is_empty());
        assert_eq!(bundle.dropped.len(), 1);
        assert_eq!(bundle.dropped[0].reason, "stale");
        // The pointer survives with its span, which is the whole difference
        // between a stale hit and a bad one.
        assert!(
            bundle.text.contains("DROPPED (stale, 1): m.py:1-2"),
            "{}",
            bundle.text
        );
    }

    #[test]
    fn a_truncated_excerpt_is_trusted_on_its_range_alone() {
        // An excerpt clipped by `search` differs from the file by construction;
        // comparing it would call every long chunk stale.
        let fixture = corpus(&[("m.py", "one\ntwo\nthree\n")]);
        let mut clipped = hit("m.py", 1, 3, "one\ntw");
        clipped.excerpt_truncated = true;
        let bundle = assemble("one", &response(&[clipped]), &fixture.1, 4000, 24);
        assert_eq!(bundle.hits_verified, 1);
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
    }

    #[test]
    fn a_range_past_the_end_of_the_file_and_an_absent_path_are_refused() {
        let fixture = corpus(&[("m.py", "one\ntwo\n")]);
        let bundle = assemble(
            "one",
            &response(&[hit("m.py", 1, 99, ""), hit("gone.py", 1, 1, "")]),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(bundle.hits_verified, 0);
        assert_eq!(bundle.hits_rejected, 2);
        let reasons: Vec<&str> = bundle.dropped.iter().map(|d| d.reason.as_str()).collect();
        assert_eq!(reasons, ["missing", "range"]);
    }

    #[test]
    fn a_path_escaping_the_project_root_is_refused_even_though_the_file_exists() {
        // The containment check is realpath against realpath, so a `..` in an
        // indexed path cannot become a file the bundle quotes -- and the file
        // here genuinely exists, which is what makes the test worth having.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let outer = paths::canonicalize_root(dir.path()).expect("a canonical root");
        std::fs::write(outer.join("secret.env"), "TOKEN=1\n").expect("writing the outside file");
        let root = outer.join("proj");
        std::fs::create_dir(&root).expect("creating the project root");
        std::fs::write(root.join("a.txt"), "alpha\n").expect("writing a fixture file");
        let sources = Sources::from_declared(&root, &[]);

        let bundle = assemble(
            "token",
            &response(&[hit("../secret.env", 1, 1, "")]),
            &sources,
            4000,
            24,
        );
        assert_eq!(bundle.hits_verified, 0);
        assert_eq!(bundle.dropped[0].reason, "missing");
        assert!(!bundle.text.contains("TOKEN"), "{}", bundle.text);
    }

    #[test]
    fn short_spans_widen_and_touching_same_file_spans_merge() {
        let fixture = corpus(&[("big.txt", &numbered(200, ""))]);
        // Two three-line hits thirty lines apart: each widens by thirteen
        // lines, which brings them within MERGE_GAP, so one readable block
        // comes out rather than two fragments of the same file.
        let bundle = assemble(
            "line",
            &response(&[hit("big.txt", 60, 62, ""), hit("big.txt", 90, 92, "")]),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(bundle.spans_widened, 2);
        assert_eq!(bundle.spans_after_merge, 1);
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.spans[0].merged, 2);
        assert_eq!(
            (bundle.spans[0].line_start, bundle.spans[0].line_end),
            (47, 105)
        );
    }

    #[test]
    fn distant_spans_in_one_file_stay_separate() {
        let fixture = corpus(&[("big.txt", &numbered(400, ""))]);
        let bundle = assemble(
            "line",
            &response(&[hit("big.txt", 1, 40, ""), hit("big.txt", 300, 340, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.spans_after_merge, 2);
        assert_eq!(bundle.spans.len(), 2);
        assert!(bundle.spans.iter().all(|span| span.merged == 1));
    }

    #[test]
    fn merging_stops_at_the_cap_so_one_busy_file_cannot_eat_the_budget() {
        let fixture = corpus(&[("big.txt", &numbered(400, ""))]);
        // 1-100 and 102-201 touch, but merged they would span 201 lines --
        // past MERGE_CAP_LINES -- so they stay two spans.
        let bundle = assemble(
            "line",
            &response(&[hit("big.txt", 1, 100, ""), hit("big.txt", 102, 201, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.spans_after_merge, 2);
    }

    #[test]
    fn widening_clamps_to_the_file_rather_than_running_off_its_ends() {
        let fixture = corpus(&[("small.txt", "a\nb\nc\n")]);
        let bundle = assemble(
            "bbb",
            &response(&[hit("small.txt", 2, 2, "b")]),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(
            (bundle.spans[0].line_start, bundle.spans[0].line_end),
            (1, 3)
        );
    }

    #[test]
    fn an_oversized_span_becomes_a_pointer_instead_of_half_a_block() {
        let fixture = corpus(&[("huge.txt", &numbered(400, ""))]);
        let bundle = assemble(
            "line",
            &response(&[hit("huge.txt", 1, 300, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.spans_oversized, 1);
        assert!(bundle.spans.is_empty());
        assert_eq!(bundle.further_reading.len(), 1);
        assert!(
            bundle.text.contains("FURTHER READING: huge.txt:1-300"),
            "{}",
            bundle.text
        );
    }

    #[test]
    fn the_budget_demotes_whole_spans_and_never_truncates_one() {
        let body = numbered(400, "");
        let fixture = corpus(&[("a.txt", &body), ("b.txt", &body)]);
        // Room for one hundred-line block and nowhere near two.
        let bundle = assemble(
            "line",
            &response(&[hit("a.txt", 1, 100, ""), hit("b.txt", 1, 100, "")]),
            &fixture.1,
            400,
            24,
        );
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.spans[0].path, "a.txt");
        assert_eq!(bundle.further_reading.len(), 1);
        assert_eq!(bundle.further_reading[0].path, "b.txt");
        // The rendered block is whole: its last line is there, nothing elided.
        assert!(bundle.text.contains("100  line 100"), "{}", bundle.text);
        assert!(
            bundle.text.contains("FURTHER READING: b.txt:1-100"),
            "{}",
            bundle.text
        );
    }

    #[test]
    fn the_first_span_always_renders_even_under_an_impossible_budget() {
        // Otherwise a caller who set the budget too low gets a bundle that
        // claims nothing was found, which is a different (and false) statement
        // from "this did not fit".
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let bundle = assemble(
            "alpha",
            &response(&[hit("a.txt", 1, 1, "alpha")]),
            &fixture.1,
            0,
            24,
        );
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.verdict, "found");
    }

    #[test]
    fn a_none_verdict_is_trimmed_to_the_smaller_budget() {
        // Both spans fit the 4000-token budget and neither carries a query
        // term, so the verdict is `none` and the 1200-token trim keeps only the
        // first -- the rest is evidence the bundle has just disclaimed.
        let body = numbered(400, " padding padding padding");
        let fixture = corpus(&[("a.txt", &body), ("b.txt", &body)]);
        let bundle = assemble(
            "quantum chromodynamics lattice gauge",
            &response(&[hit("a.txt", 1, 150, ""), hit("b.txt", 1, 150, "")]),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(bundle.verdict, "none");
        assert_eq!(bundle.coverage, 0.0);
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.further_reading.len(), 1);
        assert!(
            bundle.bundle_tokens_est < 1500,
            "{}",
            bundle.bundle_tokens_est
        );
    }

    #[test]
    fn lexical_only_degradation_is_reported_in_the_header() {
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let mut results = response(&[hit("a.txt", 1, 1, "alpha")]);
        results.lexical_only = true;
        let bundle = assemble("alpha", &results, &fixture.1, 4000, 24);
        assert!(bundle.lexical_only);
        assert!(
            bundle.text.contains("lexical-only degradation"),
            "{}",
            bundle.text
        );
    }

    #[test]
    fn an_empty_result_is_none_and_says_what_it_found_nothing_for() {
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let bundle = assemble("nothing here", &response(&[]), &fixture.1, 4000, 24);
        assert_eq!(bundle.verdict, "none");
        assert!(bundle.spans.is_empty());
        assert!(
            bundle
                .text
                .starts_with("VERDICT: none (nothing relevant found for: nothing here)"),
            "{}",
            bundle.text
        );
    }

    #[test]
    fn the_label_is_the_index_own_name_and_synthetic_ids_are_dropped() {
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let mut symbol = hit("a.txt", 1, 1, "alpha");
        symbol.symbol_path = Some("Board.Update".into());
        let mut synthetic = hit("a.txt", 1, 1, "alpha");
        synthetic.symbol_path = Some("#s0".into());
        synthetic.heading_path = Some(vec!["Guide".into(), "Setup".into()]);

        assert_eq!(label(&symbol), "Board.Update");
        assert_eq!(label(&synthetic), "Guide > Setup");
        assert_eq!(label(&hit("a.txt", 1, 1, "alpha")), "");

        let bundle = assemble("alpha", &response(&[symbol]), &fixture.1, 4000, 24);
        assert!(
            bundle.text.contains("=== a.txt:1-1 [Board.Update] ==="),
            "{}",
            bundle.text
        );
    }

    // =======================================================================
    // INDEPENDENT VERIFICATION PASS
    // =======================================================================
    //
    // Authored separately from the port, against its two ground truths rather
    // than against this module's own habits:
    //
    //   1. `bench/rcb/sandbox/lore_pkg.py` — the validated prototype, whose
    //      term rules, calibration constants, widen/merge arithmetic, budget
    //      demotion, dedent/BOM handling and rendering are the spec;
    //   2. `design/4_Interfaces/2026-08-27_bundle-mcp-tool.md` — the contract.
    //
    // Expectations marked **(oracle)** were produced by EXECUTING the
    // prototype, never by reading it. Its pure helpers (`_CAMEL_RE.findall`,
    // `_WORD_RE.findall`, `_stems`, `query_terms`, `coverage`, `_normalize`)
    // were called directly, and `build_bundle` was driven end to end over a
    // temporary corpus with `search`/`expand` stubbed out. The `expand` stub
    // reproduces `super::expand::widen`'s own arithmetic
    // (`start = max(1, start - ctx)`, `end = min(file_lines, end + ctx)`), so
    // this module's direct-from-disk widening is being compared against the
    // span the prototype's round trip would actually have returned.
    //
    // Deviations from the prototype that are declared and intentional
    // (Sources-based resolution, no `expand` round trip, no timing fields,
    // restructured `dropped`, `Option`-al label) are exercised for behaviour
    // equivalence, not for identity. Deviations found that are NOT declared
    // are marked `defect_` and `#[ignore]`d rather than asserted away.

    // -- terms: the camel/underscore splitter -------------------------------

    /// (oracle) `_CAMEL_RE = [A-Z]+(?![a-z])|[A-Z][a-z0-9]*|[a-z0-9]+`,
    /// `findall` on each chunk. The awkward cases are the ones where the
    /// regex engine backtracks out of a greedy capital run, and the ones
    /// where it does not: `IDs` really is `I` + `Ds`, and `OAuth2Client`
    /// really is `O` + `Auth2` + `Client`.
    #[test]
    fn indep_case_parts_matches_the_prototype_camel_regex() {
        let expected: &[(&str, &[&str])] = &[
            ("HTTPServer", &["HTTP", "Server"]),
            ("ABC", &["ABC"]),
            ("getURL", &["get", "URL"]),
            ("Foo2Bar", &["Foo2", "Bar"]),
            ("HTTP2Server", &["HTTP", "2", "Server"]),
            ("ABCdef", &["AB", "Cdef"]),
            ("aB", &["a", "B"]),
            ("A", &["A"]),
            ("Ab", &["Ab"]),
            ("XMLHTTPRequest", &["XMLHTTP", "Request"]),
            ("IOError", &["IO", "Error"]),
            ("parseJSONData", &["parse", "JSON", "Data"]),
            ("a", &["a"]),
            ("2fa", &["2fa"]),
            ("snake", &["snake"]),
            ("ALLCAPS", &["ALLCAPS"]),
            ("camelCase", &["camel", "Case"]),
            ("PascalCase", &["Pascal", "Case"]),
            (
                "HTTPSProxyURLHandler",
                &["HTTPS", "Proxy", "URL", "Handler"],
            ),
            ("V2", &["V", "2"]),
            ("v2", &["v2"]),
            ("MyID", &["My", "ID"]),
            ("IDs", &["I", "Ds"]),
            ("OAuth2Client", &["O", "Auth2", "Client"]),
            ("", &[]),
        ];
        for (chunk, want) in expected {
            assert_eq!(&case_parts(chunk), want, "case_parts({chunk:?})");
        }
    }

    /// (oracle) `_WORD_RE = [A-Za-z][A-Za-z0-9_]*`. A token must *start* with
    /// an ASCII letter, and non-ASCII prose tokenizes on the ASCII runs
    /// inside it — which is also the case that would panic a byte-index
    /// scanner that got its boundaries wrong.
    #[test]
    fn indep_word_scanner_matches_the_prototype_word_regex() {
        let expected: &[(&str, &[&str])] = &[
            ("_foo bar9 9abc a1_b", &["foo", "bar9", "abc", "a1_b"]),
            ("föö bär", &["f", "b", "r"]),
            ("x-y_z", &["x", "y_z"]),
            ("  ", &[]),
            ("A.B.C", &["A", "B", "C"]),
            ("", &[]),
        ];
        for (text, want) in expected {
            assert_eq!(&words(text), want, "words({text:?})");
        }
    }

    /// (oracle) `_stems`: the term plus one-, two- and three-character
    /// truncations, each kept only when at least four characters survive.
    #[test]
    fn indep_stems_match_the_prototype() {
        let expected: &[(&str, &[&str])] = &[
            ("agents", &["agents", "agent", "agen"]),
            (
                "orchestrator",
                &["orchestrator", "orchestrato", "orchestrat", "orchestra"],
            ),
            ("span", &["span"]),
            ("abcd", &["abcd"]),
            ("abcde", &["abcde", "abcd"]),
            ("abcdef", &["abcdef", "abcde", "abcd"]),
            ("abcdefg", &["abcdefg", "abcdef", "abcde", "abcd"]),
            ("public", &["public", "publi", "publ"]),
            (
                "implementation",
                &[
                    "implementation",
                    "implementatio",
                    "implementati",
                    "implementat",
                ],
            ),
            ("http", &["http"]),
        ];
        for (term, want) in expected {
            assert_eq!(&stems(term), want, "stems({term:?})");
        }
        // The floor is a floor in both directions: a stem must MATCH as a
        // prefix, so `implement` does not cover `implementation` but
        // `implementations` does.
        assert_eq!(
            coverage(&["implementation".to_string()], "implement"),
            (vec![], vec!["implementation".to_string()])
        );
        assert_eq!(
            coverage(&["implementation".to_string()], "implementations"),
            (vec!["implementation".to_string()], vec![])
        );
        assert_eq!(
            coverage(&["span".to_string()], "spa"),
            (vec![], vec!["span".to_string()])
        );
    }

    /// (oracle) Whole-query extraction, including the stopword boundaries the
    /// prototype argues about in prose: `run`/`runs` are glue but `running`
    /// and `runner` are not; `apis`/`usage`/`source`/`sources` were promoted
    /// into the brief list but `api` and `answer`/`answers` deliberately were
    /// not; `public`, `implementation`, `behavior`, `documentation` and
    /// `serialization` stay countable.
    #[test]
    fn indep_query_terms_match_the_prototype() {
        let expected: &[(&str, &[&str])] = &[
            (
                "CheckpointStorage and parse_HTTPHeader",
                &[
                    "checkpointstorage",
                    "checkpoint",
                    "storage",
                    "parse_httpheader",
                    "parse",
                    "http",
                    "header",
                ],
            ),
            (
                "Identify the exact source locations and cite concrete evidence showing any \
                 usage examples of retries",
                &["retries"],
            ),
            (
                "public implementation behavior answer",
                &["public", "implementation", "behavior", "answer"],
            ),
            (
                "documentation serialization usage",
                &["documentation", "serialization"],
            ),
            (
                "HTTPServer XMLHttpRequest parse_JSON_data",
                &[
                    "httpserver",
                    "http",
                    "server",
                    "xmlhttprequest",
                    "xml",
                    "request",
                    "parse_json_data",
                    "parse",
                    "json",
                    "data",
                ],
            ),
            ("the and for with how does", &[]),
            ("a ab abc abcd", &["abc", "abcd"]),
            ("Foo Foo foo FOO", &["foo"]),
            (
                "widget_factory WidgetFactory widget factory",
                &["widget_factory", "widget", "factory", "widgetfactory"],
            ),
            (
                "answers answer sources source apis api",
                &["answers", "answer", "api"],
            ),
            (
                "IOError handling in read_file_utf8",
                &[
                    "ioerror",
                    "error",
                    "handling",
                    "read_file_utf8",
                    "read",
                    "file",
                    "utf8",
                ],
            ),
            ("run runs running runner", &["running", "runner"]),
            ("", &[]),
            ("   ", &[]),
            (
                "quantum chromodynamics lattice gauge",
                &["quantum", "chromodynamics", "lattice", "gauge"],
            ),
        ];
        for (query, want) in expected {
            assert_eq!(query_terms(query), *want, "query_terms({query:?})");
        }
    }

    /// The stopword table was diffed as a SET against
    /// `lore_pkg._STOPWORDS | _BRIEF_STOPWORDS` by running the prototype:
    /// 218 words each way, symmetric difference empty. This guards the size
    /// and the entries the prototype's comments call load-bearing, so a
    /// future edit cannot quietly move a word across the line.
    #[test]
    fn indep_stopword_table_is_the_prototypes_union() {
        assert_eq!(STOPWORDS.len(), 218, "the prototype's union has 218 words");
        for glue in [
            "identify",
            "evidence",
            "exact",
            "locations",
            "source",
            "sources",
            "apis",
            "usage",
            "repository",
            "code",
            "run",
            "runs",
            "example",
            "examples",
            "task",
            "point",
        ] {
            assert!(is_stopword(glue), "{glue} is brief vocabulary");
        }
        for real in [
            "implementation",
            "documentation",
            "serialization",
            "public",
            "behavior",
            "answer",
            "answers",
            "api",
            "running",
            "runner",
        ] {
            assert!(!is_stopword(real), "{real} must stay countable");
        }
    }

    // -- verdict cuts -------------------------------------------------------

    /// Twenty four-letter terms, pairwise non-substring so `stems` cannot let
    /// one cover another; the fixture file carries the first `covered` of
    /// them, which makes the ratio exactly `covered/20`.
    const INDEP_TERMS: [&str; 20] = [
        "qqqa", "wwwb", "eeec", "rrrd", "ttte", "yyyf", "uuug", "iiih", "oooi", "pppj", "aaak",
        "sssl", "dddm", "fffn", "gggo", "hhhp", "jjjq", "kkkr", "llls", "zzzt",
    ];

    fn indep_ratio_bundle(covered: usize) -> BundleResponse {
        let body = format!("{}\n", INDEP_TERMS[..covered].join(" "));
        let fixture = corpus(&[("z.txt", &body)]);
        assemble(
            &INDEP_TERMS.join(" "),
            &response(&[hit("z.txt", 1, 1, "")]),
            &fixture.1,
            40_000,
            24,
        )
    }

    /// (oracle) The cuts are inclusive: 13/20 is exactly 0.65 and must read
    /// `found`, 9/20 is exactly 0.45 and must read `weak`. The prototype's
    /// whole calibration argument is about where these two numbers sit, so a
    /// `>` where it wants `>=` is a silent recalibration.
    #[test]
    fn indep_verdict_cuts_are_inclusive_at_exactly_0_65_and_0_45() {
        let found = indep_ratio_bundle(13);
        assert_eq!(found.coverage, 0.65);
        assert_eq!(found.verdict, "found");

        let below = indep_ratio_bundle(12);
        assert_eq!(below.coverage, 0.6);
        assert_eq!(below.verdict, "weak");

        let weak = indep_ratio_bundle(9);
        assert_eq!(weak.coverage, 0.45);
        assert_eq!(weak.verdict, "weak");
        assert!(
            weak.text
                .contains("weak (1 verified span(s), but only 9 of 20 query terms appear in them"),
            "{}",
            weak.text
        );

        let none = indep_ratio_bundle(8);
        assert_eq!(none.coverage, 0.4);
        assert_eq!(none.verdict, "none");
        assert!(
            none.text
                .contains("only 8 of 20 query terms appear anywhere in what came back"),
            "{}",
            none.text
        );

        let all = indep_ratio_bundle(20);
        assert_eq!(all.coverage, 1.0);
        assert_eq!(all.verdict, "found");
        assert!(all.terms_uncovered.is_empty());
    }

    /// (oracle) Uncovered terms keep the query's order, which is what makes
    /// `NO MATCH FOR:` readable as "the tail of what you asked".
    #[test]
    fn indep_uncovered_terms_keep_query_order() {
        let weak = indep_ratio_bundle(13);
        assert_eq!(
            weak.terms_uncovered,
            ["fffn", "gggo", "hhhp", "jjjq", "kkkr", "llls", "zzzt"]
        );
        assert!(
            weak.text
                .contains("NO MATCH FOR: fffn, gggo, hhhp, jjjq, kkkr, llls, zzzt"),
            "{}",
            weak.text
        );
    }

    /// (oracle) A term that appears only in an *overflowed* or *oversized*
    /// span's path is covered by the path and not by any text, because the
    /// prototype folds `spans + oversized` paths into the blob after
    /// budgeting. `charlie` is covered (its path came back), `line` is not
    /// (the 300-line block was never rendered).
    #[test]
    fn indep_coverage_counts_paths_of_demoted_spans_but_not_their_text() {
        let oversized = corpus(&[("charlie.txt", &numbered(400, ""))]);
        let bundle = assemble(
            "charlie line",
            &response(&[hit("charlie.txt", 1, 300, "")]),
            &oversized.1,
            40_000,
            24,
        );
        assert_eq!(bundle.coverage, 0.5);
        assert_eq!(bundle.terms_covered, ["charlie"]);
        assert_eq!(bundle.terms_uncovered, ["line"]);

        // The same rule for a span the *budget* demoted rather than its size.
        let body = numbered(400, "");
        let budgeted = corpus(&[("alpha.txt", &body), ("bravo.txt", &body)]);
        let bundle = assemble(
            "alpha bravo",
            &response(&[hit("alpha.txt", 1, 100, ""), hit("bravo.txt", 1, 100, "")]),
            &budgeted.1,
            400,
            24,
        );
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.further_reading.len(), 1);
        assert_eq!(bundle.coverage, 1.0);
    }

    /// A query made only of stopwords has no terms at all, and the prototype
    /// calls that vacuously covered (`ratio = 1.0`) rather than a failure.
    #[test]
    fn indep_an_all_stopword_query_is_vacuously_covered() {
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let bundle = assemble(
            "the and for",
            &response(&[hit("a.txt", 1, 1, "alpha")]),
            &fixture.1,
            40_000,
            24,
        );
        assert!(bundle.terms.is_empty());
        assert_eq!(bundle.coverage, 1.0);
        assert_eq!(bundle.verdict, "found");
    }

    // -- staleness, dedent, BOM, line endings -------------------------------

    /// (oracle) The chunker dedents; the file may indent with tabs where the
    /// excerpt has spaces. Indentation is compared not at all, and the render
    /// keeps the file's real tabs.
    #[test]
    fn indep_tabs_in_the_file_against_spaces_in_the_excerpt_are_not_stale() {
        let fixture = corpus(&[("m.py", "class C:\n\tdef foo(self):\n\t\treturn 1\n")]);
        let bundle = assemble(
            "foo",
            &response(&[hit("m.py", 2, 3, "def foo(self):\n    return 1")]),
            &fixture.1,
            40_000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
        assert_eq!(
            bundle.text,
            "VERDICT: found (1 verified span(s) from 1 file(s))\n\
             === m.py:1-3 ===\n\
             1  class C:\n\
             2  \tdef foo(self):\n\
             3  \t\treturn 1\n"
        );
    }

    /// (oracle) A *partial* dedent — the chunker stripped some but not all of
    /// the common indent — is still not stale, for the same reason.
    #[test]
    fn indep_a_partially_dedented_excerpt_is_not_stale() {
        let fixture = corpus(&[("m.py", "class C:\n    def foo(self):\n        return 1\n")]);
        let bundle = assemble(
            "foo",
            &response(&[hit("m.py", 2, 3, "  def foo(self):\n      return 1")]),
            &fixture.1,
            40_000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
        assert_eq!(bundle.spans.len(), 1);
    }

    /// (oracle) Blank lines are dropped on both sides before comparing, and
    /// trailing whitespace is trimmed away — neither is "different code".
    #[test]
    fn indep_blank_lines_and_trailing_whitespace_are_not_staleness() {
        let blanks = corpus(&[("m.py", "one\ntwo\nthree\n")]);
        let bundle = assemble(
            "two",
            &response(&[hit("m.py", 1, 3, "one\n\ntwo\n\nthree")]),
            &blanks.1,
            40_000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);

        let trailing = corpus(&[("m.py", "def foo():   \n    pass\t\n")]);
        let bundle = assemble(
            "foo",
            &response(&[hit("m.py", 1, 2, "def foo():\npass")]),
            &trailing.1,
            40_000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
    }

    /// (oracle) Interior whitespace is NOT collapsed: `def  foo` and
    /// `def foo` are different code, and the check exists to catch exactly
    /// that.
    #[test]
    fn indep_interior_whitespace_still_counts_as_moved_text() {
        let fixture = corpus(&[("m.py", "def  foo():\n    pass\n")]);
        let bundle = assemble(
            "foo",
            &response(&[hit("m.py", 1, 2, "def foo():\npass")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.dropped.len(), 1);
        assert_eq!(bundle.dropped[0].reason, "stale");
        assert_eq!(bundle.dropped[0].paths, ["m.py:1-2"]);
    }

    /// (oracle) A CRLF file compared against an LF excerpt is not stale, and
    /// the render carries no carriage returns — the prototype reads in text
    /// mode (universal newlines) and `str::lines()` lands in the same place.
    #[test]
    fn indep_a_crlf_file_against_an_lf_excerpt_is_not_stale() {
        let fixture = corpus(&[("m.py", "one\r\ntwo\r\nthree\r\n")]);
        let bundle = assemble(
            "two",
            &response(&[hit("m.py", 1, 3, "one\ntwo\nthree")]),
            &fixture.1,
            40_000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
        assert_eq!(
            bundle.text,
            "VERDICT: found (1 verified span(s) from 1 file(s))\n\
             === m.py:1-3 ===\n1  one\n2  two\n3  three\n"
        );
    }

    /// A byte-order mark inside the *excerpt* is not stripped by either side
    /// (neither `str::trim` nor Python's `str.strip` treats U+FEFF as
    /// whitespace), so an index row that stored the mark reads as stale. The
    /// prototype agrees; this pins the asymmetry deliberately, because
    /// stripping only the disk side is the whole point of the BOM rule.
    #[test]
    fn indep_a_bom_kept_in_the_excerpt_reads_as_stale() {
        let fixture = corpus(&[("b.md", "\u{feff}# Title\nbody\n")]);
        let bundle = assemble(
            "title",
            &response(&[hit("b.md", 1, 2, "\u{feff}# Title\nbody")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.dropped.len(), 1);
        assert_eq!(bundle.dropped[0].reason, "stale");
    }

    /// **Declared-deviation probe.** `str::lines()` splits on `\n` only, so a
    /// classic-Mac file whose lines are separated by bare `\r` is one line
    /// here; the prototype reads through Python's universal-newline
    /// translation and sees three. This module's answer is the one that
    /// agrees with the rest of the daemon (`daemon::expand::widen` and the
    /// chunker both use `str::lines()`), so the divergence is benign — but it
    /// is a divergence, and it is asserted rather than left to chance.
    #[test]
    fn indep_a_bare_cr_is_not_a_line_break_here_though_it_is_in_the_prototype() {
        let fixture = corpus(&[("cr.txt", "one\rtwo\rthree\n")]);
        let bundle = assemble(
            "two",
            &response(&[hit("cr.txt", 1, 1, "one")]),
            &fixture.1,
            40_000,
            24,
        );
        // One line, so line 1 is the whole file and the one-line excerpt does
        // not match it. The prototype renders `cr.txt:1-3` instead.
        assert_eq!(bundle.dropped.len(), 1);
        assert_eq!(bundle.dropped[0].reason, "stale");
    }

    // -- ranges -------------------------------------------------------------

    /// (oracle) The range check is `1 <= start <= end <= file_lines`, and the
    /// file's line count excludes the empty string a trailing newline splits
    /// off.
    #[test]
    fn indep_range_edges_match_the_prototype() {
        let two = corpus(&[("m.py", "one\ntwo\n")]);
        for (start, end) in [(0u32, 1u32), (2, 1), (1, 3), (3, 3)] {
            let bundle = assemble(
                "one",
                &response(&[hit("m.py", start, end, "")]),
                &two.1,
                40_000,
                24,
            );
            assert_eq!(
                bundle.dropped.first().map(|d| d.reason.as_str()),
                Some("range"),
                "{start}-{end} should be out of range"
            );
        }

        // A file with no trailing newline still has its last line.
        let ragged = corpus(&[("m.py", "one\ntwo")]);
        let bundle = assemble(
            "two",
            &response(&[hit("m.py", 2, 2, "two")]),
            &ragged.1,
            40_000,
            24,
        );
        assert!(bundle.dropped.is_empty(), "{:?}", bundle.dropped);
        assert_eq!(
            (bundle.spans[0].line_start, bundle.spans[0].line_end),
            (1, 2),
            "and it widens to the whole two-line file"
        );

        // An empty file has zero lines, so nothing can be in range.
        let empty = corpus(&[("m.py", "")]);
        let bundle = assemble(
            "one",
            &response(&[hit("m.py", 1, 1, "")]),
            &empty.1,
            40_000,
            24,
        );
        assert_eq!(bundle.dropped[0].reason, "range");
    }

    // -- widening -----------------------------------------------------------

    /// (oracle, against the prototype driven through a stub of
    /// `super::expand::widen`) The threshold is `width < 16`, and the context
    /// is `ceil((28 - width) / 2)` on each side: 15 lines gains 7 a side,
    /// 1 line gains 14 a side, 16 lines is left exactly as it is.
    #[test]
    fn indep_widening_threshold_and_context_match_the_prototype() {
        let fixture = corpus(&[("f.txt", &numbered(400, ""))]);
        let cases = [
            ((100u32, 115u32), (100u32, 115u32), 0u32), // width 16: untouched
            ((100, 114), (93, 121), 1),                 // width 15: +7 a side
            ((100, 100), (86, 114), 1),                 // width  1: +14 a side
            ((100, 112), (92, 120), 1),                 // width 13: +8 a side
        ];
        for ((start, end), (want_start, want_end), want_widened) in cases {
            let bundle = assemble(
                "line",
                &response(&[hit("f.txt", start, end, "")]),
                &fixture.1,
                40_000,
                24,
            );
            assert_eq!(
                (
                    bundle.spans[0].line_start,
                    bundle.spans[0].line_end,
                    bundle.spans_widened
                ),
                (want_start, want_end, want_widened),
                "widening {start}-{end}"
            );
        }
    }

    /// (oracle) Widening that clamps to a no-op is not counted as a widening,
    /// which is what keeps `spans_widened` an honest number rather than "how
    /// many short spans there were".
    #[test]
    fn indep_a_widening_clamped_to_nothing_is_not_counted() {
        let fixture = corpus(&[("s.txt", "a\nb\nc\n")]);
        let bundle = assemble(
            "bbb",
            &response(&[hit("s.txt", 1, 3, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.spans_widened, 0);
        assert_eq!(
            (bundle.spans[0].line_start, bundle.spans[0].line_end),
            (1, 3)
        );
    }

    // -- merging ------------------------------------------------------------

    /// Drive `assemble` over one 400-line file with spans wide enough that
    /// widening never fires, and report `(start, end, merged)` per span.
    fn indep_merged(hits: &[(u32, u32)]) -> Vec<(u32, u32, u32)> {
        let fixture = corpus(&[("f.txt", &numbered(400, ""))]);
        let results: Vec<SearchResult> = hits
            .iter()
            .map(|&(start, end)| hit("f.txt", start, end, ""))
            .collect();
        let bundle = assemble("line", &response(&results), &fixture.1, 40_000, 24);
        bundle
            .spans
            .iter()
            .map(|span| (span.line_start, span.line_end, span.merged))
            .collect()
    }

    /// (oracle) `MERGE_GAP` is inclusive: four blank lines between two spans
    /// merge, five do not.
    #[test]
    fn indep_merge_gap_is_inclusive_at_four() {
        assert_eq!(indep_merged(&[(1, 20), (24, 43)]), [(1, 43, 2)]);
        assert_eq!(
            indep_merged(&[(1, 20), (25, 44)]),
            [(1, 20, 1), (25, 44, 1)]
        );
    }

    /// (oracle) Overlap and containment give a negative gap, which must
    /// always merge; and a later span that starts *before* the one it merges
    /// into extends it backwards while keeping the earlier span's rank
    /// position.
    #[test]
    fn indep_merge_handles_containment_and_backward_extension() {
        // 20-30 sits entirely inside 10-40: the union is unchanged.
        assert_eq!(indep_merged(&[(10, 40), (20, 30)]), [(10, 40, 2)]);
        // 60-99 abuts 100-140 from below (gap 1) and drags the start back.
        assert_eq!(indep_merged(&[(100, 140), (60, 99)]), [(60, 140, 2)]);
        // Exactly adjacent, no gap at all.
        assert_eq!(indep_merged(&[(1, 20), (21, 40)]), [(1, 40, 2)]);
    }

    /// (oracle) `MERGE_CAP_LINES` is inclusive too: a union of exactly 140
    /// lines merges, 141 refuses and the two stay separate.
    #[test]
    fn indep_merge_cap_is_inclusive_at_one_hundred_and_forty() {
        assert_eq!(indep_merged(&[(1, 100), (104, 140)]), [(1, 140, 2)]);
        assert_eq!(
            indep_merged(&[(1, 100), (104, 141)]),
            [(1, 100, 1), (104, 141, 1)]
        );
    }

    /// (oracle) Merging is a single forward pass: a span grows as members
    /// join it, so a third span can reach a span it could not have reached
    /// before — but the pass never goes back to re-check earlier output, so
    /// arrival order decides the answer. Both orders are pinned, because
    /// "fix" the second one and rank order stops meaning anything.
    #[test]
    fn indep_merging_chains_forward_but_never_rescans() {
        // 24-43 joins 1-20 (gap 4), which makes 1-43; 47-66 is then gap 4
        // from the grown span and joins too.
        assert_eq!(indep_merged(&[(1, 20), (24, 43), (47, 66)]), [(1, 66, 3)]);
        // Same three spans, different rank order: 47-66 arrives while 1-20 is
        // still short, so it stays its own span forever.
        assert_eq!(
            indep_merged(&[(1, 20), (47, 66), (24, 43)]),
            [(1, 43, 2), (47, 66, 1)]
        );
    }

    /// Same line numbers, different files: never merged, and `merged` stays 1.
    #[test]
    fn indep_spans_in_different_files_never_merge() {
        let body = numbered(400, "");
        let fixture = corpus(&[("a.txt", &body), ("b.txt", &body)]);
        let bundle = assemble(
            "line",
            &response(&[hit("a.txt", 1, 20, ""), hit("b.txt", 1, 20, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.spans_after_merge, 2);
        assert!(bundle.spans.iter().all(|span| span.merged == 1));
        assert!(
            bundle
                .text
                .starts_with("VERDICT: found (2 verified span(s) from 2 file(s))"),
            "{}",
            bundle.text
        );
    }

    // -- size and budget ----------------------------------------------------

    /// (oracle) `MAX_SPAN_LINES` is inclusive: 160 lines render, 161 become a
    /// pointer without ever entering the budget.
    #[test]
    fn indep_oversize_cut_is_inclusive_at_one_hundred_and_sixty() {
        let fixture = corpus(&[("f.txt", &numbered(400, ""))]);
        let fits = assemble(
            "line",
            &response(&[hit("f.txt", 1, 160, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(fits.spans_oversized, 0);
        assert_eq!(fits.spans.len(), 1);

        let over = assemble(
            "line",
            &response(&[hit("f.txt", 1, 161, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(over.spans_oversized, 1);
        assert!(over.spans.is_empty());
        assert_eq!(over.spans_after_merge, 1, "it was still a merged span");
        assert!(
            over.text.contains("FURTHER READING: f.txt:1-161"),
            "{}",
            over.text
        );
    }

    /// (oracle) The budget test is `used + cost > budget_chars`, so a bundle
    /// that lands exactly on the budget still fits. Two 16-line blocks from
    /// `a.txt` and `ccc.txt` cost 203 + 205 = 408 characters including the
    /// two-character joiners; at 102 tokens (408 chars) both render, and four
    /// characters less demotes the second one whole.
    #[test]
    fn indep_the_budget_admits_a_span_that_lands_exactly_on_it() {
        let body = numbered(400, "");
        let fixture = corpus(&[("a.txt", &body), ("ccc.txt", &body)]);
        let hits = [hit("a.txt", 1, 16, ""), hit("ccc.txt", 1, 16, "")];

        let exact = assemble("line", &response(&hits), &fixture.1, 102, 24);
        assert_eq!(exact.spans.len(), 2, "408 chars is exactly enough");
        assert!(exact.further_reading.is_empty());

        let short = assemble("line", &response(&hits), &fixture.1, 101, 24);
        assert_eq!(short.spans.len(), 1);
        assert_eq!(short.further_reading.len(), 1);
        assert_eq!(short.further_reading[0].path, "ccc.txt");
    }

    /// (oracle) Demotion is per span and does not stop the scan: a big span
    /// that will not fit is skipped, and a later smaller one still renders.
    /// The alternative — stopping at the first overflow — would silently make
    /// the bundle a prefix of the ranking rather than the best of it.
    #[test]
    fn indep_a_smaller_later_span_still_fits_after_a_bigger_one_is_demoted() {
        let body = numbered(400, "");
        let fixture = corpus(&[("a.txt", &body), ("b.txt", &body), ("c.txt", &body)]);
        let bundle = assemble(
            "line",
            &response(&[
                hit("a.txt", 1, 20, ""),
                hit("b.txt", 1, 200, ""),
                hit("c.txt", 1, 20, ""),
            ]),
            &fixture.1,
            200,
            24,
        );
        assert_eq!(bundle.spans_after_merge, 3);
        assert_eq!(bundle.spans_oversized, 1);
        let paths: Vec<&str> = bundle.spans.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "c.txt"]);
        assert_eq!(bundle.further_reading.len(), 1);
        assert_eq!(bundle.further_reading[0].path, "b.txt");
    }

    /// (oracle) Twenty-five verified spans, one budgeted in: the header names
    /// twenty pointers and no more, while the structured field keeps all
    /// twenty-four.
    #[test]
    fn indep_further_reading_prints_at_most_twenty_pointers() {
        let body = numbered(400, "");
        let files: Vec<(String, String)> = (0..25)
            .map(|i| (format!("f{i:02}.txt"), body.clone()))
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_str()))
            .collect();
        let fixture = corpus(&borrowed);
        let hits: Vec<SearchResult> = (0..25)
            .map(|i| hit(&format!("f{i:02}.txt"), 1, 20, ""))
            .collect();

        let bundle = assemble("line", &response(&hits), &fixture.1, 100, 24);
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.further_reading.len(), 24);
        let line = bundle
            .text
            .lines()
            .find(|line| line.starts_with("FURTHER READING:"))
            .expect("a further-reading line");
        assert_eq!(line.matches(", ").count(), 19, "{line}");
        assert!(line.ends_with("f20.txt:1-20"), "{line}");
    }

    /// (oracle) The `none` trim runs on a budget of its own and keeps the
    /// first span whatever it costs — a 150-line block is ~5.9k characters,
    /// well past the 4.8k the trim allows, and it still renders because a
    /// bundle that trimmed itself to nothing would be claiming something
    /// different from what happened.
    #[test]
    fn indep_the_none_trim_keeps_its_first_span_past_its_own_budget() {
        let body = numbered(400, " padding padding padding");
        let fixture = corpus(&[("a.txt", &body), ("b.txt", &body)]);
        let bundle = assemble(
            "quantum chromodynamics lattice gauge",
            &response(&[hit("a.txt", 1, 150, ""), hit("b.txt", 1, 150, "")]),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(bundle.verdict, "none");
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.further_reading.len(), 1);
        assert!(
            bundle.bundle_tokens_est > NONE_BUDGET_TOKENS,
            "the first span is kept whole: {}",
            bundle.bundle_tokens_est
        );
        // ...and the coverage is NOT re-measured after the trim, or the
        // verdict that caused it could flip underneath itself.
        assert_eq!(bundle.coverage, 0.0);
    }

    /// A `weak` bundle is not trimmed — only `none` is.
    #[test]
    fn indep_only_a_none_verdict_is_trimmed() {
        let body = format!("{}\n", INDEP_TERMS[..10].join(" "));
        let padding = numbered(300, " padding padding padding padding");
        let fixture = corpus(&[("z.txt", &body), ("pad.txt", &padding)]);
        let bundle = assemble(
            &INDEP_TERMS.join(" "),
            &response(&[hit("z.txt", 1, 1, ""), hit("pad.txt", 1, 150, "")]),
            &fixture.1,
            4000,
            24,
        );
        assert_eq!(bundle.verdict, "weak");
        assert_eq!(bundle.spans.len(), 2, "no trim at `weak`");
        assert!(bundle.bundle_tokens_est > NONE_BUDGET_TOKENS);
    }

    // -- the DROPPED header -------------------------------------------------

    /// (oracle) The count is of hits, the list is of distinct paths sorted:
    /// three refusals over two paths print as `(missing, 3)` with two names.
    #[test]
    fn indep_dropped_counts_hits_but_lists_distinct_sorted_paths() {
        let fixture = corpus(&[("m.py", "one\ntwo\n")]);
        let bundle = assemble(
            "one",
            &response(&[
                hit("gone.py", 1, 1, ""),
                hit("gone.py", 1, 1, ""),
                hit("also-gone.py", 1, 1, ""),
            ]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.hits_rejected, 3);
        assert_eq!(bundle.dropped.len(), 1);
        assert_eq!(bundle.dropped[0].count, 3);
        assert_eq!(bundle.dropped[0].paths, ["also-gone.py", "gone.py"]);
        assert!(
            bundle
                .text
                .contains("DROPPED (missing, 3): also-gone.py, gone.py"),
            "{}",
            bundle.text
        );
    }

    /// (oracle) Header order in full: verdict, then `NO MATCH FOR`, then the
    /// lexical-only note, then one `DROPPED` line per reason in alphabetical
    /// order. Asserted as whole text, because the order is the contract's and
    /// not this module's.
    #[test]
    fn indep_the_header_is_assembled_in_the_prototypes_order() {
        let fixture = corpus(&[("m.py", "one\ntwo\n")]);
        let mut results = response(&[
            hit("m.py", 1, 99, ""),
            hit("gone.py", 1, 1, ""),
            hit("m.py", 1, 2, "not what is there"),
        ]);
        results.lexical_only = true;
        let bundle = assemble("zzzt", &results, &fixture.1, 40_000, 24);
        assert_eq!(
            bundle.text,
            "VERDICT: none (nothing relevant found for: zzzt)\n\
             NO MATCH FOR: zzzt\n\
             NOTE: the index answered without its vector arm (lexical-only degradation); \
             recall may be lower than usual.\n\
             DROPPED (missing, 1): gone.py\n\
             DROPPED (range, 1): m.py\n\
             DROPPED (stale, 1): m.py:1-2\n"
        );
    }

    /// **Defect (cosmetic, low severity).** The prototype names an empty
    /// indexed path `?` in the `DROPPED` line (`where = path or "?"`); this
    /// port prints the empty string, so the line reads `DROPPED (missing, 1):`
    /// with nothing after the colon. Ignored rather than deleted: the
    /// assertion below is what the prototype does, and the port should be
    /// changed to match rather than the test relaxed.
    #[test]
    fn defect_an_empty_indexed_path_should_be_named_in_the_dropped_line() {
        let fixture = corpus(&[("m.py", "one\n")]);
        let bundle = assemble(
            "one",
            &response(&[hit("", 1, 1, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.dropped[0].paths, ["?"]);
        assert!(
            bundle.text.contains("DROPPED (missing, 1): ?"),
            "{}",
            bundle.text
        );
    }

    /// **Defect (cosmetic, low severity).** `round3` rounds half away from
    /// zero; the prototype's `round(ratio, 3)` rounds half to even. They
    /// disagree whenever `ratio * 1000` is exactly `n + 0.5` and exactly
    /// representable — 1/16 is 0.0625, which the prototype reports as 0.062
    /// and this port as 0.063. Only the reported `coverage` field moves; the
    /// verdict is taken from the unrounded ratio, so no cut is affected.
    #[test]
    fn defect_coverage_rounding_should_match_the_prototypes_half_to_even() {
        let body = format!("{}\n", INDEP_TERMS[0]);
        let fixture = corpus(&[("z.txt", &body)]);
        let bundle = assemble(
            &INDEP_TERMS[..16].join(" "),
            &response(&[hit("z.txt", 1, 1, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.terms.len(), 16);
        assert_eq!(bundle.terms_covered.len(), 1);
        assert_eq!(bundle.coverage, 0.062, "1/16 == 0.0625, half to even");
    }

    // -- containment: the adversarial cases ---------------------------------

    /// A link to a directory in whatever form the platform makes one — a
    /// junction on Windows (needs no privilege, unlike a file symlink), an
    /// ordinary symlink on POSIX. Same shape as `super::paths`' own tests.
    #[cfg(windows)]
    fn indep_link_dir(link: &Utf8Path, target: &Utf8Path) {
        let out = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J", link.as_str(), target.as_str()])
            .output()
            .expect("mklink is available");
        assert!(out.status.success(), "mklink /J failed: {out:?}");
    }

    #[cfg(unix)]
    fn indep_link_dir(link: &Utf8Path, target: &Utf8Path) {
        std::os::unix::fs::symlink(target, link).expect("a POSIX symlink needs no privilege");
    }

    /// The adversarial containment case the `..` test cannot reach: a link
    /// that lives *inside* the project root and points out of it. Every
    /// component of the logical path is innocent, and only realpath resolution
    /// reveals the escape — which is exactly why the check is realpath against
    /// realpath and not a string prefix on the logical path.
    #[test]
    fn indep_a_link_inside_the_root_pointing_out_of_it_cannot_be_quoted() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let outer = paths::canonicalize_root(dir.path()).expect("a canonical root");
        let secrets = outer.join("secrets");
        std::fs::create_dir(&secrets).expect("creating the outside directory");
        std::fs::write(secrets.join("secret.env"), "TOKEN=hunter2\n").expect("the outside file");

        let root = outer.join("proj");
        std::fs::create_dir(&root).expect("creating the project root");
        std::fs::write(root.join("a.txt"), "alpha\n").expect("a fixture file");
        indep_link_dir(&root.join("escape"), &secrets);
        let sources = Sources::from_declared(&root, &[]);

        // Sanity: the link really does lead to the file, so the refusal below
        // is containment doing its job and not the path simply being wrong.
        assert!(root.join("escape").join("secret.env").is_file());

        let bundle = assemble(
            "token",
            &response(&[hit("escape/secret.env", 1, 1, "")]),
            &sources,
            40_000,
            24,
        );
        assert_eq!(bundle.hits_verified, 0);
        assert_eq!(bundle.dropped[0].reason, "missing");
        assert_eq!(bundle.dropped[0].paths, ["escape/secret.env"]);
        assert!(!bundle.text.contains("hunter2"), "{}", bundle.text);
    }

    /// `..` in the middle of an otherwise ordinary path, through a directory
    /// that really exists — the form a naive "does it start with `..`?" guard
    /// would wave through.
    #[test]
    fn indep_a_dotdot_in_the_middle_of_a_path_cannot_escape_either() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let outer = paths::canonicalize_root(dir.path()).expect("a canonical root");
        std::fs::write(outer.join("secret.env"), "TOKEN=hunter2\n").expect("the outside file");
        let root = outer.join("proj");
        std::fs::create_dir(&root).expect("creating the project root");
        std::fs::create_dir(root.join("sub")).expect("creating a real subdirectory");
        std::fs::write(root.join("a.txt"), "alpha\n").expect("a fixture file");
        let sources = Sources::from_declared(&root, &[]);

        let bundle = assemble(
            "token",
            &response(&[
                hit("sub/../../secret.env", 1, 1, ""),
                // ...while a `..` that lands back inside is still served, so
                // the guard is containment and not a ban on the characters.
                hit("sub/../a.txt", 1, 1, "alpha"),
            ]),
            &sources,
            40_000,
            24,
        );
        assert_eq!(bundle.hits_verified, 1);
        assert_eq!(bundle.spans[0].path, "sub/../a.txt");
        assert_eq!(bundle.dropped[0].reason, "missing");
        assert!(!bundle.text.contains("hunter2"), "{}", bundle.text);
    }

    /// An absolute path arriving from the index. On Windows it joins over the
    /// root and resolves to a real file outside it; on POSIX the leading
    /// separator is stripped and it resolves to nothing. Either way it is
    /// refused, which is the property worth asserting.
    #[test]
    fn indep_an_absolute_indexed_path_is_refused() {
        let outside = tempfile::tempdir().expect("a temporary directory");
        let outside = paths::canonicalize_root(outside.path()).expect("a canonical root");
        std::fs::write(outside.join("secret.env"), "TOKEN=hunter2\n").expect("the outside file");
        let fixture = corpus(&[("a.txt", "alpha\n")]);

        let absolute = outside.join("secret.env").as_str().replace('\\', "/");
        let bundle = assemble(
            "token",
            &response(&[hit(&absolute, 1, 1, "")]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.hits_verified, 0);
        assert_eq!(bundle.dropped[0].reason, "missing");
        assert!(!bundle.text.contains("hunter2"), "{}", bundle.text);
    }

    /// (oracle) A leading separator, and Windows separators, are normalized
    /// away for *resolution* — but the path the bundle prints is the index's
    /// own, slashes swapped and nothing else. The prototype does the same, and
    /// it matters: the printed pointer is what the reader pastes back.
    #[test]
    fn indep_a_leading_separator_resolves_but_is_kept_in_the_printed_pointer() {
        let fixture = corpus(&[("m.py", "one\ntwo\n")]);
        for indexed in ["/m.py", "\\m.py"] {
            let bundle = assemble(
                "two",
                &response(&[hit(indexed, 1, 2, "one\ntwo")]),
                &fixture.1,
                40_000,
                24,
            );
            assert_eq!(bundle.hits_verified, 1, "{indexed}");
            assert_eq!(bundle.spans[0].path, "/m.py", "{indexed}");
            assert!(bundle.text.contains("=== /m.py:1-2 ==="), "{}", bundle.text);
        }
    }

    // -- rendering ----------------------------------------------------------

    /// (oracle) The gutter is as wide as the LAST line number and
    /// right-aligned, with two spaces before the source. Full-text assertion,
    /// because the block shape is what the consuming agent parses.
    #[test]
    fn indep_the_gutter_is_sized_by_the_last_line_number() {
        let fixture = corpus(&[("f.txt", &numbered(400, ""))]);
        let bundle = assemble(
            "line",
            &response(&[hit("f.txt", 98, 102, "")]),
            &fixture.1,
            40_000,
            24,
        );
        let mut want = String::from(
            "VERDICT: found (1 verified span(s) from 1 file(s))\n=== f.txt:86-114 ===\n",
        );
        for n in 86..=114 {
            want.push_str(&format!("{n:>3}  line {n}\n"));
        }
        assert_eq!(bundle.text, want);
    }

    /// (oracle) A synthetic `#s0` symbol is dropped in favour of the heading
    /// trail, and empty headings are skipped rather than printed as `> >`.
    #[test]
    fn indep_a_heading_trail_skips_its_empty_members() {
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let mut row = hit("a.txt", 1, 1, "alpha");
        row.symbol_path = Some("#s0".into());
        row.heading_path = Some(vec!["Guide".into(), String::new(), "Setup".into()]);
        let bundle = assemble("alpha", &response(&[row]), &fixture.1, 40_000, 24);
        assert_eq!(
            bundle.text,
            "VERDICT: found (1 verified span(s) from 1 file(s))\n\
             === a.txt:1-1 [Guide > Setup] ===\n1  alpha\n"
        );
        assert_eq!(bundle.spans[0].label.as_deref(), Some("Guide > Setup"));
    }

    /// A label that is only whitespace, and a heading trail that is only
    /// empty strings, both come out as *no* label — and the structured field
    /// says `None` rather than `Some("")`.
    #[test]
    fn indep_an_empty_label_is_absent_rather_than_empty() {
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let mut blank = hit("a.txt", 1, 1, "alpha");
        blank.symbol_path = Some("   ".into());
        blank.heading_path = Some(vec![String::new(), String::new()]);
        let bundle = assemble("alpha", &response(&[blank]), &fixture.1, 40_000, 24);
        assert_eq!(bundle.spans[0].label, None);
        assert!(
            bundle.text.contains("=== a.txt:1-1 ===\n"),
            "{}",
            bundle.text
        );
    }

    /// (oracle) `clip` is 200 *characters* of the trimmed query, and the
    /// boundary must be a character boundary — a multi-byte query that is cut
    /// mid-character would panic rather than truncate.
    #[test]
    fn indep_the_none_detail_clips_the_query_at_two_hundred_characters() {
        let fixture = corpus(&[("a.txt", "alpha\n")]);
        let query = format!("  {}  ", "é".repeat(250));
        let bundle = assemble(&query, &response(&[]), &fixture.1, 40_000, 24);
        assert_eq!(
            bundle.verdict_detail,
            format!("nothing relevant found for: {}", "é".repeat(200))
        );
    }

    /// The reported counts are of the *hits*, not of what survived: a hit
    /// verified and then folded into another span still counts once as
    /// verified, and `hits_returned` counts what search handed over.
    #[test]
    fn indep_the_reported_counts_describe_the_hits_not_the_survivors() {
        let fixture = corpus(&[("f.txt", &numbered(400, ""))]);
        let bundle = assemble(
            "line",
            &response(&[
                hit("f.txt", 1, 20, ""),
                hit("f.txt", 24, 43, ""),
                hit("gone.py", 1, 1, ""),
            ]),
            &fixture.1,
            40_000,
            24,
        );
        assert_eq!(bundle.hits_returned, 3);
        assert_eq!(bundle.hits_verified, 2);
        assert_eq!(bundle.hits_rejected, 1);
        assert_eq!(bundle.spans_after_merge, 1);
        assert_eq!(bundle.spans.len(), 1);
        assert_eq!(bundle.spans[0].merged, 2);
        // `top_score` is the first VERIFIED hit's score, and rounding it must
        // not change it.
        assert_eq!(bundle.top_score, Some(0.03));
    }

    // -- fixtures ----------------------------------------------------------

    /// A temporary corpus and the [`Sources`] that addresses it. The `TempDir`
    /// rides in the tuple so it outlives the test's use of the paths.
    fn corpus(files: &[(&str, &str)]) -> (tempfile::TempDir, Sources) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = paths::canonicalize_root(dir.path()).expect("a canonical root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("writing a fixture file");
        }
        let sources = Sources::from_declared(&root, &[]);
        (dir, sources)
    }

    /// `count` lines of `line <n><suffix>`, newline-terminated.
    fn numbered(count: u32, suffix: &str) -> String {
        (1..=count).fold(String::new(), |mut out, n| {
            out.push_str(&format!("line {n}{suffix}\n"));
            out
        })
    }

    fn response(results: &[SearchResult]) -> SearchResponse {
        SearchResponse {
            results: results.to_vec(),
            lexical_only: false,
        }
    }

    fn hit(path: &str, line_start: u32, line_end: u32, excerpt: &str) -> SearchResult {
        SearchResult {
            chunk_id: "0123456789abcdef".into(),
            project: "fixture".into(),
            project_key: "fixture-0000".into(),
            path: path.into(),
            line_start,
            line_end,
            language: None,
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
}
