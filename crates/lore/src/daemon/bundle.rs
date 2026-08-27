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
    BundleDropped, BundleResponse, BundleSpan, BundleSpanRef, SearchResponse, SearchResult,
};

use crate::sources::Sources;

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

fn is_stopword(word: &str) -> bool {
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
fn case_parts(chunk: &str) -> Vec<&str> {
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
            continue 'next;
        }
        out.push(span);
    }
    out
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The span's real lines, numbered, under a `path:start-end [label]` header.
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
    sources: &Sources,
    budget_tokens: u32,
    limit: u32,
) -> BundleResponse {
    let mut good: Vec<Span> = Vec::new();
    // Ordered so the `DROPPED` lines come out in a stable order rather than a
    // hash order that changes between runs.
    let mut refused: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for hit in &results.results {
        match verify(hit, sources) {
            Ok(span) => good.push(span),
            Err((reason, where_)) => refused.entry(reason.as_str()).or_default().push(where_),
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

    let budget_chars = budget_tokens as usize * CHARS_PER_TOKEN;
    let mut rendered: Vec<(Span, String)> = Vec::new();
    let mut overflow: Vec<Span> = oversized.clone();
    let mut used = 0usize;
    for span in spans {
        let Some(block) = render_span(&span) else {
            refused
                .entry(Refusal::Unreadable.as_str())
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

    parts.extend(rendered.iter().map(|(_, block)| block.clone()));
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
        spans: rendered
            .iter()
            .map(|(span, _)| BundleSpan {
                path: span.path.clone(),
                line_start: span.start,
                line_end: span.end,
                label: Some(span.label.clone()).filter(|label| !label.is_empty()),
                merged: span.merged,
                chunk_id: span.chunk_id.clone(),
            })
            .collect(),
        further_reading: overflow
            .iter()
            .map(|span| BundleSpanRef {
                path: span.path.clone(),
                line_start: span.start,
                line_end: span.end,
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
        text,
    }
}

/// The first `max` characters, on a character boundary.
fn clip(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

fn round3(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

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
