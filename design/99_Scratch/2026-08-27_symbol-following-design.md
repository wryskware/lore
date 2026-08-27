# Symbol following — prose is the signpost, the implementation rides along

*2026-08-27. Design proposal, not canon. No implementation, no ledger entry.
Written against lore at HEAD (`daemon/search.rs`, `daemon/bundle.rs`,
`store/mod.rs`, `store/schema.rs`, `chunk/code/mod.rs`) and the shipped bundle
contract in `design/4_Interfaces/2026-08-27_bundle-mcp-tool.md`.*

## The problem, stated narrowly

Natural-language queries match natural language. Lore's two ranking arms both
reward that: BM25 scores word overlap, and the vector arm scores semantic
similarity between a prose question and a prose chunk. A README paragraph that
*describes* the retry policy looks more like "how does the retry policy work"
than the `RetryPolicy` class that *is* the retry policy.

The bench numbers say exactly that, and they say it is a pure ranking problem:

| evidence kind | span recall | file recall |
|---|---|---|
| primary implementing source | 0.15–0.21 | 0.34 |
| sample / doc evidence | 0.47–0.58 | 0.71–0.77 |

Coverage is perfect — every gold span has a covering chunk in the index. So the
implementation is *there*, ranked below the prose that talks about it, and the
prose that outranks it is usually prose that **names it**. A samples file calls
`AgentThread.RunAsync`. A README says "use `ChatClientBuilder`". The signpost is
in the top ten; the thing it points at is at rank 40.

Following that pointer is cheap, exact, and needs no new understanding of the
query. That is the whole idea.

## Recommendation in one paragraph

Add a **follow pass in the bundle path only**, not in `search`. After search
returns and before the store lock is released, look at the top few
prose-adjacent hits (Markdown/doc chunks, and code under `samples/`,
`examples/`, `docs/`-style paths), pull identifier-shaped references out of
their text under a strict specificity rule (must be CamelCase, snake_case, or a
dotted/`::` chain — never a bare lowercase word), resolve each one against the
**existing `chunks_fts.anchor` index** by exact symbol-path tail match, and hand
the resolved definition chunks to `bundle::assemble` as a **separate list**. The
assembler verifies them exactly like any other hit, renders each one immediately
after the span that referenced it, labels it `via <referring path>:<lines>`, and
pays for it out of a **separate allowance on top of the caller's budget** so
nothing that would have rendered is displaced. The verdict is computed from the
ranked spans alone — follow-ins never talk the bundle into confidence.

No schema change. No new table. No re-index. No cache. No query translator.
`search`'s wire bytes are unchanged.

## 1. Where it lives: bundle, not search

**Recommendation: bundle assembly only, in a new `daemon::follow` module that
knows nothing about bundles.**

The mechanism splits cleanly in two: *find and resolve references* (needs the
store, is generic) and *render and budget them* (needs the bundle). Put the
first in `daemon/follow.rs` as a free function over search results, and call it
from `bundle_route` only. If search ever wants it, the module is already there
and the wiring is a request flag — but that is the owner's call to make after
the number lands, not a thing to ship speculatively.

Why not a post-pass inside `search::execute`:

- **It cannot be additive on the search wire and also useful.** `search` returns
  a ranked page of `limit` results, and every field on a result — including
  `score` — means "this is where fusion put it". A followed definition has no
  fused score. Appending it past the page breaks `limit`; interleaving it breaks
  order. Either way a consumer that today reads `results[0..n]` as "the ranking"
  reads something else tomorrow. That is the reordering the owner ruled out
  ("preserve the existing search api"), even if the JSON schema technically only
  grew.
- **Search's ranking has a proof attached and follow is not part of it.**
  `page_is_final` proves that nothing outside the page could still get onto it,
  given the fused scores. Injecting rows after that proof does not invalidate
  it, but it does mean the page is no longer "the top N by score", which is the
  one thing the whole acquisition loop exists to guarantee. Keeping that
  statement true is worth more than the convenience.
- **Search hands out pointers; follow-ins are only worth their tokens when
  rendered.** An extra chunk id and a 2000-char excerpt in a `search` response
  is roughly the cost of the definition itself, with none of the verification.
  In the bundle the definition arrives as line-numbered source read from disk,
  which is the form an agent can actually use without a second call.
- **Search is cross-project; the bundle is scoped to one.** Resolving a symbol
  across every registered project would be a genuinely new (and mostly wrong)
  behaviour. `bundle_route` already resolves exactly one project and holds its
  id, so the scope question does not arise.

Cost of choosing bundle-only: MCP `search` consumers get nothing. That is a real
loss, and it is the thing to revisit once the eval says how big the win is.

### Where in the request flow

`bundle_route` (`daemon/http.rs:897`) already does the right thing: embed the
query outside the lock, then one `store.with(...)` for search, then assembly on
the blocking pool. Follow goes **inside the same closure**, right after
`search::execute`:

```rust
let (results, followed) = state.store.with(move |store| {
    let results = search::execute(store, &search_request, query_vector.as_deref())?;
    let followed = follow::resolve(store, project_id, &results.results, follow_on);
    Ok((results, followed))
}).await??;
```

One lock acquisition, one extra statement inside it, and the daemon stays the
single owner of index state (D-0003). Nothing new touches the filesystem inside
the lock.

## 2. Finding references in a hit's text

Two questions: which hits are signposts, and which tokens in them are
references.

### Which hits

A hit is **prose-adjacent** when either:

- its chunk kind is `Section` (that is the Markdown chunker's output), or its
  language tag is a doc language (`markdown`, `mdx`, `rst`, or `None` for plain
  text); or
- it is a code chunk whose path contains a sample-ish directory segment:
  `samples`, `sample`, `examples`, `example`, `demo`, `demos`, `docs`, `doc`,
  `tutorial`, `tutorials`, `cookbook`, `getting-started`, `snippets`.

Only the **top `FOLLOW_TOP_HITS = 5`** ranked hits are considered. A rank-20
prose hit is a weak signpost, and the cost of following it is the same as the
cost of following a good one.

Tests and benchmarks are deliberately **not** in the trigger set for v1. Test
directories are dense with symbol references and would dominate the candidate
set on every query, and test files are frequently the gold evidence themselves
(so they are already being ranked on their own merits). Open question 3 below
leaves this for the owner.

### Which tokens

Scan the hit's `excerpt` (the wire field, already the chunk's own text, clipped
at 2000 chars). Two extraction modes, because the two hit kinds have different
signal:

**Markdown / doc chunks.** Only take candidates from places the author marked as
code: inline backtick spans, fenced code blocks, and dotted or `::` chains
anywhere in the running text (`AgentThread.RunAsync`, `store::vector_search`).
Prose that merely capitalizes a word — "the Agent loop", "our Store" — is not a
reference, and the backtick rule excludes it for free. This is a large precision
win for one line of parsing, and it costs almost nothing in recall: authors who
name a symbol in a doc nearly always mark it up.

**Sample/example code chunks.** The whole chunk is code, so backticks do not
apply. Take tokens in *reference position*: immediately followed by `(` or `<`,
immediately preceded by `new `, or part of a dotted/`::` chain. Bare
declarations and locals are skipped by construction.

**Then the specificity floor, applied to every candidate:**

1. At least 4 characters.
2. Must be **multi-part**: contains `_`, or splits into ≥2 case runs by the
   bundle's existing `case_parts` (which already handles `HTTPServer` →
   `HTTP` + `Server` and is already tested), or arrived as part of a dotted /
   `::` chain (in which case the *last* segment is the name and the chain is
   kept for disambiguation).
3. Not a stopword after lowercasing — reuse `bundle::STOPWORDS`, which already
   contains `run`, `code`, `source`, `set`, `get`.

Rule 2 is what kills the common-word problem outright. `run`, `main`, `get`,
`parse`, `send` are single lowercase words and are rejected without a special
case; `Board.run` survives because the chain makes it specific. No new
hand-maintained list of forbidden names is needed, which matters because such a
list is unmaintainable across languages.

Cap at `FOLLOW_MAX_CANDIDATES_PER_HIT = 12` distinct names per hit, in document
order, so an API-reference chunk cannot generate eighty lookups.

## 3. Resolving a symbol — no new store surface, no migration

**Recommendation: reuse `chunks_fts.anchor`. Zero schema change.**

Every chunk already writes an `anchor` string (`ChunkKind::anchor()`):
`code:Board.Update` for code, `md:A > B` for sections, `win:3` for text windows.
That column is in the FTS5 index and is BM25-weighted at 2.0 today. The
tokenizer is `unicode61 remove_diacritics 2 tokenchars '_'`, so `.`, `:`, `#`
are separators and `code:Lexomancy.Board.Update#w1` tokenizes to `code`,
`Lexomancy`, `Board`, `Update`, `w1`. A column-scoped MATCH on the anchor column
therefore finds every chunk whose symbol path contains a given segment, from an
index that already exists and is already maintained by the sync triggers.

New store method, one statement per bundle request:

```rust
/// Candidate definitions for a batch of symbol names, cheaply.
pub fn symbol_anchor_candidates(
    &self,
    filter: &SearchFilter,
    names: &[String],       // already validated: [A-Za-z0-9_]+ only
    limit: usize,
) -> Result<Vec<AnchorCandidate>>
```

- Expression: `{anchor} : ("Update" OR "RunAsync" OR ...)`, capped at
  `FOLLOW_MAX_TERMS = 32` names — mirroring `query::MAX_TERMS`. Quoting is
  unreachable-safe because the specificity filter has already guaranteed the
  names are alphanumeric plus underscore.
- **Projection is deliberately narrow**: `c.chunk_id, c.path, c.kind,
  c.line_start, c.line_end, c.language`. Not `c.text`. A hot token could match
  thousands of anchors, and hydrating their text would be the only expensive
  thing in this design. `LIMIT FOLLOW_SCAN_ROWS = 400`.
- Filtering to the exact definition happens **in Rust**, on the narrow rows:
  - kind must be `Code`;
  - strip a trailing `#w<n>` window suffix from `symbol_path`, then require the
    last dotted segment to equal the reference name **case-sensitively** (FTS
    folded case to find the row; the author's spelling decides whether it is a
    real reference);
  - when the reference arrived as a chain (`Board.Update`), prefer candidates
    whose symbol path agrees on more trailing segments;
  - reject `symbol_kind == "statements"` and any anchor tail starting with `#s`
    — those are the chunker's filler spans and name nothing a reader can use
    (`bundle::label` already refuses to print them);
  - accept `symbol_kind == "group"` — a merged run of tiny members keeps its
    first member's symbol path, so it genuinely is that symbol's definition;
  - prefer window index 0 when the definition was split, because that is the
    window carrying the signature;
  - drop anything whose `chunk_id` is already in the search results.
- Winners (at most `FOLLOW_MAX_TOTAL = 6` across the whole request) are then
  hydrated with the existing `get_chunk`, and turned into `SearchResult`s by
  `search::to_result`, which must become `pub(crate)`. Reusing it is not tidiness
  — `verify()` depends on `excerpt` and `excerpt_truncated` having exactly the
  meaning search gives them, and a second constructor would eventually disagree.

**Why not a real symbol table.** A `symbols(project_id, name, chunk_id)` table
with an index on `name` would be a nicer lookup and is the obvious instinct. It
is the wrong trade here: under D-0018 there are no migrations, so adding a
column or table bumps `schema::VERSION`, and `Store::open` then **discards and
rebuilds the whole database** — which means re-chunking every project *and
re-embedding every chunk*. That is minutes to hours of local GPU time on the
owner's corpora, paid by every user of every lore build, to buy a lookup the FTS
index already answers in single-digit milliseconds. Revisit only if measurement
shows the anchor scan is actually a bottleneck (it will not be) or when the
first tagged release ends the no-migration posture and migrations become
affordable.

## 4. Budget, ranking, and how a follow-in is labelled

**Ranking.** Follow-ins are not ranked. They are attached to the span that
referenced them and rendered immediately after it, so the bundle reads
signpost → implementation, signpost → implementation. The ranked spans keep
their order exactly; `bundle.rs`'s "the order search ranked in is preserved
throughout" stays true of the ranked spans, and the module header gains one
sentence saying follow-ins are interleaved by provenance rather than by score.

**Cap.** At most 2 definitions per symbol (`FOLLOW_MAX_DEFS_PER_SYMBOL`), at
most 3 per referring hit, at most 6 per bundle. Deterministic ordering by
`(path, line_start)` so two runs of the same query produce the same bundle.

**Budget.** Follow-ins get an allowance **on top of** `budget_tokens`:
`FOLLOW_BUDGET_SHARE = 0.35`, i.e. up to 35% more rendered text. They are
budgeted after all ranked spans have been placed, out of their own pot, so a
span that would have rendered never loses its slot to a definition. This is the
one place the design deliberately spends more tokens, and it is disclosed:

- the bundle contract already says `budget_tokens` bounds rendered spans only
  and can be exceeded (the first span always renders) — this extends that
  sentence rather than contradicting it;
- the header line names the cost;
- `follow: false` turns it off entirely.

The alternative — carving the 35% out of the existing budget — was rejected
because the eval already shows the 4000-token budget demoting gold evidence to
FURTHER READING (`bundle_all` outscores `bundle_rendered` on span recall). Taking
tokens away from ranked spans to make room for definitions would trade one recall
loss for another and muddy the measurement completely.

Follow-ins that do not fit their allowance go to FURTHER READING like anything
else, with their `via` annotation attached.

**Verdict.** *Follow-ins do not participate in coverage.* The coverage blob is
built from ranked spans only. Two reasons, and the second is the load-bearing
one: the thresholds (0.65 / 0.45) were calibrated on twenty judged cells with no
follow-ins in the corpus of measurement, and a bundle that pulls in extra text
and then grades itself on that text can talk a `none` into a `weak`. The verdict
is a claim about what the *retrieval* found; a definition lore chose to include
because a doc mentioned it is not evidence that the retrieval found the answer.
This also makes the eval clean: verdict distribution should be **bit-identical**
with follow on and off, which is a free regression check.

**Labelling — the honesty requirement.** In the text:

```
FOLLOWED: 3 definition(s) pulled in because a doc or sample above names them.
...
=== samples/GettingStarted/Program.cs:12-40 [Program.Main] ===
  12  var thread = agent.GetNewThread();
  ...
=== src/Agents/AgentThread.cs:88-141 [AgentThread.RunAsync] (via samples/GettingStarted/Program.cs:12-40) ===
  88  public async Task<AgentRunResponse> RunAsync(...)
  ...
```

In the JSON, `BundleSpan` and `BundleSpanRef` gain one optional field:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub via: Option<BundleVia>,

pub struct BundleVia {
    pub path: String,        // the referring span
    pub line_start: u32,
    pub line_end: u32,
    pub symbol: String,      // the reference that resolved
}
```

Absent on every ranked span, so a consumer that ignores it sees today's shape.
`BundleResponse` gains `followed: u32` (rendered) and `followed_dropped: u32`
(resolved but failed verification), because a count nobody can see is a claim
nobody can check.

`BundleRequest` gains `follow: Option<bool>`, defaulting to true if the eval
clears the bar (open question 2).

## 5. Cost

Per bundle request, on top of what it does today:

| step | cost | bounded by |
|---|---|---|
| candidate extraction | pure string scan over ≤5 excerpts of ≤2000 chars | `FOLLOW_TOP_HITS`, excerpt cap |
| anchor lookup | **one** FTS5 column-scoped MATCH, narrow projection, `LIMIT 400` | `FOLLOW_MAX_TERMS`, `FOLLOW_SCAN_ROWS` |
| hydration | ≤6 `get_chunk` point lookups on a unique index | `FOLLOW_MAX_TOTAL` |
| verify + render | 2 file reads per follow-in (verification, then render-time re-read) | `FOLLOW_MAX_TOTAL` |
| rendered tokens | ≤35% more bundle | `FOLLOW_BUDGET_SHARE` |

Search settles in one acquisition round on ordinary corpora and assembly runs
0.15–0.94 s, dominated by file IO. The added SQL is one statement against an
index that is already warm — low single-digit milliseconds. The added file IO is
≤12 reads, which is roughly half of what a 24-hit bundle already does. **Expect
under 60 ms added at the median**, and a strictly smaller relative cost on the
slow tail (which is IO on large files, not lookups).

Where the cliffs are, and why each is fenced:

- **Candidate explosion.** An API-reference Markdown chunk with two hundred
  backticked identifiers would build a two-hundred-term OR expression. Fenced by
  `FOLLOW_MAX_CANDIDATES_PER_HIT` and `FOLLOW_MAX_TERMS`.
- **Hot token.** A name like `Client` appearing as a segment of thousands of
  symbol paths would make the anchor scan return thousands of rows. Fenced three
  ways: the multi-part specificity rule makes such names rarer than intuition
  suggests, the projection excludes `text` so a row is tens of bytes, and
  `LIMIT 400` caps it outright. The consequence of hitting the limit is a missed
  follow-in, never a slow query.
- **Wide corpora.** The lookup is filtered to one project by `SearchFilter`, so
  it does not grow with the number of registered projects.
- **The lock.** The extra statement runs inside the store lock, so it delays
  every other query by its own duration. That is why it is one batched statement
  and not one per symbol; twelve round trips inside the lock would be the wrong
  shape even if each were fast.

## 6. Failure modes, and what each one does

| situation | behaviour | why that is right |
|---|---|---|
| **Overloads** — `Parser.Parse(string)` and `Parser.Parse(Stream)` share a symbol path | pull both (cap 2), same file, likely merged into one span by the existing merge step | both really are the definition; showing one and hiding the other would be a lie by omission |
| **Same name in several files** — 3–4 definitions | pull the first 2 by `(path, line_start)`; the annotation says `via … (1 of 3 definitions)` | honest about the ambiguity without spending the budget on it |
| **More than `FOLLOW_AMBIGUITY_CAP = 4` definitions** | skip the symbol entirely, silently | a name that means five things is not a signpost; following it is noise, and there is nothing useful to say about it |
| **Stale index** — the definition chunk no longer matches disk | dropped by the *existing* `verify()` staleness check, counted in `dropped` under reason `follow:stale`, and in `followed_dropped` | the bundle's core guarantee — rendered text came from disk — applies to follow-ins with no new code; the separate reason keeps the ranked-hit DROPPED tally readable |
| **Symbol lives in an unindexed dependency** | resolves to nothing; nothing is added; nothing is said | lore never claims "not found" for a symbol the caller did not ask about. The `NO MATCH FOR:` line is about *query terms*, and inventing a second not-found channel for tokens lore chose to look up itself would be noise |
| **Definition already in the bundle** | deduplicated by `chunk_id`, and again by overlapping `(path, line range)` after merge | otherwise the highest-value case (a doc next to its own implementation) renders twice |
| **Prose-name reference** — "the concurrent orchestrator", "the retry logic" | **not solved, deliberately** | resolving an English description to a symbol is query translation, and lore has none, ever (D-0003 posture, bundle contract non-goals). Symbol following is exact-name only, and the design should never grow a fuzzy-name mode without a decision |
| **Vault / design query where prose IS the answer** | prose hits are untouched and keep every slot they had; at most 6 labelled definitions may appear after them, costing ≤35% more tokens; `follow: false` removes even that | strict additivity is the guarantee. Note that a design doc quoting `` `SearchFilter` `` *will* pull that struct in — which is usually helpful and always visibly labelled, but it is a real behaviour change for vault users and the eval should report it |
| **Windowed definition** — the symbol is oversized and split | prefer window 0, and let the existing merge fold adjacent windows if both arrive | window 0 carries the signature, which is what a reader following a pointer wants first |
| **Follow-in is oversized (>160 lines)** | goes to FURTHER READING with its `via` annotation, like any oversized span | half a definition is worse than a pointer to one; the existing rule already says so |

## 7. Eval

`bench/rcb/retrieval_eval.py` already computes everything needed except the
comparison itself. Three concrete changes.

**a. A surface that speaks to the daemon.** The eval currently drives the Python
prototype through `lore_pkg.build_bundle`, and reads `span_index` /
`spans_overflowed` — the prototype's field names, which the shipped
`BundleResponse` does not use (it has `spans` / `further_reading`). Measuring
the daemon's follow pass therefore needs a new surface function that
`POST`s `/v1/bundle` and maps the shipped names, plus two new configs:

```python
CONFIGS = ["search@10", "search@24", "bundle_rendered", "bundle_all",
           "dbundle", "dbundle+follow", "dbundle_all", "dbundle_all+follow"]
```

`dbundle*` runs `follow: false`, `dbundle*+follow` runs `follow: true`, same
query, same limit, same budget. The gap between the pair **is** the effect, with
no other variable moving. Both `question` and `brief` variants, as today.

**b. Report recall split by evidence strength.** `score_hits` already carries
`strength` per item but only surfaces it in the miss list. Add an aggregate table
keyed on `evidence_strength` (primary vs supporting), because the headline claim
is specifically about primary implementing source — that is the 0.15–0.21 number
this design exists to move, and a mean over all evidence would dilute it into
invisibility. Worth adding alongside: a source-vs-doc split by file extension, so
"did source recall rise" is answerable directly.

**c. Report the costs next to the win.** Three guard metrics, printed in the
same table:

- `distractor_files_hit` — must not rise. Following references into distractor
  files is the precision failure this design can plausibly cause.
- `bundle_tokens_est` — the price. Should land within the 35% allowance.
- verdict distribution — must be **identical** between the pair, since coverage
  ignores follow-ins. Any difference is a bug, not a result.

**Success bar, stated before the run:** primary-evidence `span_recall_half` up
by ≥0.10 absolute on `dbundle+follow` vs `dbundle`, with distractor hits flat or
lower, tokens up ≤35%, and verdicts unchanged. Below that bar the feature is not
worth its tokens and should not default on.

Also worth recording per run, straight out of the response: how many follow-ins
were rendered, how many symbols resolved to nothing, and how many were skipped
as ambiguous. Those three numbers tell you *why* a disappointing result was
disappointing — bad extraction, bad resolution, or bad ranking upstream — and
they cost nothing to emit.

## Implementation sketch, file by file

**`crates/lore-core/src/lib.rs`**
- `BundleRequest`: `+ follow: Option<bool>`.
- `BundleSpan`, `BundleSpanRef`: `+ via: Option<BundleVia>` (serde-skipped when
  absent, so today's JSON is byte-identical when nothing follows).
- `+ struct BundleVia { path, line_start, line_end, symbol }`.
- `BundleResponse`: `+ followed: u32`, `+ followed_dropped: u32`.
- `SearchResult`, `SearchResponse`: **unchanged**.

**`crates/lore/src/daemon/follow.rs`** (new, ~250 lines + tests)
- `pub struct Followed { pub hit: SearchResult, pub via: BundleVia }`
- `pub fn resolve(store: &Store, project: ProjectId, hits: &[SearchResult],
   enabled: bool) -> Vec<Followed>` — the whole pass; returns empty when
  disabled, when no hit is prose-adjacent, or when nothing resolves.
- `fn is_prose_adjacent(hit: &SearchResult) -> bool`
- `fn candidates(hit: &SearchResult) -> Vec<Reference>` — the two extraction
  modes and the specificity floor. Reuses `bundle::case_parts` and
  `bundle::is_stopword`, which means promoting both to `pub(crate)` (they are
  already tested where they live).
- `fn pick(candidates, rows) -> Vec<Followed>` — tail matching, filler
  rejection, window preference, ambiguity cap, dedupe, caps.
- All constants live here with their reasoning, matching the module-doc style of
  `search.rs` and `bundle.rs`.

**`crates/lore/src/store/mod.rs`**
- `+ pub struct AnchorCandidate { chunk_id, path, kind, line_start, line_end }`
- `+ pub fn symbol_anchor_candidates(&self, filter, names, limit)` — the single
  narrow-projection FTS statement described in §3.
- No change to `CHUNK_COLS`, `row_to_chunk`, the schema, or anything the indexer
  writes.

**`crates/lore/src/daemon/search.rs`**
- `to_result` becomes `pub(crate)`. That is the entire diff. Ranking, fusion,
  collapse, acquisition and the wire shape are untouched.

**`crates/lore/src/daemon/bundle.rs`**
- `assemble(query, results, followed: &[Followed], sources, budget_tokens, limit)`.
- `Span` gains `via: Option<BundleVia>`.
- Verification: follow-ins go through the same `verify()`; refusals are tallied
  under a `follow:`-prefixed reason.
- After merge and the oversize partition, follow-in spans are **placed** after
  the ranked span they came from rather than appended to the tail, then rendered
  from their own allowance.
- `render_span` prints ` (via path:start-end)` when `via` is set.
- Coverage blob construction explicitly excludes follow-in spans — one filter,
  with a comment saying why, because it is the kind of line a later reader would
  "fix".
- Header gains the `FOLLOWED:` line when any rendered.

**`crates/lore/src/daemon/http.rs`**
- `bundle_route`: read `request.follow`, call `follow::resolve` inside the
  existing `store.with` closure, pass the result through to `assemble`.

**`crates/lore-mcp`**
- Renders `text` verbatim already, so: nothing, unless `follow` is exposed as a
  tool parameter (recommend not exposing it — it is an eval and escape-hatch
  knob, not an agent-facing one; the daemon endpoint keeps it either way).

**Docs**
- `design/4_Interfaces/2026-08-27_bundle-mcp-tool.md` gains a symbol-following
  section and the amended budget sentence. No ledger entry without Wrysk's
  explicit sign-off.

## Test plan

Authored as its own pass, not by whoever writes the implementation.

**`follow.rs` unit tests** (no store, pure functions)
- CamelCase, snake_case and dotted references are extracted; `run`, `get`,
  `main`, `code`, `set` are not.
- In a Markdown chunk, a backticked `RunAsync` is a candidate and a bare
  capitalized "Agent" in prose is not; a fenced code block contributes.
- In a sample code chunk, `Foo(` and `new Bar` are candidates; a bare local is
  not.
- `HTTPServer` splits the way `case_parts` says it does (guards against a second
  divergent splitter creeping in).
- Per-hit candidate cap holds; term cap holds.
- Only the top 5 hits and only prose-adjacent hits are scanned.

**Store test** (temp db, real schema)
- `symbol_anchor_candidates` finds a symbol by its tail across dotted prefixes.
- A `#w1` window's anchor still matches, and the tail comparison strips the
  suffix.
- Case: FTS finds `parse` for `Parse`, and the Rust tail filter rejects it.
- Filler chunks (`#s0`, `symbol_kind = "statements"`) never resolve.
- A `group` chunk does resolve, for its first member's name.
- Unknown name → empty, no error.
- The `LIMIT` is respected on a corpus with many same-tail anchors.

**Bundle tests** (temp corpus, as the existing ones are written)
- A doc hit naming a symbol whose definition exists renders the definition
  immediately after it, with the `via` header and the `via` JSON field.
- The ranked spans that rendered without follow still render, in the same order,
  with follow on — the strict-additivity assertion, and the most important test
  in the file.
- Verdict, `coverage`, `terms_covered` and `terms_uncovered` are identical with
  follow on and off, on a fixture where the definition contains an otherwise
  uncovered term. This is the calibration guard.
- A definition already present as a ranked hit is not duplicated.
- Five definitions of one name → the symbol is skipped; three → one is shown and
  the annotation says so.
- A stale definition is dropped under `follow:stale` and counted, and the ranked
  DROPPED lines are unaffected.
- An oversized definition goes to FURTHER READING carrying its `via`.
- `follow: false` produces a bundle byte-identical to today's.
- A vault-shaped fixture (design Markdown, no code) is byte-identical with
  follow on — nothing to follow, nothing changes.

**Wire test**
- A `search` response over a corpus that would produce follow-ins is
  byte-identical to the pre-change response. This is the "preserve the existing
  search api" promise, asserted rather than argued.

## Open questions — genuinely the owner's

1. **Does `search` eventually get this too?** The recommendation is no for v1,
   because it cannot be done without changing what a ranked page means. If the
   bundle number is large, the honest options are an opt-in `follow` request
   flag returning follow-ins in a *separate* response array (not `results`), or
   leaving `search` alone forever and treating the bundle as the surface where
   lore does work on the caller's behalf. Wrysk's call, after the eval.
2. **Default on or off?** Recommendation: on, if the eval clears the bar in §7,
   because a feature that ships default-off is a feature nobody measures in the
   wild. Shipping default-off for one round to compare live behaviour is equally
   defensible and costs only a flag flip.
3. **Do `tests/` and `benchmarks/` count as prose-adjacent?** Excluded in v1 for
   the reasons in §2. The eval could answer this cheaply as a third config, but
   only if the owner wants that variable in this round.
4. **Is the follow allowance on top of the budget, or carved out of it?** §4
   recommends on top, and disclosed. This is the one place the design knowingly
   costs the owner tokens, and tokens were an explicit selling point of the
   bundle (4× cheaper than iterative search) — so it deserves a deliberate yes.
5. **Should follow-ins ever feed coverage?** §4 recommends never, to protect the
   calibration. The counter-argument is that the bundle really does contain the
   term, so calling it uncovered is its own small dishonesty. If the owner
   prefers the other reading, the fix is a second reported number
   (`coverage_with_followed`) rather than moving the verdict.
