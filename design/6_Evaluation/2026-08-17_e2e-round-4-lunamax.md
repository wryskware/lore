---
design_status: exploration
last_reviewed: 2026-08-17
decision_refs:
  - D-0009
  - D-0022
---

# E2E round 4 — the same ten cells at luna `max`

Round 3 ran Lexomancy on luna `high` and produced the program's first
retrieval-loses result: off 4.0, on 2.5. Wrysk's read was that luna is too weak
to measure whether lore helps, and that comparing two failing arms is
comparing garbage to garbage. This round tests that directly: **identical
prompts, pins, index, corpus and harness — only the variant changes.**

He was right, and [[2026-08-17_e2e-round-3-lexomancy]] carries a correction as
a result.

## Results

| task | prompt | off | on | off in | on in | diff | off tools | on tools |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| T1 | how does a surge cast i submit actually end up damaging an enemy? walk me through the code path, files and classes at each step | 0.5 | 0.5 | 222,522 | 145,072 | -35% | 105 | 81 |
| T2 | do axioms cost lexic residue? is there a slot limit or tiers? | 1 | **1** | 31,340 | 60,779 | **+94%** | 24 | 11 |
| T3 | i thought we removed residue from the design. is it still referenced anywhere in the game? code and assets, list everything you find | 1 | 1 | 163,410 | 141,847 | -13% | 173 | 147 |
| T4 | did we look at lanes for targeting? why'd we drop it, and where's that written down | 1 | 1 | 103,432 | 19,289 | -81% | 41 | 4 |
| T5 | add a tiebreaker to enemy targeting: prefer lowest shield percentage, between effective damage and hp fraction. keep it deterministic, handle the edge cases, and add tests | 0.5 | 0.5 | 132,688 | 110,780 | -17% | 80 | 58 |
| | **total** | **4.0** | **4.0** | **653,392** | **477,767** | **-27%** | 423 | 301 |

Wall: 1,478s off, 1,429s on (-3%). Graded by luna `max` under
[[2026-08-17_grading-protocol]] — a different grader from round 3, which used
`high`, so the two rounds' scores are not strictly one scale.

## Beside round 3

| task | luna off | luna on | max off | max on |
| --- | ---: | ---: | ---: | ---: |
| T1 | 1 | 0.5 | 0.5 | 0.5 |
| T2 | 1 | **0** | 1 | **1** |
| T3 | 1 | 1 | 1 | 1 |
| T4 | 0.5 | 0.5 | 1 | 1 |
| T5 | 0.5 | 0.5 | 0.5 | 0.5 |
| **total** | **4.0** | **2.5** | **4.0** | **4.0** |

**The arms tie.** No cell separates them. Round 3's 1.5-point gap was the model
failing, not retrieval hurting — and by the same evidence there is no sign of
retrieval helping correctness either. What it buys is **-27% input tokens and
-29% tool calls**, consistently, across four of five tasks.

Model choice moved the score by 1.5 points. Retrieval moved it by zero. That is
the first time in this program that the two have been measured against each
other, and it is not the ordering the bench was built expecting.

## Why T2 flipped, verified against the index

Round 3 said the ledger "never entered the pool" and that `behavior = "rank"`
cannot add to a pool it never entered. **That claim was too strong**, and this
cell is the counter-example.

Round 4's on-arm T2 made the same naive first query and got the same trap:

```
[1] 1.6.5_Axioms.md:18-23         Axioms (Draft Spec) > 2) Acquisition
[3] 1.6.5_Axioms.md:25-35         3) Slots, Tiers, and Limits (Draft)
[5] MetaProgression_Forging.md    3. Lexic Residue (tentative)
[6] 1.6_ForgingSystem.md:41-47    Lexic Residue
```

It then reformulated — `Axiom slot capacity tier class limit decided active
decision` — and the second search returned:

```
[1] design/0_Canon/DECISIONS.md:125-143   D-0006 — Axiom capacity and duplicates
[3] design/0_Canon/DECISIONS.md:217-242   D-0009
[8] design/0_Canon/DECISIONS.md:102-123   D-0005
```

**D-0006 at rank 1.** The ledger was one query away the whole time. Luna
searched once, answered from drafts, and scored 0; max searched twice and
scored 1, at 94% more input tokens than its own off arm.

What survives from round 3 is narrower and still worth acting on: **the naive
phrasing of an authority question returns drafts above the ledger.** That is a
ranking problem, not an absence. The drafts carry no `design_status`, so the
authority profile has nothing to demote them by, while `DECISIONS.md` scores
below them until the query happens to contain ledger vocabulary
(`decided`, `decision`, `capacity`).

## Retrieval behaviour

**14 searches across 5 on-arm cells returned 68 unique paths; 22 were used
(32%) — 18 read, 7 expanded in place — and 14 (21%) survive into the answer.**
Round 3's figures were 22% used, 15% cited. Max both searches more (14 calls vs
12) and does more with what comes back.

Pass B: four `relevant-and-used`, one `partially-relevant` (T3 — every hit was
design prose and the question asked for code and assets, so the agent was right
to fall back to grep). Its T2 verdict names the two-stage pattern directly:
"the early returns were partly stale, but later retrieval surfaced the
canonical residue and capacity decisions."

## What the bench learned about itself

- **A cell can hang.** One round-4 cell sat with a live process and no output
  for 19 minutes and had to be killed by hand. Nothing in `run.ps1` bounds a
  cell's wall clock, so a hang blocks its wave indefinitely — and in the serial
  path, everything behind it.
- **Wave splitting keyed on a model's name.** `$_.Model -eq 'luna'` demoted
  `lunamax` into the serial bucket, so the first attempt ran eight cells one at
  a time. Now keyed on which models contend for the GPU.
- **Containment is not a property the harness checks.** A separate attempt to
  run this round on `gemini-3.7-flash-high` via the antigravity CLI produced a
  cell that walked out of its workspace, read this vault's task set, and
  returned a flawless, perfectly-sourced answer transcribed from the answer
  key. It was caught by reading the tool trail, not by anything automatic. See
  § Next.

## Next

1. **A containment assertion in `run.ps1`.** Fail any cell whose tool trail
   references a path outside its tree. Round 3 and round 4 both pass it; the
   gemini cell would have been voided loudly instead of scoring 1.
2. **A per-cell wall-clock timeout**, for the hang above.
3. **Decide what a zero correctness delta means for lore's pitch.** Two rounds
   now say the same thing from opposite directions: with a weak model retrieval
   changes the answer (for the worse, once), and with a strong one it does not
   change the answer at all — it makes getting there cheaper. A cost argument is
   a real argument, but it is not the argument the design vault currently makes.
4. **Fix the ranking observation that survived**, per § Why T2 flipped: an
   authority question phrased in the user's words puts undeclared drafts above
   the ledger.
5. **T1 and T5 still do not discriminate** in either round, and T5 has never
   been scored above `confidence: low` because the Lexomancy suite is not
   headless-drivable. A `unity-mcp` server exposing `run_unity_tests` exists on
   this machine and has not been tried.
