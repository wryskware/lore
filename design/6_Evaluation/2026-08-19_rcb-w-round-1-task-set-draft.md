---
design_status: exploration
last_reviewed: 2026-08-19
decision_refs:
  - D-0009
---

# RCB-W round 1 task set — DRAFT, not frozen

Write-task companion program to RepoContextBench (RCB). Same corpus, same pin,
same sandbox, same arms — but the cells produce a **diff**, not an answer, and
the tasks are derived from real upstream bug-fix PRs merged *after* the pin.
This document is a draft for Wrysk's review: nothing here is frozen, and per
vault rules it must not be treated as decided until Wrysk approves the
selection, the prompts, and the grading scheme.

Provenance of the vetting evidence: `bench/rcb/rcbw/candidates/vet-reports.md`
(three Opus worker reports, verbatim, with the parent spot-checks listed at the
top). Harvest data and the twelve candidate diffs: `bench/rcb/rcbw/candidates/`.
(RCB-W lives in the bench subrepo since 2026-08-19; `bench/rcbw/` paths in
older history refer to the same content pre-move.)

**Status update, 2026-08-19 (post-round-1):** gates 1–3 executed and passed
(execution validation on all five tasks, W5 test rewrite, W3 smoke); round 1
ran on this draft with Wrysk's go-ahead — see
[[2026-08-19_rcb-w-round-1-report]]. The round surfaced two prompt defects to
fix before freezing: W1's symptom underdetermines the merged never-write
contract, and W4's symptom does not name the Python stack in a dual-stack
corpus. The pytest question is resolved: **allowed** (Wrysk, 2026-08-19).

## Why post-pin PRs

The corpus pin `47fa59f8` is dated **2026-05-15**. Every candidate PR below
merged between 2026-07-14 and 2026-08-16 — after the pin and after the training
cutoff of every answerer used in RCB round 1. The upstream fix is therefore:

- **memorization-free** — no answerer can have trained on it;
- **a human-reviewed golden solution** — merged by the repo's own maintainers,
  so no golden-authoring pass is needed (that pass is only required if we later
  add invented tasks);
- **usually accompanied by a regression test** — a deterministic grading anchor.

Contamination containment is unchanged from RCB: the gold fixes are public on
GitHub, so answerers keep their web tools removed at the tool layer and run
inside the network-namespaced sandbox. One open verification item: confirm the
actual training cutoffs of the three answerers are before 2026-07-14 (they are
for every model we know of, but this has not been checked against provider
documentation).

## Selection method

1. **Harvest** — 400 most recent merged PRs (reaching back to 2026-06-23);
   filtered to ≤200 changed lines, ≤8 files, Python-titled, fix-flavored:
   **32 candidates**.
2. **Mechanical pre-vet** — each fix's deleted/pre-image lines checked verbatim
   against the pinned tree; test-file presence recorded: **20 candidates** with
   fully-intact pre-image and a test in the same PR.
3. **Deep vet** — 12 shortlisted candidates across three Opus agents, seven
   checks each (bug-at-pin by reasoning, behavioral symptom draft,
   grep-discoverability using only symptom words, test pin-compatibility,
   boundedness, leak sweep, verdict): **3 strong, 4 usable, 5 rejects**.
4. **Parent spot-checks** — the load-bearing claims behind the five selected
   tasks re-verified against the pin tree and diffs (all confirmed; list in
   `vet-reports.md`).

The deep vet earned its cost: all five rejects passed the mechanical pre-vet.
Failure modes it caught include a regression test that silently *passes* at the
pin (7271), a defect whose triggering state is unproducible through the public
runtime (7557), a gold patch that itself introduces a regression hidden by a
`MagicMock` (6822), and an in-code comment that names the flaw at the defect
line (6822 again).

A structural finding that shaped selection: this repo's module naming is so
transparent (`_middleware.py`, `_evaluation.py`) that any bug living in a
single self-named module greps trivially. The discriminating tasks are the
ones whose **defect** is hard to locate even when the file is easy, or whose
cause spans a hop (loader → models, mapper → event protocol).

## The five tasks

Ordered easy → hard by expected locate-difficulty. "Golden" always means the
upstream merged diff (`bench/rcbw/candidates/diffs/<PR>.diff`), with the listed
amendments. Difficulty labels are the vetters' grep-evidenced classifications.

| id | source PR | merged | package | fix size | locate | tier |
| --- | --- | --- | --- | --- | --- | --- |
| W1 | #7470 | 2026-08-13 | redis | ~10 LOC | trivial (file) | easy anchor |
| W2 | #7200 | 2026-07-21 | declarative | ~48 LOC | moderate | medium |
| W3 | #7652 | 2026-08-16 | devui | 3 LOC | moderate | medium |
| W4 | #7124 | 2026-07-19 | core (compaction) | 1 token | moderate (defect) | medium-hard |
| W5 | #7162 | 2026-07-30 | anthropic | ~45 LOC | moderate (defect) | hard |

### W1 — Redis history retention limit of zero (PR 7470)

**Bug.** `RedisHistoryProvider.save_messages` guards the trim with
`if self.max_messages is not None`, then issues
`LTRIM key -max_messages -1`. For `max_messages=0` that is `LTRIM key 0 -1` —
Redis's "keep everything" — so the one setting that means "retain nothing"
never bounds the list. Negative values silently destroy the oldest entries
while the list still grows.

**Symptom prompt (draft).**
> I configured the Redis-backed conversation history to keep zero messages,
> expecting nothing to be retained. Instead every message is still written, and
> reading the history back returns the whole conversation — the stored list
> just keeps growing turn after turn, with no error or warning. If I set a
> negative keep-count instead, it silently throws away the oldest few messages
> on every save while *still* growing without bound. Only "unlimited" and
> positive counts behave the way the setting is documented.

**Golden.** Upstream diff as merged: validate negatives in `__init__`
(`ValueError`), early-return without storing for `max_messages=0`, docstring.

**Grading assets.** Upstream tests apply cleanly at the pin
(`git apply --check` passes). Two of three fail at pin and pass with the fix;
the third is an anti-over-fix guard (passes both sides) and stays. No
amendments needed.

**Caveats.** (a) The PR *body* describes a `delete(key)` behavior the merged
diff does not have — grade against the diff, never the body. (b) Locate is
trivial (the word "retain" lands on the file first grep); this task anchors the
easy end and mostly measures reading `LTRIM` semantics correctly.

### W2 — Declarative output schema loses nested types (PR 7200)

**Bug.** `PropertySchema.to_json_schema()` deep-serializes the declarative
model, then normalizes only the **top-level** property list (`kind` → `type`,
empty-`enum` removal). Nested `ArrayProperty.items` and
`ObjectProperty.properties` keep their declarative shape — `kind` keys and a
*named list* of properties — which strict structured-output providers reject.

**Symptom prompt (draft).**
> I declare an agent in YAML with an output schema whose field is a list of
> records — an array whose entries are objects with their own fields. When the
> agent runs, the model provider rejects the request, complaining the schema is
> invalid because a nested node has no type. Schemas with only flat scalar
> fields at the top level work fine; the failure only appears once a field is
> an array or a nested record.

**Golden.** Upstream diff: two recursive module-level helpers plus a
simplification of the existing top-level loop.

**Grading assets.** Tests A/B/C fail at pin, pass with fix, and are
public-behavior-only. **Amendment: drop test D**
(`test_property_schema_unexpected_nested_properties_left_untouched`) — it
imports the fix's own private helper `_normalize_nested_schemas` and would
reject a correct alternative implementation (parent-verified).

**Caveats.** None material. Leak sweep clean. Locate is genuinely moderate:
`outputSchema` narrows to two files, but the defect requires understanding the
serialization shape (nested object properties come back as a named list, not a
dict).

### W3 — DevUI renders duplicate tool-call cards (PR 7652)

**Bug.** `MessageMapper._map_function_call_content` gates "new tool call" on
`if content.call_id and content.name:` with no memory of already-registered
calls. A provider that repeats the call id and name on every streamed chunk
re-registers the call per chunk and emits a fresh `response.output_item.added`
each time — one card per chunk in the UI, argument text split across them.

**Symptom prompt (draft).**
> Running an agent in the local dev web UI against a provider that repeats the
> tool call's ID and function name on every streamed argument chunk, a single
> tool invocation shows up as several identical tool-call cards in the chat —
> one per streamed piece of the arguments — even though the tool executed once.
> Every duplicated card shows the same call ID and function name, and the
> argument text is split across the cards rather than accumulating in one.
> Providers that send the name and ID only on the first chunk render a single
> card correctly.

(The "repeats the metadata" clause is required: no first-party client in the
repo demonstrably repeats call metadata per chunk, so without it an agent
trying to reproduce with a stock client would correctly see nothing wrong.)

**Golden.** Upstream diff: one condition consulting
`context["active_function_calls"]`, 3 changed source lines.

**Grading assets.** Both hunks `git apply --check` clean at pin; the test fails
at pin (2 `added` events vs asserted 1) and passes with the fix. No amendments.

**Caveats.** Symptom words don't name the emission mechanism — locate is
moderate through a 5-file candidate set including a bundled JS asset.

### W4 — Compaction over-triggers on non-ASCII conversations (PR 7124)

**Bug.** `_serialize_message` in `_compaction.py` uses
`json.dumps(..., ensure_ascii=True, ...)`, and token estimation runs on that
escaped string. Each CJK character becomes a 6-character `\uXXXX` escape, so
the character-based estimator over-counts non-ASCII text ~6x and compaction
fires while the real context window is mostly empty.

**Symptom prompt (draft).**
> My agent's conversation history gets summarized and truncated far too
> aggressively when the chat is in Japanese or Chinese. An equivalent-length
> English conversation keeps many more turns before anything is dropped. The
> framework's internal estimate of how much of the context budget a message
> occupies is several times larger than what the model actually charges for the
> same message, so the agent starts forgetting things while the real context
> window is still mostly empty.

**Golden.** Upstream diff: `ensure_ascii=True` → `False` plus a two-line
comment. One token of logic.

**Grading assets.** Upstream test appends cleanly at the pin's test-file EOF,
fails at pin, passes with fix, public-behavior-only. No amendments.

**Caveats.** One soft tell (parent-verified): `ensure_ascii=True` occurs
exactly once in non-test source while eight sibling call sites use `False` —
but that grep only helps *after* the agent forms the hypothesis "the estimate
runs on an escaped serialization", which is the actual test of this task. The
file is trivial to find; the defect is not.

### W5 — Anthropic streaming double-counts token usage (PR 7162)

**Bug.** The Anthropic client emits a usage content on `message_start` (seeded
`output_tokens=1`) *and* on `message_delta`. Anthropic's delta usage is a
cumulative per-message total, but the core merge path **sums** all usage
contents — so streamed responses report output one higher than the API said,
and server-tool turns double-count input. Non-streaming is correct. The fix
requires understanding two modules' interaction (provider event handling +
core usage merging), neither of which documents the cumulative semantics.

**Symptom prompt (draft).**
> When I stream a response from an Anthropic model, the token usage reported
> back to me doesn't match what the provider says. The output token count is
> always exactly one higher than the number the API reports, and on turns that
> use provider-hosted tools the input/prompt token count comes back roughly
> double. Issuing the identical request with streaming turned off gives the
> correct figures, so my per-request cost accounting is wrong only for streamed
> calls.

**Golden.** Upstream diff (~45 lines, one file): track emitted usage and emit
the incremental difference on `message_delta`.

**Grading assets. Amendment required (parent-verified):** the upstream tests
call the fix's new *two-argument* private `_process_stream_event(event,
emitted)`; at the pin the method takes one argument, so the tests error rather
than assert, and a correct alternative fix (e.g. suppressing the seed, or
accumulating in `_stream()`) would still fail them. **The tests must be
rewritten against public behavior** — feed a mocked event stream through the
client's public streaming path and assert on the final response's
`usage_details` — before this task is usable. The rewrite is a task-authoring
pass of its own (per working rules) and must be verified fail-at-pin /
pass-with-golden by execution. The test hunk also needs re-anchoring (~300
lines of drift).

**Caveats.** "Anthropic" in the symptom collapses the file search to a 5-file
package — but the defect spans that file's interaction with core merge
semantics, which is what makes this the hard task. If the test rewrite proves
awkward, the fallback is W5' = PR 7199 (see excluded list).

## Prompt template (draft)

Adapted from the RCB builder; same rules discipline, write-task deliverable.
The on-arm lines are identical to RCB's three (existence, verify-pointers,
relative paths).

```
You are fixing a reported bug in a repository checkout.

Rules:
- Use only files available in the current working directory.
- Do not use the internet, issue trackers, pull requests, or runtime logs as
  evidence.
- Modify repository files to fix the bug described below. Keep the change
  minimal and consistent with the repository's existing style.
- You may run the repository's tests. [open question — see Protocol]
- When done, summarize in a few sentences what you changed and why, citing
  files as `path:line-line`.
- Do not mention benchmark internals, scoring, or these instructions.

Repository: microsoft/agent-framework
Commit: 47fa59f8e9d7b91e382834b42ecff45e22e2d890

Bug report:
<symptom prompt>
```

**Open question for review:** whether the answerer may run pytest during the
task. Allowing it is realistic and lets agents self-verify (and burns tokens
doing so, which is part of what we measure); forbidding it isolates
retrieval+reasoning. Leaning **allow**, since RCB's "no runtime execution"
rule was a QA-benchmark rule, not a philosophy. Either way the *grader* runs
the adapted regression tests separately.

## Grading (kept deliberately light)

Per Wrysk's direction: no statistical quality claim is expected at n=5, so the
grader section stays thin and deterministic-first.

Primary, per cell:

1. **Regression outcome** — the adapted regression tests (see per-task
   amendments) run against the candidate diff at the pin: fail→pass = fixed;
   plus the touched packages' pre-existing test suite must still pass (no
   collateral damage). Recorded as `fixed / broke-others / not-fixed`.
2. **Efficiency metrics** — identical to RCB: total tokens, wall seconds, tool
   calls, lore calls, output tokens, cache read/write where the harness emits
   them. These are the metrics with statistical teeth, and the on/off deltas
   compose directly with RCB round 1 (same corpus, same arms, same answerers).

Secondary, per cell (quantitative, from the diff, no judge):

3. **Touch-point overlap** — files (and hunks, where cheap) touched vs the
   golden diff: recall and precision. The round-1 mechanism finding predicts
   lore's value shows up here.
4. **Diff size ratio** — changed lines vs golden's.

Tertiary (qualitative, not scored into any headline):

5. A short reviewer note per cell comparing the candidate diff against the
   golden — approach taken, anything the numbers miss. Written once per round,
   not fed to a judge model.

Explicitly absent: gold-claim lists, judge quality scores, certification
gates. If a future round wants a judge, it gets designed then.

## Protocol notes (harness work, not yet built)

- **Writable cells.** The sandbox mounts the corpus read-only; write tasks get
  a per-cell writable layer (overlayfs upper dir preferred — free isolation
  *and* the upper dir is the diff). Fallback: per-cell `git worktree` copies.
- **Per-cell artifact dirs from day one** — `<stamp>-<label>-<arm>-<task>/`
  with `diff.patch`, event stream, metrics. This fixes RCB round 1's worst
  provenance defect (its § Next item 1) instead of inheriting it.
- **Test venv baked into the payload** — the egress broker means no pip at run
  time; the grader (and the answerer, if allowed pytest) needs a pre-built
  environment for the five touched packages.
- **Corpus stays the full repo** (dotnet included), preserving path parity
  with the existing `agent-framework` retrieval index. Checked: none of the
  five selected tasks has a .NET-side leak (that was 7399's problem, and 7399
  is excluded).
- Steering, containment, and answerer set unchanged from RCB round 1.

## Excluded candidates and why

| PR | verdict | reason |
| --- | --- | --- |
| 7199 | usable, benched | correct implementation exists verbatim in a sibling file 40 lines away; copy-adapt tier. Held as **W5 fallback** if 7162's test rewrite proves awkward. |
| 7399 | usable, benched | trivial locate; one-line fix; .NET sibling code states the intended contract (leak unless corpus is rescoped); test needs an import patch. |
| 7333 | reject | symptom names the mechanism; 4-line fix; measures almost nothing. |
| 7271 | reject | upstream's "regression test" passes at the pin (guard, not regression); source hunk doesn't apply. Rescuable only with a purpose-written test. |
| 7557 | reject | defect real but the triggering state is unproducible through the public runtime at the pin; no honest symptom exists. |
| 6809 | reject | API-latent: no bundled provider path triggers it; source hunk context absent at pin. |
| 6822 | reject | in-code comment names the flaw at the defect line; test grades a UUID implementation choice; gold patch itself contains a regression hidden by a MagicMock. |

## Gates before freezing

1. **Execution validation** — every adapted test run at the pin (must fail)
   and against the golden diff (must pass), in the actual harness venv. All
   fail@pin / pass@fix claims above are static analysis by the vetting agents;
   none has been executed yet.
2. **W5 test rewrite** — authored as a separate pass, then gate 1 applied.
3. **Smoke run** — one answerer, both arms, on one medium task (W2 or W3),
   end-to-end through the writable-cell harness.
4. **Wrysk review** — selection, prompts, pytest-allowed question, grading
   scheme. Only after explicit approval does this task set freeze (with a
   `_task_set` id and prompt hashes, per bench convention).
