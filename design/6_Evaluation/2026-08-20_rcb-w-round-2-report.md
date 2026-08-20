---
design_status: exploration
---

# RCB-W round 2 — frozen tasks, recalibrated search, one answerer

The first round on the frozen `rcbw-v1` task set, and the first after the
2026-08-20 search recalibration (lore `0834816`: excerpts inline for the top
3 hits only, pointer headers for the tail, daemon default page 20 → 10). One
run, Opus 5 @ medium only, at Wrysk's direction; repeats are a pending
decision on these results.

## Round card

| field | value |
| --- | --- |
| round | RCB-W 2 |
| ran | 2026-08-20 |
| question | Does the search-payload recalibration collapse the on-arm cost premium, and do the W1/W4 prompt fixes unblock those tasks? |
| model | `claude-opus-5` @ medium effort (stamp `rcbw2o-0820`), Claude Code CLI, model id verified in the event streams |
| arms | off, on — identical three steering lines as rounds 1 |
| repo / task set | `microsoft/agent-framework` @ `47fa59f8` / **`rcbw-v1` frozen** (`tasks/_task_set.json`, stamped in every row) |
| lore under test | `0834816` (pointer-tail rendering, DEFAULT_LIMIT 10); daemon and MCP binary rebuilt and redeployed in WSL before the round |
| cells | 10 planned, 10 ran, 10 graded, 0 excluded |
| grading | deterministic (regression + collateral, same exclusions as round 1) + arm-blind Sonnet batch judge, tertiary |
| token basis | deduped per API call (recount fix `1726fff`) — round-2 numbers are NOT comparable to round 1's inflated ledger tables, only to the corrected table in round 1's debug section |
| reproduce | `run_writer.py <tasks> /tmp/rcbw-round2o.jsonl --answerer claude --arms off,on --stamp rcbw2o-0820` with `SBX_CLAUDE_MODEL=claude-opus-5 SBX_CLAUDE_EFFORT=medium`; bench subrepo @ `f681480` |

## Headline

**The cost premium collapsed and the fix counts went up.** On-arm nominal
cost fell from +79% (round 1, same model) to **+23%**; cost-weighted tokens
from +87% to **+22%**. Deterministic fixes: **off 3/5, on 4/5** (round 1:
3/5 both arms). The W1 prompt fix converted W1 from 0-for-4-cells across
round 1 to fixed in both arms here. The one on-arm-only fix (W5) involved
zero lore calls, so it is trajectory variance, not attributable retrieval
value — but for the first time the on arm is not paying a large premium for
it.

## Cell ledger

| task | arm | grade | judge | wall s | calls | lore | tokens (deduped) | cost-wt | $ | lines vs golden |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| W1 | off | **fixed** | equivalent | 36.1 | 11 | 0 | 273,421 | 39,576 | 0.284 | 7/15 |
| W1 | on | **fixed** | equivalent | 38.5 | 12 | 0 | 304,084 | 47,067 | 0.344 | 8/15 |
| W2 | off | not_fixed | incorrect | 126.2 | 15 | 0 | 425,941 | 67,487 | 0.517 | 47/53 |
| W2 | on | not_fixed | incorrect | 163.1 | 17 | 1 | 501,976 | 71,016 | 0.512 | 46/53 |
| W3 | off | **fixed** | equivalent | 65.5 | 10 | 0 | 257,981 | 39,308 | 0.279 | 6/6 |
| W3 | on | **fixed** | equivalent | 88.4 | 21 | 1 | 593,518 | 79,117 | 0.554 | 6/6 |
| W4 | off | **fixed** | equivalent | 34.2 | 11 | 0 | 283,863 | 42,651 | 0.305 | 4/4 |
| W4 | on | **fixed** | equivalent | 59.4 | 19 | 1 | 496,408 | 65,266 | 0.440 | 4/4 |
| W5 | off | not_fixed | incorrect | 87.4 | 15 | 0 | 675,768 | 122,110 | 0.916 | 12/62 |
| W5 | on | **fixed** | alternative_correct | 196.2 | 23 | 0 | 831,516 | 117,343 | 0.982 | 31/62 |

All 20 diffs touched exactly the golden source file(s) and nothing else
(recall 1.0, precision 1.0 on every cell) — location remains a non-problem
for Opus on this corpus.

## Totals, and round 1 (Opus, corrected) beside them

| | r2 off | r2 on | r2 delta | r1o delta (corrected) |
| --- | ---: | ---: | ---: | ---: |
| fixed | 3/5 | 4/5 | +1 | 3/5 both |
| tokens (deduped) | 1,916,974 | 2,727,502 | **+42%** | +108% |
| cost-weighted | 311,132 | 379,809 | **+22%** | +87% |
| nominal $ | 2.30 | 2.83 | **+23%** | +79% |
| API calls | 62 | 92 | +48% | +62% |
| lore calls | — | 3 | | 3 |

## What changed and what it says

**1. The payload mechanism is confirmed by its removal.** Round 1's lore
search results were 19–36KB each; round 2's are **6.0–7.2KB** (10 results,
3 excerpts, 7 pointer headers). With residency mostly gone, the remaining
on-arm premium is almost entirely the other round-1 driver — longer on-arm
trajectories (92 vs 62 calls, W5-on 23 calls with zero lore use, W3-on 21
vs off's 10 for the same one-line fix) — which the payload change was never
going to touch. At n=1 that length gap is the same trajectory variance
round 1 showed in both directions on W5.

**2. The schema teaches.** Round 1: zero of 12 lore calls passed any
parameter beyond `query`. Round 2: one call passed `limit: 10` explicitly
and another scoped with `path_prefix: "python/"`. Small numbers, but the
new tool descriptions are being read. W3-on's single search returned the
golden `_mapper.py` at rank 1 with its excerpt inline — the recalibrated
page answered the question round 1 needed a 36KB payload for.

**3. The W1 prompt fix is validated.** W1 went from 0-fixed-in-4-cells
(both models, round 1) to fixed in both arms, judged equivalent to golden
both times — never-write contract, ValueError on negative, stored history
untouched. The round-1 failures were task-design artifacts, as diagnosed.
W4 (now naming Python) stayed fixed in both arms.

**4. W2 is now 0-for-8 across rounds and models.** Every attempt lands
46–50 of the golden's 53 lines in the right file and still fails the
regression — the recursive schema-normalization contract has a subtlety
every trajectory misses. It is doing its job as the hard task; worth
keeping as-is.

**5. The 4/5-vs-3/5 needs repeats before it means anything.** The on-only
W5 fix used no lore, and round 1's Opus arm fixed W5 from both sides. The
defensible claims after this round are: the recalibration removed most of
the measured cost of having lore attached, the prompt fixes removed the
task-design noise, and adoption (3 calls / 10 on-cells) is still too
shallow to test whether searching changes outcomes. Repeats are the pending
decision.

## Deviations and incidents, stated explicitly

- **A zero-round was discarded before this one ran**: the first
  `rcbw2o-0820` launch died in all 10 cells inside ~6s on an expired host
  OAuth token (jailed CLI copies host credentials). Nothing model-generated
  existed; the out dir and JSONL were deleted and the stamp reused for the
  clean rerun. Token refreshed host-side first.
- **The first grading pass of the real round was invalid** — grade.py was
  invoked with a relative round dir, `git -C <cell> apply` resolved
  `./off/W1/diff.patch` inside the temp cell, and all 10 cells graded
  not_fixed with "can't open patch". Fixed in the harness (`f681480`,
  diff path made absolute) and re-graded; the judge batch was dispatched
  only on the corrected grades.
- The daemon restart for the recalibration briefly took the WSL lore
  daemon down between rounds; index state persisted (single-owner
  semantics) and `agent-framework` served 39,048/39,048 chunks on restart.

## Artifacts

`~/bench/rcbw/out/rcbw2o-0820-claude/` (per-cell streams, diffs, metrics,
`grades.jsonl`, `verdicts.jsonl`); round JSONL `/tmp/rcbw-round2o.jsonl`.
Judge: 10/10 verdicts parsed, all consistent with deterministic grades.

## Next (Wrysk's call)

1. Repeats (k=3 per cell, ~$15/answerer with the recalibrated payloads) to
   turn the 4/5-vs-3/5 and the trajectory-length gap into signal or noise.
2. The diagnosis-steer arm (require one search before proposing a fix)
   remains queued behind repeats, now unconfounded by payload size.
