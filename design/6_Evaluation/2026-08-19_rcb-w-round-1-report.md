---
design_status: exploration
last_reviewed: 2026-08-19
decision_refs:
  - D-0009
---

# RCB-W round 1 — write tasks, one answerer, first contact

Follows [[2026-08-18_bench-report-format]] where it applies and
[[2026-08-19_rcb-w-round-1-task-set-draft]] for the task set. First round of
the write-task program; nothing here is comparable to RCB QA rounds except
where a mechanism is explicitly contrasted. Sections the format expects that
cannot be filled honestly say so.

## Round card

| field | value |
| --- | --- |
| round | RCB-W 1 |
| ran | 2026-08-19 |
| question | Does lore change what a coding agent spends, and whether it lands correct fixes, on real post-pin bugs it must locate from behavioral symptoms? |
| model | `claude-sonnet-5` @ high (Claude Code CLI) — sole answerer this round |
| arms | off, on (on = lore MCP + the same three steering lines as RCB round 1, verified by prompt diff) |
| repo / task set | `microsoft/agent-framework` @ `47fa59f8` / RCB-W W1–W5 (draft, unfrozen — see Deviations) |
| cells | 10 planned, 10 ran, 10 graded, 0 excluded |
| grading | deterministic (authored regression tests + collateral suites + diff overlap); qualitative labels via arm-blind Sonnet 5 batch judge, tertiary only |
| gates before round | gate 1 (fail@pin / pass@golden / collateral, by execution) passed on all five tasks; W5 tests rewritten to public behavior; W3 smoke both arms |
| comparable to | nothing yet; first round of this program |
| reproduce | `python3 rcbw/run_writer.py <tasks> round.jsonl --answerer claude --arms off,on --stamp rcbw1-0819`; `rcbw/grade.py`; `rcbw/judge_batch.py`; `rcbw/summarize.py` (bench/rcb repo @ `748b63b`) |

## Headline

- **Both arms fixed exactly 1 of 5 tasks cleanly** (off: W5; on: W3). Two of
  the eight failures are task-design artifacts, not ability signals (below).
- **The QA round's efficiency result does not transfer.** The on arm spent
  **+54% total tokens** (on better 1/5) and +7% wall. The QA mechanism —
  off-arm subagent fan-out that lore displaced — is entirely absent here:
  **0 subagent messages in all 10 cells.** The on arm's premium is cache-read
  growth: more turns, each carrying MCP definitions and search results.
- **Location was never the bottleneck.** 8 of 10 cells put edits in the
  golden file (recall 1.0); the failures were *contract* and *completeness*
  failures inside the right file. Write tasks stress a different capability
  than the QA set's "find it at all".
- **Two clean discriminations in opposite directions.** W3: off suppressed
  the symptom in the frontend TypeScript (failed grading), on fixed the real
  server-side mapper defect and added a test. W5: off produced the full
  62-line-equivalent fix; on stopped at a 14-line partial — and made **zero
  lore calls** on that task.

## Setup of record

| field | value |
| --- | --- |
| corpus | template copy of `agent-framework` @ `47fa59f8`; `.git` holds exactly one commit (no post-pin history reachable in-cell) |
| cell isolation | per-cell `cp -a` of the template, mounted **rw** at the corpus path in the RCB jail (`SBX_CORPUS_RW`); host corpus never visible |
| test environment | shared uv venv (1.4G, `uv sync --all-packages`, system python 3.12.3), mounted ro at `python/.venv`; disclosed to both arms in the prompt; pytest explicitly allowed (Wrysk, 2026-08-19) |
| retrieval project | same `agent-framework` index as RCB (39,048/39,048 chunks, 100% embedded); serves pinned content — agent edits are not re-indexed (pinned-index semantics, disclosed here) |
| containment | unchanged from RCB round 1 (netns + broker allowlist, web tools removed twice) |
| diff capture | `git add -A && git diff --cached --binary <root>` per cell; `.lore.toml`/`.loreignore` masked via template `info/exclude` |
| artifacts | `~/bench/rcbw/out/rcbw1-0819-claude/<arm>/<task>/` — per-cell dirs from day one (RCB defect 1 not inherited) |
| token basis | recounted per assistant message from `agent.ndjson`, subagents included (RCB's reporting error not reproduced); basis = input+output+cache_read+cache_write |

## Cell ledger

| task | arm | grade | wall s | turns | tools | lore | total tok | file recall | lines vs golden |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| W1 | off | not_fixed | 38.3 | 7 | 6 | 0 | 382,664 | 1.0 | 9/15 |
| W1 | on | not_fixed | 44.5 | 11 | 10 | 1 | 912,629 | 1.0 | 9/15 |
| W2 | off | not_fixed | 79.9 | 11 | 10 | 0 | 803,819 | 1.0 | 34/53 |
| W2 | on | not_fixed | 96.3 | 13 | 12 | 2 | 1,556,126 | 1.0 | 50/53 |
| W3 | off | not_fixed | 44.3 | 13 | 12 | 0 | 823,028 | 0.0 | 0/6 |
| W3 | on | **fixed** | 92.3 | 16 | 15 | 4 | 1,924,586 | 1.0 | 8/6 |
| W4 | off | not_fixed | 132.0 | 25 | 24 | 0 | 1,997,972 | 0.0 | 0/4 |
| W4 | on | not_fixed | 269.2 | 27 | 26 | 4 | 4,734,785 | 0.0 | 0/4 |
| W5 | off | **fixed** | 231.8 | 28 | 27 | 0 | 2,482,642 | 1.0 | 52/62 |
| W5 | on | not_fixed | 58.6 | 13 | 12 | 0 | 876,193 | 1.0 | 14/62 |

All 10 cells `SUCCESS` at the CLI level; `blocked_tool_attempts` 0 everywhere.

## Totals and deltas

| | off | on | delta | on better | off better |
| --- | ---: | ---: | ---: | ---: | ---: |
| mean total tokens | 1,298,025 | 2,000,864 | **+54%** | 1 | 4 |
| mean wall s | 105.3 | 112.2 | +7% | 1 | 4 |
| mean tool calls | 15.8 | 15.0 | -5% | 1 | 4 |
| mean turns | 16.8 | 16.0 | -5% | 1 | 4 |
| fixed (clean) | 1/5 | 1/5 | — | — | — |

No p-values: n=5 paired cells, every cell n=1. Sign counts are the honest
summary. **This is one round, one answerer, no repeats.**

## What each task actually showed

**W1 (redis retention) — grading artifact, both arms.** Both arms found the
right file and implemented the same coherent alternative: treat
`max_messages<=0` as delete-stored-history-on-save. Upstream's merged contract
is never-write-never-delete plus `ValueError` on negatives — the alternative
the PR *body* floated and the maintainers rejected. Our tests encode the
merged contract, so both cells grade not_fixed. The symptom prompt
underdetermines this choice; the test author flagged the risk in advance.
Carried to Next as a prompt fix, not a test fix.

**W2 (nested schema) — genuine partials, on closer.** Both arms fixed array
`items` but missed the named-list nested object properties and
`additionalProperties` (the same 2 of 3 assertions fail in both). The on arm
got materially closer (50/53 changed lines vs 34/53). The task's difficulty
gradient is real.

**W3 (devui duplicates) — the clean discrimination.** Off patched the
*frontend TypeScript* to dedupe cards client-side: symptom suppressed, server
still emits duplicates, regression test fails. On (4 lore calls) fixed the
mapper's registration gate — 8 lines vs golden's 6 — and added a test. This
is the smoke result reproduced in the round proper.

**W4 (compaction non-ASCII) — task underspecification, both arms.** Both arms
fixed compaction token counting in the **.NET implementation**
(`CompactionMessageIndex.cs`); the on arm added .NET tests and spent 4.7M
tokens doing it — the round's most expensive cell. The symptom never names the
stack and the corpus carries two implementations. Retrieval did not rescue
this: lore indexes dotnet too, and its hits evidently reinforced the wrong
stack. Prompt fix carried to Next.

**W5 (anthropic usage) — off outworked on.** Off ground through 28 turns and
produced the complete fix (incremental accumulator, both event sites). On
never called lore (0 calls — the only on-cell with none), stopped at 14 lines
that fixed the cumulative-input double-count but left the `message_start`
seed, failing the +1 assertion. Less work, worse fix.

## Retrieval behaviour

| task | on-arm lore calls |
| --- | ---: |
| W1 | 1 |
| W2 | 2 |
| W3 | 4 |
| W4 | 4 |
| W5 | 0 |

Adoption is present but shallow (11 calls total vs 62 across 20 QA cells for
the same model). No pass-B-style relevance grading exists in this program
either; nothing here says the returned pointers were good, only that they
were requested — same gap as RCB, carried again.

## Qualitative verdicts

Arm-blind Sonnet 5 batch judge (Anthropic Batches API), tertiary only.
*Pending at time of writing — this section is filled by `verdicts.jsonl` when
the batch lands; the deterministic grades above are unaffected either way.*

## What this round says

**The write program measures a different thing than the QA program, by
construction and now by evidence.** The QA result (big efficiency win, quality
rescue on location failures) came from a mechanism — off-arm subagent fan-out
during search — that write tasks on this model simply do not trigger: location
was nearly free here (8/10 cells found the golden file), and the hard part was
getting the *contract* and the *completeness* of the fix right inside it.
Retrieval helps with "where"; these tasks fail on "what exactly".

**Read the 1/5-vs-1/5 with its artifacts removed.** On W1 and W4 both arms
failed identically for task-design reasons. On the three discriminating
tasks: on won W3 outright, got closer on W2, and lost W5 while — tellingly —
not using lore at all there. That is not a quality claim in either direction;
it is a hypothesis for round 2: lore's effect on write tasks may hinge on
whether the agent actually leans on it during the *understanding* phase, not
the locating phase.

**The efficiency premium is real and explainable.** The on arm pays for MCP
context and search results in every turn's cache reads without the offsetting
fan-out collapse that made lore cheap in the QA round. On this task shape,
lore is currently a cost, not a saving — with n=5, one answerer, no repeats.

## Deviations from the task-set draft, stated explicitly

- The round ran on the **unfrozen draft** task set, per Wrysk's go-ahead
  (2026-08-19: pytest allowed, selection approved, round + batch authorized).
  No `_task_set` id or prompt hashes existed yet; the prompts of record are
  the subrepo's `tasks/W*/symptom.txt` @ `748b63b` and the runner's template
  in `run_writer.py` @ the same commit.
- Grading ran with one known-flaky exclusion
  (`test_ui_memory_regression.py`, Chrome-driven, fails 3/4 runs at the
  unpatched pin in this environment; evidence in `grade.py`).

## Harness changes made during this round

- `run-write.sh` env-prefix bug fixed after the first W3 smoke attempt
  produced two rc=127 no-op cells (smoke stamp `smoke-0819`, discarded).
  Round cells are unaffected (`smoke2-0819` and `rcbw1-0819` ran after).
- RCB-W machinery moved into the bench subrepo mid-round
  (`bench/rcb/rcbw/` @ `748b63b`); WSL-side deployed copies unchanged.

## Carried over and next

1. **W1 prompt fix**: the symptom must pin the merged contract behaviorally
   ("what was already stored must remain readable; nothing new is kept") or
   the task keeps grading coherent alternatives as failures.
2. **W4 prompt fix**: name the stack ("my Python agent") — dual-stack corpus
   plus stack-blind symptom measures coin flips, not ability.
3. **Freeze the task set** (with the two prompt fixes) before round 2:
   `_task_set` id + prompt hashes, per bench convention. Needs Wrysk.
4. **Round 2 questions**: do the artifacts-removed patterns hold on repeats?
   Does W5-on's zero-adoption repeat, and does adoption correlate with fix
   completeness? Consider 2–3 repeats of this same round before adding
   answerers.
5. **Retrieval relevance grading** — carried from RCB, still absent.
6. **Judge verdicts**: fold `verdicts.jsonl` in when the batch completes;
   audit the W1 cells' labels against the known contract-artifact reading.
