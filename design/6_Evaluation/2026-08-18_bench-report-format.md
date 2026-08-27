---
design_status: exploration
last_reviewed: 2026-08-18
decision_refs:
  - D-0009
---

# E2E bench round report — required format

Five rounds have been written ([[2026-08-16_e2e-round-1-report]],
[[2026-08-17_e2e-round-2-report]], [[2026-08-17_e2e-round-3-lexomancy]],
[[2026-08-17_e2e-round-4-lunamax]], [[2026-08-18_e2e-round-5-qwen27b]]) with no
codified format. Each one re-invented its tables, dropped a different subset of
the numbers, and buried the round's provenance in prose. This document is the
format proposed for every round report from round 6 onward.

It governs **round reports only**. Task sets, answer keys and
[[2026-08-17_grading-protocol]] have their own homes; this says nothing about
criteria.

## What the audit found

Every defect below is present in at least one shipped report.

| # | Defect | Evidence |
| --- | --- | --- |
| 1 | **Per-cell lore call counts vanish after round 1.** Round 1 had a whole adoption matrix (`lore calls / total tool calls`, per cell). Rounds 2-5 report only a round-level aggregate in prose ("12 searches across 5 on-arm cells"), so no claim about a cell can be checked against its call count. | R1 § Lore adoption vs R2-R5 |
| 2 | **No report names a single cell directory.** Every number in the program is unattributable to the artifact that produced it. `bench/results/<stamp>-<cell>/` exists and is never cited. | all five |
| 3 | **The measured columns change every round.** Output tokens: R1 only. Cache read: R2, R3 only. Wall clock: R1 per cell, R2 aggregate, R3 as `sec`, R4 one line, R5 absent. Exit codes: never. | all five |
| 4 | **Score denominators are unstated and shift.** R1 scores `/15` per arm; R2's headline "Score: 8.0 / 8.0" reads as eight-out-of-eight but means off 8.0 / on 8.0 out of 10; R3-R5 are per-arm out of 5. | R2 headline |
| 5 | **Setup of record is optional in practice.** R1 has none (scattered through Validity notes), R2 and R3 have tables with different rows, R4 and R5 have no setup table at all — the reader chases a prior round for the pin, index and binary hash. | R4, R5 |
| 6 | **Excluded and failed cells are handled ad hoc.** R3 mentions three killed cells in passing, R5 a smoke-test duplicate, R4 a 19-minute hang and a containment breach — none as a field, all as prose a writer can forget. | R3, R4, R5 |
| 7 | **Grader identity, effort and audit status are buried.** Whether two rounds share a grading scale is load-bearing for every cross-round table, and is stated — when it is stated — mid-paragraph. | R3 vs R4 vs R5 |
| 8 | **Variance is remembered only when someone trips over it.** R1 has a replicates table, R5 an ad-hoc 2.3x note, R2-R4 nothing — yet every per-cell figure in the program is n=1. | all five |
| 9 | **Grading evidence is discarded.** `verdicts.json` records criterion-by-criterion `met` / `evidence` / `missing` per cell. Reports keep the number and throw away which criteria each arm missed — the most useful thing in the file. | all five |
| 10 | **No reproduce block.** Nothing records the `run.ps1` / `pack.py` / `grade.ps1` invocations that produced the round. | all five |
| 11 | **Prompts pasted verbatim into numeric tables.** R3-R5 each carry the same five prompt strings inside the results table, producing an eleven-column table whose first column is 200 characters wide. | R3, R4, R5 |
| 12 | **Formatting drift.** Mixed ASCII and Unicode minus, percentages with and without sign, thousands separators applied inconsistently, `sec` vs `wall` vs `wall_ms` for one quantity, undefined jargon (`din`) introduced before its caption. | R1 vs R3 |

Two things the existing reports do **well**, which this format keeps: R3's
retroactive `## Correction` block, and R5's explicit carry-over of unbuilt items
from the prior round.

## Principles

1. **Mechanical numbers are generated, never typed.** Anything derivable from
   `metrics.json`, `pack.py` or `verdicts.json` is emitted by tooling and pasted
   whole. A writer's judgment belongs in the prose, not in the arithmetic.
2. **Every number is traceable to a cell directory.** A round report that cannot
   be walked back to `bench/results/` is a claim, not a measurement.
3. **Omission must be explicit.** A field that does not apply says so
   (`n/a — vLLM does not report cache_read`); it is never silently dropped.
4. **Comparability is declared, not inferred.** Each round states what it may be
   compared against and what it may not, in a fixed field.
5. **Prose earns its place.** Tables carry the numbers; prose exists to say what
   the numbers mean and what is still unknown.

## Required structure

Sections in this order. A section with nothing to report keeps its heading with
the word `None.` under it — present-and-empty, never absent.

### Front matter and title

```yaml
---
design_status: exploration
last_reviewed: <date>
decision_refs:
  - D-0009
---
```

Filename: `<date>_e2e-round-<n>-report.md`. Rounds 3-5 named files after their
subject (`-lexomancy`, `-lunamax`, `-qwen27b`); the round number is the stable
key, and the subject belongs in the H1.

H1: `# E2E round <n> — <one-line subject>`.

### 1. Round card

A fixed key-value table, first thing under the H1. Every row required.

| field | value |
| --- | --- |
| round | 6 |
| ran | 2026-08-20 |
| question | the one thing this round was run to find out |
| model(s) | `Qwen/Qwen3.8-27B-FP8` @ effort medium |
| arms | off, on |
| repos / task set | lexomancy / `round-2` |
| cells | 10 planned, 10 graded, 1 excluded (§ 7) |
| grader | luna `max`, pass A + pass B |
| audit status | not audited — or: sample of N, agreement M/N |
| comparable to | rounds 4, 5 (same grader scale, same index) |
| not comparable to | round 3 (grader `high`), rounds 1-2 (different corpus) |
| reproduce | `.\run.ps1 -Matrix -Models qwenrp -Repos lexomancy`; `python pack.py --cells '20260820-*' --batch repo-task`; `.\grade.ps1 -Round 20260820 -Pass A -Model lunamax` |

### 2. Headline

Three to five bullets, one sentence each, each resting on a number that appears
in a table below. No bullet may introduce a figure found nowhere else.

### 3. Setup of record

Fixed rows, always present, `n/a` only where genuinely inapplicable: pins (git
SHA or cm changeset, per repo), retrieval project with file / chunk counts and
embedded percentage, embedding model and endpoint, authority profile per repo,
pinned `lore-mcp.exe` sha256, plugin versions, task set id, corpus scrub state,
slot map.

R4 and R5 inherited this by reference from a prior round. A row may read
`unchanged from round <n>` — the row itself stays.

### 4. Cell ledger *(generated)*

**One row per cell, every cell**, including excluded ones (struck through). This
is the table the current reports are missing, and it subsumes most of what they
scatter.

| cell | task | arm | score | conf | wall s | input | cache rd | output | tools | lore | uptake | exit |
| --- | --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `20260820-101500-qwenrp-lexomancy-off-T1` | T1 | off | 1 | high | 214 | 159,226 | 0 | 3,410 | 17 | 0 | — | 0 |
| `20260820-101500-qwenrp-lexomancy-on-T1` | T1 | on | 0.5 | high | 331 | 846,841 | 0 | 5,002 | 30 | 11 (9s/2x) | 3/24 | 0 |

- `cell` is the directory name under `bench/results/`, verbatim.
- `lore` is total lore calls with the search/expand split in parentheses —
  `9s/2x`. Expands were invisible to rounds 1-2's uptake metric (fixed in
  `ca685d5`), and the split is the difference between "searched once" and
  "searched once and drilled in".
- `uptake` is `paths opened / paths returned`, from `pack.py`; `—` on off-arm.
- `conf` is the pass-A `confidence` field. A round where every T5 is `low`
  should be visible at a glance, not in a footnote.
- `exit` is the process exit code. Non-zero is never silently graded.

### 5. Totals and deltas *(generated)*

| | off | on | delta |
| --- | ---: | ---: | ---: |
| score | 4.5 / 5 | 4.0 / 5 | -0.5 |
| wall | 2,041s | 1,884s | -8% |
| input tokens | 4,308,642 | 2,733,712 | -37% |
| cache read | n/a | n/a | vLLM does not report it |
| output tokens | 21,880 | 18,455 | -16% |
| tool calls | 115 | 83 | -28% |
| lore calls | 0 | 41 | — |

Score always carries its denominator. A token column states its basis when that
is not the plain OpenAI `usage` split — R5's vLLM `prompt_tokens` is the
analogue of luna's `input + cache_read`, and that note belongs directly under
this table rather than four paragraphs later.

### 6. Score detail *(generated from `verdicts.json`)*

Per task, per arm: which criteria are in `missing`, and any
`fabricated_citations`. The full criterion list stays in the JSON.

| task | off | on | off missing | on missing | flagged citations |
| --- | ---: | ---: | --- | --- | --- |
| T1 | 1 | 0.5 | — | `battle_simulator_step` | on: 4 imprecise (verified — lines resolve, land on `///` openers) |

Every entry in `fabricated_citations` is checked by the parent against the repo
at the pin **before the report ships**, and the verified outcome written beside
it. Rounds 3 and 5 each found a grader false positive this way, and round 5
found two errors in the frozen key by doing it twice.

### 7. Excluded, failed and re-run cells

A table, never prose: cell id, what happened, disposition.

| cell | what happened | disposition |
| --- | --- | --- |
| `x-20260820-1101-...-on-T3` | opencode `database is locked` at 1.1s | re-run as `20260820-1104-...`; quarantined |

If nothing was excluded, `None.` A round reporting 10 cells that launched 13
must show the other three here.

### 8. Retrieval behaviour *(on-arm only)*

The round-level line, in the shape rounds 2-5 converged on by accident:

> **N searches across M on-arm cells returned P unique paths; U were used
> (u%) — R read, X expanded in place — and C (c%) survive into the final
> answer.**

Then per-cell pass-B verdicts as a table (cell, verdict, one-line diagnosis),
not as prose. `verdict` is one of the protocol's five values.

State pass B's blind spot once, by reference: it judges topical relevance and
not sufficiency-for-a-correct-answer, so a `relevant-and-used` verdict on a cell
scored 0 is not a contradiction ([[2026-08-17_e2e-round-3-lexomancy]]).

### 9. Variance and sample size

Required, and required to be honest. Default text when nothing was repeated:

> Every per-cell figure here is n=1. The one measured repeat in this program
> (round 5, off-arm T2 run twice) spread 2.3x on input tokens and 2.8x on tool
> calls under identical conditions; round 1's four qwen T5 replicates spread up
> to 3x. Read per-cell deltas smaller than ~2x as noise. Round totals, averaged
> over five tasks, are steadier.

If cells were repeated, a replicates table replaces that paragraph.

### 10. What this round says

The analysis. Free-form prose with subheads as needed — this is where a round
earns its keep, and the format deliberately does not constrain it beyond
requiring that every figure it cites already appears in a table above.

Where the round repeats a prior task set, a cross-round score table goes here
(R4 and R5 both have one; R3 does not), with the comparability caveat from the
round card repeated in one line beneath it.

### 11. Harness changes made during this round

One bullet per change: commit SHA, one sentence, and whether it invalidates any
cell in this round. Rounds 2 and 3 did this well; 4 and 5 folded it into prose.

### 12. Carried over and next

Two lists, kept separate:

- **Carried over** — items a prior round raised that are still unbuilt, each
  naming the round that raised it. R5's containment assertion and per-cell
  timeout have been open since round 4, and this is the field that makes that
  visible.
- **Next** — new items this round raises, ordered, each with what it would
  settle.

## Number and formatting rules

- Wall clock in whole seconds; header `wall s`. Never `sec`, never `wall_ms`.
- Token counts with thousands separators, no unit suffix.
- Deltas as signed whole percent, ASCII `+` / `-`, always on-relative-to-off:
  `-37%` means the on arm spent 37% less. State that direction once, under the
  first table that uses it.
- Scores as `0`, `0.5`, `1`; totals always written `X / N`.
- No Unicode minus in numbers, no smart quotes, no emoji.
- Cell ids and `file:line` citations in backticks.
- Bold reserved for a number the reader must not miss — at most two per table.
- Prompts are **never** pasted into a numeric table. Reference tasks by id and
  link the task set; if a report wants them inline, they go in an appendix.
- A table that would exceed ~13 columns gets split, not compressed.

## Provenance — where each field comes from

| field | source | mechanical? |
| --- | --- | --- |
| cell, model, repo, arm, task, slot, project, pins, prompt sha, binary sha, wall, tokens, tool_calls, lore_calls, exit_code | `bench/results/<cell>/metrics.json` | yes |
| lore search/expand split, unique paths, uptake, answer overlap | `bench/pack.py` | yes |
| score, criteria met/missing, fabricated_citations, confidence | `bench/grades/<stamp>-passA/*.json` | yes |
| pass-B verdict, diagnosis | `bench/grades/<stamp>-passB/*.json` | yes |
| suite result (T5) | `bench/results/<cell>/suite-result.txt` | yes |
| index size, embedded %, authority profile, plugin versions | `lore status` at run time | yes, if captured |
| question, headline, comparability, analysis, carried-over, next | the writer | no |

Everything marked `yes` should be emitted by a `pack.py --round-summary <glob>`
step that prints sections 4, 5, 6 and the retrieval line as paste-ready
Markdown. Until that exists, the writer assembles them by hand from the same
files, and the omissions catalogued above will keep recurring —
**the generator is the enforcement mechanism; the format alone is not.**

Two fields have no source today and need harness work before they can be filled
honestly:

- `time_compacting` has stored the literal string `'None'` for all 127 cells to
  date, so compaction has never been measured
  ([[2026-08-18_e2e-round-5-qwen27b]]). Until that is fixed the cell ledger
  omits it rather than printing a value that means nothing.
- Per-cell `output` tokens are captured in `metrics.json` but have not been
  reported since round 1; no work is needed, only inclusion.

## Amending a published round

A round report is the record of what was claimed and on what evidence. When a
later round overturns it:

- Add a `## Correction (round <n>)` block **immediately under the H1**, stating
  what is wrong, what replaced it, and what survives.
- Leave the original text unedited below it.
- Add a line in the superseding round's § 10 pointing back.

Round 3 did exactly this, and it is the reason the program's record is readable
at all. It is required here rather than merely admirable.

## Skeleton

```markdown
---
design_status: exploration
last_reviewed: YYYY-MM-DD
decision_refs:
  - D-0009
---

# E2E round N — <subject>

| field | value |
| --- | --- |
| round | |
| ran | |
| question | |
| model(s) | |
| arms | |
| repos / task set | |
| cells | |
| grader | |
| audit status | |
| comparable to | |
| not comparable to | |
| reproduce | |

## Headline
## Setup of record
## Cell ledger
## Totals and deltas
## Score detail
## Excluded, failed and re-run cells
## Retrieval behaviour
## Variance and sample size
## What this round says
## Harness changes made during this round
## Carried over and next
```
