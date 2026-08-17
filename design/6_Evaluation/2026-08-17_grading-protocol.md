---
design_status: exploration
last_reviewed: 2026-08-17
decision_refs:
  - D-0009
---

# Bench grading protocol — grader threads, batching, and the retrieval axis

Grading is **part of the bench run job**, not an afterthought a human does by
hand afterwards. Round 1 was graded ad hoc and the round-2 task set had to
spend nine defect entries repairing what that produced. This document is the
durable procedure: what a grader thread receives, how cells are batched into
threads, what it returns, and how the grading itself gets checked before
anyone believes it.

Authority for *criteria* stays with the round's task set (currently
[[2026-08-17_e2e-round-2-task-set]]). This document says how criteria get
applied, never what they are.

## The two passes, and why they are separate

A grading run is two passes over the same cells with **deliberately different
inputs**. Collapsing them into one thread is the mistake this design exists to
prevent.

| | Pass A — task score | Pass B — retrieval behaviour |
| --- | --- | --- |
| Grades | 0 / 0.5 / 1 against the key | nothing; it is diagnostic |
| Sees | prompt, final answer, diff, suite result | lore calls with their **returned hits**, tool trail, answer |
| Does not see | the arm, the tool trail, whether lore was used | — |
| Covers | every cell | on-arm cells only |
| Batches by | `(repo, task)` — both arms per thread | `repo` — all on-arm cells per thread |

**Why pass A is blind to the arm.** The task set's arm-neutrality rule says
every scored criterion must be derivable from the prompt. A grader that can see
which answer had retrieval available will grade the two arms relative to each
other instead of against the key — and the round's whole claim is a comparison
between arms, so a grader that anchors one to the other is measuring itself.
Pass A therefore receives answers labelled `A` / `B` in a fixed but unlabelled
order, and never sees the tool trail (which reveals the arm on its first
`lore_search` line).

**Why pass B scores nothing.** The task set is explicit that no criterion may
rest on an agent's process, because the harness captures reasoning only as raw
events and no grader should adjudicate that. Pass B does not adjudicate
reasoning — it reads what retrieval *returned* and what the agent did next —
but its output is a diagnosis, never a number that touches a cell's score.
Keeping it out of the score is what lets it be honest.

## Pass B exists because "one lore call" is ambiguous

Round 2's first 20 cells produced a striking pattern: six of ten on-arm cells
made exactly one `lore_search` call and then reverted to grep-and-read. Two
incompatible stories explain that, and they have opposite fixes:

- **Non-adoption.** Retrieval answered well and the agent ignored it. Fix is
  steering (Lever A/B), not the engine.
- **Poor retrieval.** The hits were irrelevant, the agent correctly gave up.
  Fix is ranking/chunking, and no amount of steering helps.

The event stream distinguishes them, because it captures tool **outputs** as
well as arguments. `pack.py` extracts, per lore call, the hits returned and
two computed numbers:

- **uptake** — how many returned paths the agent went on to open;
- **answer overlap** — how many returned paths survive into the final answer.

Both are string containment over paths, computed deterministically, with no
model in the loop. They are evidence for pass B, not a verdict: terrarium T2's
single call returned the exact key answer at ranks 1 and 4, was followed by
opening 4 of 6 returned paths, and cost 40% fewer input tokens — one call
because one sufficed. lore T5's single call also returned the right function
and was followed by a whole-tree glob anyway. Same call count, opposite
stories, and only the hit list tells them apart.

## What a pass-A thread receives

One self-contained brief per `(repo, task)`, assembled by `bench/grade.ps1`
into an isolated working directory. A grader thread **never runs inside a bench
repo** and has no retrieval of its own — it grades what the harness captured,
and a grader that can go re-derive the answer itself is no longer grading the
answer.

1. The prompt, verbatim.
2. The key section for that task, sliced from the round's task set — including
   its scale, its "credited, not required" list, and its self-check.
3. Each cell's answer, labelled `A` / `B`, plus `diff.patch` and the suite
   result for T5.
4. The output schema below.

Both arms in one thread is the batching unit on purpose: the key section is
long, it is read once instead of twice, and the criteria are applied by the
same reader in the same sitting — which removes the drift you get from two
threads interpreting "materially complete" differently.

## Output schema

One JSON object per cell, criterion by criterion. A bare score is not
acceptable output: the parent has to be able to check the evidence without
re-grading from scratch.

```json
{
  "label": "A",
  "score": 1,
  "criteria": [
    {"id": "hop-7 merge point", "met": true,
     "evidence": "names fuse_detailed and RRF_K=60 in § Merge"}
  ],
  "missing": ["hop-4 embedding before the store lock"],
  "fabricated_citations": [],
  "confidence": "high",
  "notes": "one sentence, only if something does not fit the schema"
}
```

`fabricated_citations` is mandatory and separately checkable: several keys
grade precision, and a `file:line` that does not resolve costs score. The
grader flags them; the parent verifies them against the repo at the pin, which
the grader cannot see.

Pass B returns, per on-arm cell:

```json
{
  "cell": "…-luna-terrarium-on-T2",
  "per_call": [{"position": 1, "relevance": "on-target",
                "why": "ranks 1 and 4 are the two plan.md revisions the task turns on"}],
  "verdict": "relevant-and-used",
  "diagnosis": "single call sufficed; agent opened 4 of 6 returned paths"
}
```

`verdict` is one of `relevant-and-used`, `relevant-but-ignored`,
`partially-relevant`, `irrelevant`, `no-hits`. `relevant-but-ignored` is the
steering finding; `irrelevant` is the ranking finding. Do not let a thread
return both for one cell — make it choose, and say why in `diagnosis`.

## Who grades, and how the grading gets graded

Start cheap and escalate on evidence, not on the task feeling important.

1. **First pass: the bench model itself** (luna). It is the cheapest grader and
   it has already read this material's subject matter. Its known weakness is
   agreeableness — it will accept a plausible answer that misses a required
   criterion — which is exactly what the criterion-by-criterion schema and the
   `missing` array are designed to expose.
2. **Audit the grader, not the round.** Re-grade a **sample** with a stronger
   model — every cell where the two arms scored differently, every cell the
   grader marked `confidence: low`, and two randomly chosen cells that looked
   unanimous. Record agreement as a fraction.
3. **Escalate on disagreement.** If the audit disagrees on more than ~1 in 5
   sampled cells, the first pass is not usable as a grading of record: re-grade
   every cell with the stronger model and keep luna's verdicts only as a
   comparison artifact. Below that, keep luna's grades and correct the
   individual cells the audit overturned, noting each correction.
4. **Wrysk adjudicates** anything the audit and the first pass both flag as
   genuinely ambiguous — which usually means the *key* is ambiguous, and the
   fix belongs in the next round's task set, not in this round's scores.

Do not run two graders over every cell as a matter of course. Independent
duplicate grading is worth paying for at the sample size in step 2 and not
beyond it.

## Mechanical steps the grader does not do

- **Suites are grader-run and are not captured by the harness.** A T5 cell's
  tree is reset immediately after the diff is captured, so running its suite
  means re-applying `diff.patch` to the slot, running the repo's suite command,
  writing the result to the cell's `suite-result.txt`, and reverting again.
  Until that file exists, the packet says so in as many words and a pass-A
  thread must return `confidence: low` on any criterion that depends on it,
  rather than assuming green.
- **Citation resolution.** The grader flags suspect `file:line`s; the parent
  checks them at the pin.
- **Filling scores back into `metrics.json`.** `score` stays `null` until a
  verdict is accepted, and is written by the merge step, never by hand.

## Running it

```powershell
# 1. extract packets and thread bundles (deterministic, no model)
python pack.py --cells '20260817-*' --batch repo-task

# 2. pass A: one thread per (repo, task), arms blinded
.\grade.ps1 -Round 20260817 -Pass A -Model luna -Throttle 5

# 3. pass B: one thread per repo, on-arm cells only
.\grade.ps1 -Round 20260817 -Pass B -Model luna

# 4. read grades\<stamp>\summary.md, then audit the sample (step 2 above)
```

Verdicts land in `bench/grades/<stamp>/<batch>.json`, with the assembled brief
kept beside them so a grading is reproducible and attributable to the exact
text the grader saw.
