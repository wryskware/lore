# Context bundles on the MCP surface (`bundle` tool)

*2026-08-27. Proposal — implementation authorized by Wrysk (2026-08-27:
"worth rolling into standard lore … add it as part of the mcp surface and
preserve the existing search api"). Not canon; no ledger entry yet — an
accepted-decision entry is pending Wrysk's explicit sign-off once the
implementation settles the open questions below.*

## What and why

A new MCP tool (working name `bundle`; final name open) that answers a
natural-language retrieval query with a **finished evidence bundle**: a
verdict line, then verified code/doc spans rendered with line numbers from
the files on disk, under a caller-set token budget, with overflow paths
listed as further reading and an honest statement of what found no match.

Evidence (RCB, 2026-08-26, all glm-judged, gpt-5.6-luna @ high, 20 cells
each; bench/rcb/RESULTS.md is the ledger):

| arm | wall | tokens | quality/pass |
|---|---|---|---|
| lore search via MCP (today) | 46.2 s | 138k | 0.75 / 0.95 |
| lore bundles (bench prototype) | 50.4 s, median 32.5 | 110k, median 32k | 0.71 / 0.80 |

The median bundle cell is faster and ~4× cheaper than the current MCP
integration; the mean carries a tail of cells where the agent ignored the
trust discipline (agent-behavior, not assembler — 0 assembler errors, all
spans verified). Quality sits slightly below iterative search. Decision:
ship the bundle **alongside** `search`/`expand`, unchanged — hosts compare
and switch deliberately; the bundle is also the designated starting point
for the later trained scouter (bundle first, explore from there).

## Contract

Input: `query` (agent-written; there is deliberately no translator layer),
`project` (resolved as today), optional `budget_tokens` (default 4000),
optional `limit` (ranked chunks considered, default 24).

Output (single text block for the agent, plus structured fields):

```
VERDICT: found (N verified span(s) from M file(s)) | weak | none
NO MATCH FOR: <query terms with no coverage — always printed when nonempty>
DROPPED (<reason>, <count>): <up to 8 paths>        [one line per reason, when any]
=== path:start-end [breadcrumb] ===
<line-numbered source, read from disk at render time>
...
FURTHER READING: <verified paths that exceeded the budget, capped at 20>
```

Contract fine print (as implemented; verified against the prototype by an
independent differential test pass):

- `budget_tokens` bounds the **rendered ranked spans only** — header,
  DROPPED, FOLLOWED and FURTHER READING sit outside it, and the first span
  always renders, so the full block can exceed the nominal budget. Symbol
  following (below) adds a second allowance of up to 35% *on top of* it,
  which the `FOLLOWED:` header line names in tokens.
- A `none` verdict is trimmed to ~1200 tokens regardless of the requested
  budget (the closest-matches courtesy render, not a full bundle).
- FURTHER READING truncates at 20 paths with no marker in the text; the
  structured `further_reading` field keeps them all.
- A hit whose stored excerpt was truncated by the indexer is verified on
  path + range only — the staleness comparison is skipped for it. This is
  a real, deliberate hole in the "rendered text came from disk" claim's
  *comparison* step (the render itself is still from disk).

## Design (daemon-side, per Wrysk)

Assembly lives in the **daemon**, not lore-mcp: one implementation that
the MCP tool, the CLI, and any future surface all call (new endpoint,
e.g. `POST /v1/bundle`). lore-mcp gains a thin tool wrapper. D-0003 is
untouched: the daemon remains the single authoritative owner of index
state and of source rendering.

Pipeline (port of the validated bench prototype
`bench/rcb/sandbox/lore_pkg.py`, whose calibration ran 20 judged cells):

1. search (existing fused retrieval) → top `limit` chunks;
2. verify each hit: realpath containment, line range vs actual file
   length; **render text from the file on disk, never from the index** —
   compare against the index excerpt and demote mismatches as `stale`;
3. widen short chunks (<16 lines) via expansion, merge touching same-file
   spans;
4. budget: spans that fit render in rank order; overflow demotes whole
   spans to FURTHER READING (never truncate mid-span);
5. verdict from **term coverage, not retrieval score**.

Two implementation traps the prototype hit, preserved as requirements:
the code chunker stores dedented text (compare dedented, render raw), and
files may carry a BOM (strip before compare).

## Symbol following (implemented and measured 2026-08-27)

*Status: shipped behind `follow`, **default off**. The success bar was
stated before the measurement
(`design/99_Scratch/2026-08-27_symbol-following-design.md` §7: primary
span_recall_half +0.10, distractors flat, tokens ≤+35%, verdicts
identical) and the retrieval eval came in under it: every guard held
perfectly — verdict distributions bit-identical, distractors flat,
tokens +9–11%, 0 of 35 follow-ins dropped — but primary half-coverage
did not move (span_any rose only +.02–.04, ≈2 gold items, n=18). Per
the pre-commitment, the extra tokens are opt-in. The path back to
default-on is a consumption-side result: an RCB answerer round with
`follow: true` showing wall/token/quality gains, not a recall metric.
Numbers: `bench/rcb/scores/retrieval_eval.integrated.jsonl`
(`dbundle` vs `dbundle+follow` pairs).*

Natural-language queries match natural language, and both ranking arms
reward that. On the RCB corpus, span recall for **primary implementing
source** sat at 0.15–0.21 while sample/doc evidence sat at 0.47–0.58,
with coverage perfect — the implementation was always indexed, ranked
below the prose that *names* it.

So the bundle path (only — `search` is untouched) follows that pointer.
After search and before the store lock is released, the top 5
prose-adjacent hits (doc chunks, and code under `samples/`-shaped paths;
`tests/` and `benchmarks/` deliberately excluded in v1) are scanned for
identifier-shaped references, and each is resolved against the existing
`chunks_fts.anchor` index by exact symbol-path tail match. No schema
change, no new table, no re-index, one batched FTS statement.

Contract additions, all serde-skipped when absent, so a bundle with
nothing followed is byte-identical to one from before this existed:

- request: `follow` (bool, default false — see status above);
- `spans[]` / `further_reading[]`: `via { path, line_start, line_end,
  symbol }`, present **only** on a followed span;
- response: `followed`, `followed_dropped`;
- text: a `FOLLOWED: N definition(s) … costing T tokens on top of the
  B-token budget.` header line, and ` (via <path>:<lines>)` on each
  followed span header.

Three properties are the fence, and each is asserted:

- **Strictly additive.** Ranked spans widen, merge and budget exactly as
  they do with following off; a definition is never merged into one, and
  one overlapping a ranked span is dropped rather than shown twice.
- **Paid for separately**, at up to 35% on top of `budget_tokens`, and
  disclosed. A `none` verdict spends nothing on it.
- **Never evidence.** Coverage, and therefore the verdict, is computed
  from ranked spans alone — the 0.65/0.45 cuts were calibrated on twenty
  judged cells with no follow-ins in them.

Exact-name only, forever: resolving "the concurrent orchestrator" to a
symbol is the query-translation layer this contract's non-goals rule out.

## Why term coverage, not score

lore's fusion is RRF: score = Σ 1/(60+rank) — a pure function of rank.
Measured on the bench corpus: a nonsense query's top hit (0.0294)
outscored the #2 hit of a well-answered query. Any score threshold
therefore manufactures confident `found` on empty results. The prototype's
verdict instead measures how many meaningful query terms are covered by
the returned spans (`found` ≥ 0.65, `weak` ≥ 0.45, else `none`), with a
stopword list for retrieval-brief meta-vocabulary ("identify", "locate",
"usage", …). Uncovered terms always print — this is the honest-gap signal
that lets a consuming agent know what to go find itself, and it is what
fixed the benchmark's unanswerable-task failure (a confident `found` on a
question the corpus cannot answer).

Calibration from the bench round (thresholds are constants to revisit,
not canon): real queries 0.72–1.00, half-answerable ~0.50, junk ≤0.44.

## Non-goals (decided)

- No bundle caching (seconds of assembly never earns invalidation
  complexity — Wrysk, 2026-08-26).
- No translator/query-rewriting layer, ever; the calling agent writes the
  query.
- No steering/compliance tuning shipped with the tool: how hosts instruct
  agents to consume bundles is productization, deferred until multi-model
  evidence exists.
- `search`/`expand` unchanged, kept indefinitely for comparison.

## Open questions — settled by the implementation (2026-08-27, b75c233)

- Tool name: **`bundle`** (`context` collides with the product's own
  vocabulary).
- Span widening: **direct from-disk** — verification already holds the
  file, and the widen arithmetic was proven equivalent to `expand`'s by
  differential test against the prototype's round-trip. Assembly runs
  ~0.15–0.94 s live.
- Thresholds/stopwords: **constants**, documented in the module — a
  `.lore.toml` knob would publish calibration from one corpus; revisit
  after a second corpus is measured.
- **JSON + text**: the endpoint returns structured JSON with `text` as one
  field; the MCP tool renders `text` verbatim.
- CLI `lore bundle` subcommand: **deferred** (not trivial, not asked for).

Still open: whether the rendering budget should be raised or made
rank-aware — the retrieval-recall eval showed the 4000-token budget
demoting gold evidence to FURTHER READING (`bundle_all` outscores
`bundle_rendered` on span recall).

## Provenance

Bench prototype and rounds: `bench/rcb/sandbox/lore_pkg.py`,
`bench/rcb/rounds/luna-lore-pkg-1.jsonl`, probes and trajectories under
`bench/rcb/rounds/traj/`. Comparative context and judge conventions:
`bench/rcb/RESULTS.md`. Plan/decision trail:
`design/99_Scratch/2026-08-26_context-package-path-forward.md`.
