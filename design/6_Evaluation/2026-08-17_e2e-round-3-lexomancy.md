---
design_status: exploration
last_reviewed: 2026-08-17
decision_refs:
  - D-0009
  - D-0022
---

# E2E round 3 — Lexomancy, the ten cells round 2 could not run

Round 2 deferred Lexomancy because the walker did not descend the bench root's
junctions, so the bench tree indexed three loose files
([[2026-08-17_e2e-round-2-report]]). D-0022 settled that: the bench root
declares its junction targets as `[[sources]]`, and `Lexomancy-bench` now
indexes 18,271 chunks over 1,451 files — within 1.3% of the live `Lexomancy`
project. These are those ten cells.

**Not comparable to round 2's numbers, and not filed into its table.** The
embedding backend moved from llama.cpp `qwen3-4b` to vLLM `Qwen3-Embedding-4B`
(`0a42f89`) and the corpus is now chunked through the Unity plugin. On-vs-off
*within* Lexomancy is unaffected, and that is the comparison that carries the
result. The prompts and keys are still round 2's, unchanged.

Graded under [[2026-08-17_grading-protocol]]. **Scores are a first pass by luna
and have not been audited.** Given what they say, they should be.

## Setup of record

| | |
| --- | --- |
| retrieval project | `Lexomancy-bench` — 1,451 files, 18,271 chunks, 100% embedded |
| cm pin | `cs:134` head, workspace `Lexomancy-alt` |
| design vault | `6aa9a82` (round 2 recorded `7604c27`; the only delta is an added `.loreignore`, so corpus content is unchanged) |
| embeddings | `Qwen/Qwen3-Embedding-4B` via vLLM at `http://127.0.0.1:8000/v1` |
| authority | `lore-v1 (rank)`, 15/16 decisions active |
| plugins | `unity fd8527497697` |
| pinned `lore-mcp.exe` | `sha256 14036A99…E48D683`, unchanged from round 2 |
| task set | `round-2`, prompt SHAs recorded per cell |
| model | luna (`openai/gpt-5.6-luna`, variant high), both arms, slot a |

Slot b does not exist for Lexomancy, so both arms shared slot a: T1–T4 in one
wave (read-only, safe to share), T5 serially, one arm at a time.

`LORE_PROJECT=Lexomancy-bench` scoping was verified against the pinned binary
before the round — round 2 shipped the pin unverified. It returns
mount-relative paths (`Lexomancy/Assets/…`, `tools/…`) that open through the
junctions from the bench cwd.

## Results

| task | prompt | score off | score on | input off | input on | diff | tools off | tools on | sec off | sec on |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| T1 | how does a surge cast i submit actually end up damaging an enemy? walk me through the code path, files and classes at each step | 1 | 0.5 | 130,845 | 73,268 | -44% | 65 | 38 | 192 | 112 |
| T2 | do axioms cost lexic residue? is there a slot limit or tiers? | 1 | **0** | 64,178 | 14,705 | -77% | 27 | 4 | 55 | 17 |
| T3 | i thought we removed residue from the design. is it still referenced anywhere in the game? code and assets, list everything you find | 1 | 1 | 101,663 | 99,688 | -2% | 83 | 76 | 278 | 322 |
| T4 | did we look at lanes for targeting? why'd we drop it, and where's that written down | 0.5 | 0.5 | 76,491 | 21,317 | -72% | 34 | 4 | 77 | 21 |
| T5 | add a tiebreaker to enemy targeting: prefer lowest shield percentage, between effective damage and hp fraction. keep it deterministic, handle the edge cases, and add tests | 0.5 | 0.5 | 128,768 | 110,560 | -14% | 75 | 62 | 455 | 324 |
| **all** | | **4.0** | **2.5** | **501,945** | **319,538** | **-36%** | **284** | **184** | **1057** | **796** |

Cache read tracks input: 5,289,984 off vs 3,563,008 on (-32.6%).

**On wins cost everywhere and loses 1.5 points of correctness.** The two
largest cost wins, T2 and T4, are the two tasks it did not win on correctness.
T3 is the one task retrieval bought nothing on -- the hits were design prose and
the question needed live code and assets.

**This is the first round in which retrieval costs correctness**, and it loses
on T2 -- the authority/modality archetype the authority profile exists to win.

Per-task notes: T1, the on arm's trace misses the damage-mutation leg. T4, both
arms cite D-0016 and both are thin on the rejected alternative. T5 is
`confidence: low` on both arms -- no headless suite, see § What to do next.
Round 2's aggregate was a wash (-6.9% input); an 18k-chunk corpus is where
retrieval pays.

## T2: fast, cheap, confident, wrong

The prompt: *do axioms cost lexic residue? is there a slot limit or tiers?*
The key: no to both, sourced to the supersession clauses in D-0002 and D-0006,
with `1.6.5_Axioms.md` identified as superseded rather than authoritative.

The on arm answered in **17 seconds, 14.7k input tokens, 4 tool calls — all
four of them lore calls**. It never left the index. Its single search returned:

```
[1] MetaProgression_Forging.md:32-36   3. Lexic Residue (tentative)
[2] Lexomancy_Design_Document.md       6.5 Axioms
[3] 1.6.5_Axioms.md:18-23              Axioms (Draft Spec) > 2) Acquisition
[5] 1.6.5_Axioms.md:25-35              3) Slots, Tiers, and Limits (Draft)
```

It expanded two of those chunks and answered *"Yes. Axioms are forged using
Lexic Residue"*, with tiers and slots "still TBD", sourced to
`1.6.5_Axioms.md`. The off arm spent 27 tool calls and 64.2k tokens and got it
right.

**The mechanism, reproduced directly against the index rather than inferred
from the transcript:** running that exact query returns no `DECISIONS.md` in
the top 10. Eight of the ten hits carry no `status:` line at all — they are
unclassified design prose. So `behavior = "rank"` had almost nothing to rank
with, and ranking reorders a pool; it cannot add to one. The decisive document
never entered.

The agent was not careless. It was handed drafts, told nothing about
supersession, and had no reason to doubt them. Retrieval did not merely fail to
help here — it substituted for the search that would have found the ledger.

This is a sharper claim than round 2's, and it points somewhere different.
Round 2 said agents re-verify retrieval they could have trusted, and read that
as a trust problem. Round 3 says that when they *do* trust it, the pool had
better contain what supersedes the top hit. Those two findings pull in opposite
directions and the tension is not yet resolved.

## Retrieval behaviour

Deterministic, no model: **12 searches across 5 on-arm cells returned 73
unique paths; 16 were used (22%) — 10 read, 8 expanded in place — and 11 (15%)
survive into the final answer.**

`lore_expand` accounts for 8 of 22 lore calls here, against roughly none in
round 2's corpora. The old uptake metric counted only a later `read` of a
returned path, so drilling into a hit *without leaving the index* scored as
ignoring retrieval; on/T2 measured 0% uptake while using nothing but lore.
Fixed in `ca685d5`.

Pass B's verdicts: four `relevant-and-used`, one `partially-relevant` (T3,
correctly — the design hits could not cover live code).

**Pass B called on/T2 `relevant-and-used`. Pass A scored it 0.** Both are
defensible and the protocol should say so: pass B judges relevance to the
question as asked, and superseded drafts about axiom costs are squarely on
topic. It is not given the key, by design, so it cannot know the hits were
authoritative-looking and wrong. Round 2 never exposed this because no cell
scored 0. **Topical relevance and sufficiency-for-a-correct-answer are
different measurements, and pass B only makes the first.**

## A grader error, found by spot-checking

The grader flagged off/T1's citation of `BattleSimulator.cs:1190-1200` for
`ScalePayloads` as fabricated, because the key says 414. `ScalePayloads` is
*defined* at line 1190 — the answer is right and the flag is a false positive.
The key names a call site, and the nearest call in this checkout is 403, not
414, so the key's own number looks drifted from whatever tree it was written
against.

One check, one false positive, in a pass that reported `confidence: high`.

## Harness changes made during this round

- `740c541` — Lexomancy cells retrieve from `Lexomancy-bench`, not the live
  `Lexomancy` root. The bench project indexes `Lexomancy-alt`, the cm checkout
  a T5 cell actually edits; the live root drifts between rounds.
- `31fc13f` — launch stagger, and dead cells stop hiding. Wave 1 launched four
  cells in one second and three died at ~1.1s on opencode's
  `database is locked`; `-Matrix` then reported exit 0 having delivered five of
  eight. Cell exit codes now leave their process and a lossy matrix exits 1.
- `ca685d5` — `lore_expand` counts as uptake; graders get the same stagger.

The three killed cells were re-run individually and are quarantined as `x-…` so
`pack.py` does not read them as empty answers.

## What to do next

1. **Audit T2 first.** It carries the round. The sample should be on/T2 and
   off/T2, plus on/T1 (the other cell where the arms differ) and T3 as a
   unanimous control. Given the `ScalePayloads` false positive, the audit
   should check the key's citations as well as the grader's.
2. **Decide what T2 means for authority.** `rank` cannot rescue a pool the
   ledger never entered. Whether the answer is a retrieval change (ensure
   supersession records surface for queries about what they supersede), a
   ranking change, or a `behavior` beyond `rank`, is an open design question
   this round does not settle.
3. **Reconcile round 2 and round 3.** Round 2 says agents distrust good
   retrieval; round 3 says a cell that trusted it was wrong to. Both are one
   round of one model on one corpus.
4. **Close the T5 suite hole for Lexomancy.** The suite was called
   not-headless-drivable, but a `unity-mcp` server exposing `run_unity_tests`
   is present on this machine. If it can drive the EditMode suite, T5 stops
   being graded at `confidence: low`.
5. **Give pass B a sufficiency question**, distinct from relevance — or record
   in the protocol that it does not answer one.
