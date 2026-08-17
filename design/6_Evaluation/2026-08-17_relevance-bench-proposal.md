---
design_status: exploration
last_reviewed: 2026-08-17
---

# Relevance bench — proposal

A proposal, not a decision. Nothing here binds until Wrysk says so.

Question it answers: *for a given search, how much of what came back was
actually relevant, and where in the ranking did the right answers land?*

## Status — built 2026-08-17

Wrysk answered the three open questions below (2026-08-17) and the harness
was built to those answers:

- **LLM judge**, luna free tier through opencode, batched — not hand-labelled.
- **`bench/embed/` renamed to `bench/retrieval/`**, model matrix preserved
  and expected to matter again only when a new embedding model lands.
- **D-0003's "local-only" is not what Wrysk meant** and does not gate this:
  where the embedding engine runs is uncontroversial, *supporting local
  always* is the actual constraint. The ledger entry wording is inaccurate to
  intent and should be amended — pending, unauthorized, not done here.

Shipped: `query.ps1` (search loop, split out of `run.ps1` so any running
daemon can be scored), `judge.ps1` + `opencode-judge.jsonc` (pooling,
labelling, content-hash cache), `score.ps1` extended with precision, ranks,
graded nDCG, worst offenders and the calibration line. The query-set gaps
below (breadth and negative queries) are **not** built.

**Found while verifying** — the 2026-08-15/16 `lexical` control runs recorded
zero results for all 80 queries, in every run: the "floor" that summary tables
reported as 0.00 was an empty arm, not a measurement. The arm reproduces
correctly on the current binary (lore-bench hit@10 0.92, terrarium 0.80,
lexomancy 0.63). D-0012 does not rest on it — its evidence is the C#-semantic
gap between embedders — but no margin over the floor was ever real, and on
lore-bench today BM25 alone matches or beats every recorded embedding arm.
Cross-dating is not controlled: different binaries, and lexomancy's chunk
count has since changed materially under the D-0020 ignore stack.

## What already exists

Three evaluation layers ship today, and the middle one is the one people
forget:

| layer | where | asks |
| --- | --- | --- |
| end-task | `bench/` + [[2026-08-17_e2e-round-2-task-set]] | does an agent do the job better with lore on? |
| retrieval quality | `bench/retrieval/` | does the known-correct file come back, and how high? |
| cost/latency | `bench/retrieval/` score tables, `bench/latency.py` | what does it cost to index and to query? |

So retrieval quality **was** built — it just wears an embedding-model
comparison's clothes. `bench/retrieval/queries/*.json` holds 80 hand-verified
queries across three corpora (lore-bench 25, terrarium-bench 25, lexomancy 30),
each tagged `semantic` / `lexical` / `symbol` / `design`|`docs` / `multi-file`,
each carrying the paths that answer it and a `why` note. `score.ps1` reports
hit@5, hit@10, MRR@10, nDCG@10 (binary), per-kind breakdown.

## What it cannot answer, and why

**The keys are recall keys.** Average declared-relevant paths per query: 1.16
(lore-bench), 1.20 (terrarium), 1.40 (lexomancy). They name *the* answer, not
*every* acceptable answer. Consequences:

- **Precision is uncomputable.** Against a one-path key, precision@10 caps at
  0.1 even if all ten returned files are genuinely useful. "% of files that are
  vs are not relevant" cannot be derived from the current key material at all —
  not by re-scoring, only by labelling more.
- **Rank is computed and thrown away.** `score.ps1` finds `$firstHit` per
  query, folds it into MRR, and discards it. There is no per-query table saying
  "LB-07 put its target at rank 8."
- **nDCG is binary.** Everything not in the key is a zero, including the file
  that a human would call the second-best hit.
- **No negative queries.** Nothing in the set tests whether lore floods the
  caller with ten plausible-looking chunks when the honest answer is "nothing
  here matches."

The metrics are not wrong; they answer a narrower question than the one being
asked now. Everything below is additive — no existing number changes meaning.

## Proposal: a judged pool over the same queries

Classic TREC pooling, scaled down. Instead of declaring relevance ahead of
time for a handful of paths, **label every path that any arm actually
returned**, once, and reuse the labels.

1. **Pool.** For each (corpus, query), union the top-k paths across every arm
   ever run (models, config variants, lexical control). Today that is ~10
   paths per arm per query; the pool converges fast because arms overlap
   heavily.
2. **Label**, graded, per (query, path):
   - **2** — answers the query on its own. The existing hand-verified key
     entries are 2s by construction.
   - **1** — genuinely useful supporting context: the caller is better off
     having read it.
   - **0** — noise. Not wrong to exist, wrong to have surfaced.
3. **Cache.** Key each judgment on `(query_id, path, chunk content hash)`.
   Corpora are pinned (`frozen_at` per key file); when a pin moves, only the
   judgments whose content actually changed go stale. Store as
   `bench/retrieval/judgments/<corpus>.json`, alongside the keys, in git.
4. **Reuse.** A new arm re-scores for free against the cached pool and only
   pays judging for paths nobody has returned before.

The existing 80 keys are not replaced. They stay as the recall target *and*
become the judge's calibration set.

## Metrics to add

Purely score-side; the run artifacts barely change.

- **precision@5 / precision@10** — fraction of returned *files* (chunks
  collapsed to first occurrence, as `score.ps1` already does) with label ≥ 1.
  Report the strict variant (label = 2 only) beside it. Its complement is the
  number worth naming out loud: **noise rate**.
- **Rank of first relevant**, per query, in a table — plus the ranks of every
  key target, so "found it, but at 9" is visible as itself rather than as a
  0.11 contribution to MRR.
- **Graded nDCG@10** using 2/1/0 gains, next to the existing binary nDCG
  rather than replacing it.
- **recall@10 against key targets** — this is exactly today's hit@10, renamed
  once precision exists beside it and the ambiguity starts to bite.
- **Worst offenders** — every query with precision@5 below a threshold or
  first-relevant rank > 5, listed with its query text. This is the part that
  turns a score into work.

## Who judges

800+ labels per corpus is not a hand-labelling job, and the point of the bench
is to be re-runnable.

**Recommendation: an LLM judge, calibrated against the existing hand-verified
keys, with its agreement rate reported in every summary.** The judge sees the
query and the returned chunk text and emits 2/1/0 with one line of reasoning.
Calibration is cheap and non-negotiable: the 80 key entries are known 2s, so a
judge that does not call them 2 is broken and its numbers are void. Report
agreement in `summary.md` as a first-class row; a bench whose instrument is
uncalibrated should say so on its face.

Consequences to accept up front:

- **The judge needs the retrieved text.** Run artifacts record `path`,
  `line_start`, `line_end`, `score` — not the snippet. *Built instead:*
  `judge.ps1` reads the span off disk at the corpus root, which leaves the run
  path untouched, works on runs recorded before judging existed, and yields
  the file hash the cache key needs from the same read. It buys that with a
  hard dependency on the corpus sitting at its pin — hence the pin guard.
- **The judge is a dependency the bench did not have.** Local model (GPU
  contention with the embedding arm — the bench already refuses to run
  concurrently with the e2e matrix) or an API call (bench-only tooling, not
  shipped code, so D-0003's local-only embedding constraint is not implicated —
  but see the open question below).
- **Judged pools are incomplete by construction.** A file no arm ever returned
  is unlabelled, so precision is honest and recall stays bounded by the key.
  That asymmetry is standard and should be stated in the summary.

## Query-set gaps worth filling

The current 80 are almost all "find the one place" queries — which is why the
keys are sparse, and why precision was never expressible. Two kinds to add:

- **Breadth queries** — "every place X is enforced", where 4–8 files are all
  correct. Lexomancy already has two `multi-file` entries; this generalizes
  them. Precision only becomes meaningful when the ideal result set is bigger
  than one.
- **Negative queries** — plausible-sounding questions the corpus genuinely does
  not answer. Scored on precision alone: the right behaviour is a short, empty
  or clearly-weak result set, and the failure mode is confident noise.

## Harness shape

The query machinery is currently welded to the model matrix: `run.ps1`
downloads GGUFs, spawns `llama-server`, drains a per-model index, *then* runs
queries. Scoring the shipped configuration should not pay for any of that.

Split it:

- `query.ps1` — point at an already-drained daemon (own `LORE_DATA_DIR`, own
  port, same isolation refusals as today), run a query set, write
  `searches/<corpus>.json`. Both the model matrix and a one-off config check
  call it.
- `judge.ps1` — pool + label + cache. Idempotent; re-running costs only the
  unseen paths.
- `score.ps1` — extended with the metrics above. Falls back cleanly to today's
  numbers when no judgments exist for a corpus, so an unjudged run still
  scores.

`run.ps1` keeps the model matrix on top of `query.ps1` and otherwise stays as
it is.

## Open questions — answered 2026-08-17

1. **LLM judge, or hand-label a smaller pool?** → **LLM judge**, on the free
   tier, batched. Cost is the constraint, not principle.
2. **Does "local-only" extend past embeddings to bench tooling?** → The
   premise was wrong. D-0003's wording overstates Wrysk's intent: where the
   embedding engine *runs* was never the constraint; **always supporting
   local** is. The ledger entry needs amending to say that; a hosted judge in
   bench-only tooling was never in question.
3. **New folder or extend `bench/embed/`?** → **Renamed** to
   `bench/retrieval/`. The model matrix stays working but is not expected to
   matter again until a new embedding model lands.

## Still open

- **Breadth and negative queries** (§ Query-set gaps) are unbuilt. Precision
  only becomes fully meaningful once ideal result sets are bigger than one.
- **Judge cost at full scale.** The pool across all three corpora is ~2,200
  labels; a 12-item batch is one model call, so a full first pass is ~185
  calls. Cached afterwards, and only new paths cost anything.
- **A controlled re-run of the model matrix.** See the floor finding above:
  every recorded embedder margin predates both the current binary and the
  D-0020 ignore stack.
