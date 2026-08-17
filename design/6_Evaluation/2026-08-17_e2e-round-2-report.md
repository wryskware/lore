---
design_status: exploration
last_reviewed: 2026-08-17
decision_refs:
  - D-0009
---

# E2E round 2 — results, lore and terrarium

**Incomplete by design.** Lexomancy's ten cells are deferred: the walker does
not descend the bench root's junctions, and the main `Lexomancy` project has
drifted from ~81k chunks to 17.9k under the D-0020 ignore stack (`c149b75`).
Running those cells before that is settled would measure the ignore stack, not
retrieval. Everything below is lore + terrarium, both arms, luna, 20 cells.

Graded against [[2026-08-17_e2e-round-2-task-set]] under
[[2026-08-17_grading-protocol]]. **Scores are a first pass by luna and have not
been audited yet** — the protocol's step 2 sample is named at the end and is
the next thing to do.

## Setup of record

| | |
| --- | --- |
| pins | lore `977364a`, terrarium `3b1eacd56f` (unmoved) |
| pinned `lore-mcp.exe` | `sha256 14036A998C280A029145C479910F3AA10BDD3B93AA27E5DC06844A3FEE48D683` |
| embedding backend | `qwen3-4b` at `http://127.0.0.1:8090/v1`, whole round |
| authority | `lore-bench` + `-b` under `lore-v1 (rank)`; terrarium neutral, on purpose |
| Lexomancy vault SHA | `7604c27ed2f9a6d1764b44acbb348607d872b019` (recorded per D9; unused this round) |
| corpus scrub | round-1 plan removed from both lore slots, asserted before every cell |
| task set | `round-2`, prompt SHAs recorded per cell |

The pilot (`lore/T3`, `terrarium/T4`, both arms, slot a) ran first and is
**excluded** from these results — same prompts, separate cells, kept as
calibration evidence only.

## Score: 8.0 / 8.0

| task | off | on | note |
| --- | --- | --- | --- |
| lore T1 | 1 | 1 | |
| lore T2 | 1 | 1 | both read 3.1's `leaning` status correctly |
| lore T3 | 1 | 1 | both reached `authority.rs` and the declared/effective split |
| lore T4 | 1 | 1 | both cite D-0005 with the token-cost framing |
| lore T5 | 0.5 | 0.5 | see § The T5 suite hole |
| terrarium T1 | 0.5 | 0.5 | both miss a required sampler hop |
| terrarium T2 | **1** | **0.5** | on arm missed that `analysis` still emits the sidecar |
| terrarium T3 | 0.5 | 0.5 | |
| terrarium T4 | **0.5** | **1** | off arm paraphrased the reason off-record |
| terrarium T5 | 1 | 1 | |

Two cells differ, in opposite directions. Nothing here separates the arms on
correctness, which is what the task set predicted for T2/T4 and not what it
predicted for T1/T3.

## Cost: a wash in aggregate, large swings per task

| | off | on | Δ |
| --- | --- | --- | --- |
| input tokens | 744,614 | 692,984 | **−6.9%** |
| cache read | 5,573,120 | 6,003,712 | **+7.7%** |
| tool calls | 330 | 332 | +0.6% |
| wall | 1,397s | 1,369s | −2.0% |

The aggregate hides the result. Where the model leaned on retrieval the win is
large — lore T4 **−50%** input tokens (39.7k → 19.9k), terrarium T2 **−40%**
(49.3k → 29.8k). Where it made one search and then verified everything by hand
anyway, the on arm costs the same or slightly more, and cache read goes up
because the hits are extra context that did not remove any reading.

## The retrieval question: relevant, and used

Six of ten on-arm cells made exactly one `lore_search` call. That looks like
non-adoption, but "the agent ignored good hits" and "the hits were useless"
have opposite fixes, so the harness now captures what each call returned
(`bench/pack.py`), and pass B judged relevance from it.

Deterministic, no model involved: **13 searches returned 85 unique paths; the
agent later opened 40 of them (47%) and 30 (35%) survive into the final
answer.**

Pass B's verdicts: **nine of ten cells `relevant-and-used`**, one
(`lore/on/T5`) `partially-relevant` — the search found the parser and the test
home but did not surface the re-index requirement. **Zero cells came back
`relevant-but-ignored` or `irrelevant`.**

So neither of the two hypotheses that motivated the pass survives:

- Not a ranking failure. The hits contained the answer. terrarium T2's single
  call returned `plan.md` Revision 3 at rank 1 and Revision 4 at rank 4 — both
  halves of the key — and that cell ran −40% input tokens.
- Not simple non-adoption either. The agents *did* build on the hits. What they
  also did was re-verify by grep and read, which is what a coding agent is
  trained to do before it will assert a `file:line`. One call is not a symptom
  when one call sufficed.

The remaining cost story is therefore about **verification behaviour**, not
retrieval quality: lore T5's single call returned `chunk_markdown` and
`Heading` at `markdown.rs:31-87` and the agent globbed the whole tree anyway.
Steering that claims retrieval is trustworthy is a different lever from
steering that says retrieval is useful, and Lever B (shipped in `d926120`)
argues the latter.

Caveat: pass B was graded by luna, which skews agreeable, and its verdicts
should be read against the uptake numbers rather than instead of them.

## The T5 suite hole, and the fix

All four T5 cells were first graded 0.5 with `confidence: low`, none of it for
a reason about the answer: the harness captured each diff and immediately reset
the tree, so the suite — a scored criterion — had nowhere left to run.

Three of the four had in fact run a full suite themselves, which the packet was
discarding: lore/off green on `cargo test --workspace` (29 ok, zero failures),
both terrarium cells green on 147 pytest tests. lore/on never ran a full suite
— its `cargo test -p lore` hit a `daemon_watch` failure that passed when re-run
alone.

Fixed both ways (`ffd5556`): `run.ps1` now runs the suite between diff capture
and reset and writes `suite-result.txt`, and packets carry the agent's own
suite runs, scope-tagged and labelled self-reported. Re-grading with that
evidence moved terrarium T5 from 0.5/0.5 to **1/1**. lore T5 stayed 0.5/0.5 —
the grader would not credit a self-reported workspace run, which is arguably
over-conservative for lore/off given the command is exactly the key's, and is
the first thing the audit should look at.

**Future rounds have an authoritative suite result and this does not recur.**

## Harness changes made during this round

- `8b2cb13` — cells pin `LORE_PROJECT`. `lore-mcp` resolves its project from
  cwd since `b527b62`, while `run.ps1` only *recorded* the project a cell
  retrieves from. Harmless for lore/terrarium, where the two agree; it would
  have made every Lexomancy on-arm cell search three loose files.
- `8b2cb13` — `-Repos`/`-Tasks` matrix filters, and the T5 tree-clash assertion
  groups by resolved directory rather than `(repo, arm)`.
- `dfc18c5` — retrieval extraction and the grading protocol.
- `ffd5556` — the suite fix above.

## What to do next

1. **Audit the grading** (protocol § Who grades). The sample: `terrarium-T2`
   and `terrarium-T4` (the two that differ), all four T5 cells (`confidence:
   low`), plus `lore-T1` and `lore-T2` as unanimous controls.
2. **Check the flagged citations** against the repos at the pin. The grader
   flagged `crates/lore/src/lore-mcp/src/server.rs` (a path with a duplicated
   segment, in lore/off/T3) and `physarum.ts:1413-1420` (both terrarium T1
   cells).
3. **Lexomancy**, once the ignore-stack drift and the junction walk are
   settled. `LORE_PROJECT` is pinned but that scoping has **not** been verified
   empirically yet — do that before spending ten cells.
4. **Decide what the verification behaviour means for steering.** This round
   says retrieval returns the right thing and gets used, and that the agent
   re-derives it anyway. That is a claim about trust, and no lever currently
   drafted addresses it.
