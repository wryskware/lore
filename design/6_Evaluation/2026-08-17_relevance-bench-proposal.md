---
design_status: exploration
last_reviewed: 2026-08-17
---

# Relevance bench — proposal

A proposal, not a decision. Nothing here binds until Wrysk says so.

Question it answers: *for a given search, how much of what came back was
actually relevant, and where in the ranking did the right answers land?*

## What already exists

Three evaluation layers ship today, and the middle one is the one people
forget:

| layer | where | asks |
| --- | --- | --- |
| end-task | `bench/` + [[2026-08-17_e2e-round-2-task-set]] | does an agent do the job better with lore on? |
| retrieval quality | `bench/embed/` | does the known-correct file come back, and how high? |
| cost/latency | `bench/embed/` score tables, `bench/latency.py` | what does it cost to index and to query? |

So retrieval quality **was** built — it just wears an embedding-model
comparison's clothes. `bench/embed/queries/*.json` holds 80 hand-verified
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
   `bench/embed/judgments/<corpus>.json`, alongside the keys, in git.
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

- **Run artifacts must carry chunk text.** `run.ps1` records `path`,
  `line_start`, `line_end`, `score` today. The judge needs the snippet — either
  widen the recorded result or fetch by `chunk_id` through `expand`. This is
  the only change to the run path.
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

## Open questions for Wrysk

1. **LLM judge, or hand-label a smaller pool?** The proposal assumes the
   judge. A hand-labelled 20-query subset is the honest alternative — smaller,
   binding, and stale the moment the corpora move.
2. **Does "local-only" extend past embeddings to bench tooling?** D-0003
   constrains *embedding providers*. A judge is neither an embedder nor
   shipped code, so nothing in canon speaks to it — but the spirit might.
3. **New folder or extend `bench/embed/`?** Extending keeps one query set and
   one scorer, at the cost of a folder whose name no longer describes what it
   holds. Renaming it (`bench/retrieval/`) is the tidier end state and a
   mechanical change.
