# train/ — scouter trajectory generation

A harness that turns repository questions into multi-turn tool-calling SFT
trajectories, in the TRL conversational format, for fine-tuning a small local
**scouter** model.

The behaviour being taught, in one line:

> call lore `bundle` first, then keep exploring with `search`, `grep`, `read`
> and `bash` until you can answer, and finish with an answer that cites
> repository-relative `path:start-end` spans.

A teacher agent — `gpt-5.6-luna` at max reasoning effort, driven through
`opencode run` — answers each question inside a commit-pinned snapshot of the
repository the question is about, with lore's MCP server wired in so `bundle`
and `search` are real tool calls. Its session is recorded, reshaped, path-
normalised, graded against the question's reference answer, and validated.

**Status: piloted, not scaled.** `--dry-run` is proven end to end, and two real
pilots have run — eleven cells over two repositories, serially and at
concurrency 2 — producing trajectories that pass the validator. Nothing has
been trained on them. See "What is real and what is stubbed".

---

## Pipeline

```
generate.py   question  ->  teacher session  ->  work/raw/<batch>/<qid>/agent.ndjson
convert.py    raw       ->  work/data/<batch>.converted.jsonl      (+ validator)
grade.py      converted ->  work/data/<batch>.train.jsonl          (+ validator)
```

Each stage is independently re-runnable and writes its rejects beside its
output with a reason per row. `generate.py --dry-run` fabricates genuine
opencode-shaped event streams for a two-question fixture, so `convert.py` and
`grade.py` then run their **real** code paths — no teacher call, no lore daemon,
no network.

```bash
python3 generate.py --dry-run
python3 convert.py  --batch pilot-01
python3 grade.py    --batch pilot-01
```

Everything the harness produces lives under `work/` and is gitignored. A batch
is reproducible from `work/manifests/<batch>.json` plus the pinned commits.

---

## Contamination boundary

This is the constraint that shapes every other choice here, so it is stated
first and it is absolute.

**Never in training data:**

- `microsoft/agent-framework` and the RepoContextBench task set. RCB is a
  held-out evaluation. A scouter trained on its corpus would make every RCB
  number meaningless, and the numbers are the only reason RCB exists.
- Whatever corpus is ultimately chosen as retrieval-eval corpus two.

**In training data:** the 26 repositories and 260 questions of
`TIGER-Lab/SWE-QA-Pro-Bench`.

The condition this depended on is discharged: **Wrysk chose SWE Atlas as the
second eval corpus (2026-08-27, in session)**, which is exactly what clears
SWE-QA-Pro — repos, questions, and sibling SFT trajectories — for training
use. The coupling itself is recorded in
`design/7_Research/2026-08-27_second-corpus-survey.md`. Should that ruling
ever be reversed, the manifest records the question source per batch, so
identifying and deleting the affected rows is mechanical.

### Separation from `bench/rcb/`

`train/` imports nothing from `bench/rcb/`, shares no files with it, and reads
none of its outputs. The two trees have overlapping mechanisms — both drive
`opencode run --format json`, both parse its NDJSON event stream — and this
harness reimplements the ~30 lines of parsing it needs rather than importing
them. That duplication is deliberate: a shared import is a path by which an
eval harness and a training harness can quietly come to depend on each other,
and there is no amount of convenience that is worth blurring the line between
the corpus a model learns from and the corpus it is judged on.

The one file that *is* shared is the output validator (see Decision 5), which
is outside both trees and belongs to neither.

---

## Decisions

### Decision 1 — question source: SWE-QA-Pro-Bench

`TIGER-Lab/SWE-QA-Pro-Bench` (MIT), `data/test.jsonl`: 260 human-validated
questions, exactly 10 over each of 26 commit-pinned Python repositories.

Why this and not something we author:

- **It grades itself.** Every question ships a human-validated reference answer
  whose prose cites files and line ranges. Verified against the real file:
  250 of 260 references carry at least one parseable file path and 219 carry at
  least one line range, median 3 distinct files per answer. That is an
  automatic quality signal for filtering trajectories, with no judge model and
  no human in the loop — which is the difference between a corpus we can
  actually scale and one we cannot.
- **It is pinned.** `commit_id` per row, so a trajectory is reproducible
  against the exact tree the teacher read.
- **It is licensed.** MIT, confirmed on the Hub.
- **Its repos are long-tail.** 26 real Python projects, not the five everybody
  fine-tunes on.

The reference answers are used **only as an answer key**. They are never shown
to the teacher, never emitted into a row, and never quoted in `meta` beyond the
derived scores. Grading on them is fine; training on them would be teaching the
student to reproduce the key rather than to find the evidence.

Known weakness: Python only. Language diversity has to come from a second
question source later, and lore's flagship consumer is C#/Unity (D-0003). This
corpus teaches *how to scout*, not *how to read C#*.

### Decision 2 — snapshot and index pinning: a per-batch manifest

Each repository is checked out at its pinned commit under a configurable root,
registered with lore, and indexed. `work/manifests/<batch>.json` records, per
repo:

| field | why |
|---|---|
| `repo`, `commit` | what the teacher could read |
| `snapshot` | directory name only, **never an absolute path** |
| `lore_project`, `project_key` | which index answered |
| `index_generation` | the daemon's monotonic index generation |
| `files`, `chunks`, `embedded_chunks`, `daemon_version` | index coverage at generation time |

The pin is deliberately two-sided. The commit fixes what `read`, `grep` and
`bash` could return; the project key plus the index generation fixes what
`bundle` and `search` could return. A trajectory whose generation no longer
matches the live daemon is not *wrong*, but it is no longer reproducible, and
the manifest is what lets anyone tell the difference. The subset that identifies
the pin (`repo`, `commit`, `lore_project`, `lore_project_key`,
`index_generation`) also rides in every row's `meta`, so a row separated from
its batch is still traceable.

Registration and indexing stay the operator's job — `lore add` and `lore index`
are not called from here. A generation script that can mutate index state is a
generation script that can invalidate its own pins mid-batch, and D-0003's
single-authoritative-owner constraint says that state has exactly one owner.
`generate.py` reads `/v1/resolve` and `/v1/status`, and refuses to spend teacher
calls when a project is unregistered, unindexed, or degraded to lexical-only.

"Degraded" is the whole point of that check and it took the first pilot to make
it true. The daemon reports readiness as `embeddings.state == "ready"`, never as
a boolean `ready` key, so the endpoint check was reading a field that does not
exist; the fallback then accepted *any* non-zero `embedded_chunks`, which let a
project 22% through its embedding backlog pin cleanly and answer `bundle`
lexically for the other 78% without erroring. Readiness of the endpoint is not
coverage of the index, and both are now required: `embeddings.state` must be
`ready` and `embedded_chunks` must equal `chunks`.

The operator-owned half has a sharp edge worth stating: bare `lore index`
queues **every** registered project on the daemon, not the one just added. On a
shared daemon, prefer `lore index <project>`.

Deliberately simple: one manifest per batch, no database. A batch is the unit of
everything — one teacher configuration, one prompt, one set of pins.

### Decision 3 — trajectory recording: parse opencode's own event stream

Rejected: a tool-call interception layer (shim binaries on `PATH`, an MCP proxy).

Chosen: parse `opencode run --format json` NDJSON, which is written straight to
a file per cell.

The event stream already carries everything the emission format needs, verified
against real RCB session logs:

- a `text` part per assistant prose block, and a `tool_use` part per call;
- each `tool_use` carries `callID`, `state.input` (the *exact* arguments the
  model produced) and `state.output` (the *exact* result it saw);
- every part is tagged with the `messageID` it belongs to.

Grouping parts by `messageID` in wire order reconstructs assistant turns
exactly, and gets parallel tool calls for free — several `tool_use` parts
sharing one `messageID` is one assistant message with several calls, which is
precisely how the chat template renders them.

Why this beats interception: interception only sees the tools you wrapped, so a
tool you forgot is a silent hole in the trajectory; it changes the teacher's
environment, so the recorded run is not quite the run you would get without it;
and it is live, so a crashed cell loses everything. The event stream sees every
tool including opencode's built-ins, changes nothing, and leaves a parseable
partial trajectory on disk when a cell dies. RCB has driven several hundred
cells through this stream without a parse failure.

The one thing it does not give us is the teacher's hidden reasoning, which is
summarised into the `text` parts. That is the right loss: reasoning traces from
one model are not conditioning we want a different, much smaller model to
imitate.

**The teacher's prompt is discarded.** `convert.py` replaces the whole framing
with the frozen `SCOUT_SYSTEM_PROMPT` and the bare question, so the teacher can
be steered as hard as it needs to be without any of that scaffolding becoming
conditioning the student will never see at inference.

### Decision 4 — grading: citation overlap, with structure gated first

Two classes of filter, in order.

**Structural gates** (`convert.py`) — a trajectory that fails one is not a bad
trajectory, it is not a trajectory:

| reason | meaning |
|---|---|
| `no_events` | the cell produced nothing parseable |
| `no_tool_calls` | the teacher answered from memory |
| `bundle_not_first:<tool>` | the defining behaviour was not demonstrated |
| `ends_on_tool_call` | the session died mid-exploration |
| `empty_answer` | last turn has no prose |
| `forbidden_tool:<name>` | a tool outside the five-tool surface was used |
| `too_many_tool_calls:<n>` | thrashing, and over budget |
| `abs_path_leak:<frag>` | an absolute path survived into supervised tokens |
| `question_echoed_verbatim` | the `bundle` query is the question pasted back |

**Quality gates** (`grade.py`) — citation overlap with the reference answer:

| reason | default |
|---|---|
| `no_citations` | answer cites no file at all |
| `unresolvable_citation:<path>` | a cited path does not exist at the pinned commit |
| `few_tool_calls:<n>` | fewer than `min_tool_calls` = 2 |
| `low_file_recall:<x>` | below `min_file_recall` = 0.5 |
| `low_span_overlap:<x>` | below `min_span_hit_rate` = 0.34 |
| `ungradeable_reference` | the reference has no parseable citation (10 of 260) |

`file_recall` is the primary signal: of the files the reference cites, what
fraction does the answer also cite. It is robust, because a right answer has to
land on the right files, and file paths survive both refactoring noise and
citation-style differences. Path comparison is a **component-wise suffix match**
— the references are genuinely inconsistent about how much of a path they write
(`src/qibo/models/circuit.py` in one answer, `qibo/models/circuit.py` in another
for the same file), so string equality would reject correct citations.

`span_hit_rate` is secondary and carries the lower bar: of the reference's cited
line ranges, what fraction is covered by an overlapping range in the answer,
within `line_tolerance` = 20 lines. Line numbers drift with how much surrounding
context an author chose to include, so this is the sharper signal and the
noisier one. Requiring 0.34 means "at least a third of the reference's evidence
was actually located", not "the answers agree".

Thresholds are opening bids, to be recalibrated on the pilot's measured
distribution — that is one of the things the pilot is for. Rejects are written
out in full rather than deleted, so a threshold change is a re-run of `grade.py`
and not a re-run of the teacher.

`question_echoed_verbatim` earns a note, because it was found by the validator
rather than by design. A `bundle` query that is the question pasted back
demonstrates no query formulation — the single behaviour this corpus most exists
to teach — *and* it puts the user's own words inside a supervised tool call,
which the shared validator flags as the user turn leaking into the loss. Both
readings agree, so the trajectory is rejected and the teacher prompt explicitly
asks for an expansion instead. The gate uses the validator's own 80-character
prefix threshold so the two can never disagree about the same row.

### Decision 5 — path normalisation, designed in rather than bolted on

The SWE-QA-Pro trajectory conversion found that **92.8% of its supervised tool
calls** carried the generating harness's own repo root
(`repos_tmp/worker_646683/…`). A model trained on that learns to prefix every
path with a directory that will not exist at inference. It is the single most
expensive defect in that corpus and it was invisible until someone looked at the
rendered text.

So this harness normalises on the way in and then **asserts**:

1. Every snapshot root — not just this row's, because a `bash` command in one
   cell can name a sibling checkout — is rewritten out of every argument, every
   assistant message and every tool result.
2. Supervised strings are then re-scanned for anything absolute-looking, and a
   trajectory that still has one is **rejected, not repaired**. Repairing
   guesses; rejecting costs one trajectory.
3. Absolute fragments surviving in *masked* tool results are counted into
   `meta.masked_abs_fragments` rather than rejected. They are conditioning, not
   targets, and rejecting on them would throw away trajectories over text the
   model is never trained to produce — but a batch where that count climbs is
   telling you the normaliser has a blind spot.

The detector is deliberately broad — any rooted `/a/b/` path, any drive letter,
any `~/` — and is checked against both positive and negative controls
(`https://` must not read as drive `s:`; `and/or` and `src/qibo/` must not read
as rooted). False positives cost one trajectory each. A false negative is
permanent and lives in the weights.

### Decision 6 — the validator is referenced, never vendored

`grade.py` and `convert.py` hand their output to `~/lora-prep/validate_dataset.py`
(path configured under `[validate]`) — the same 33-check validator that proved
the masking on the SWE-QA-Pro conversion, whose own teeth are proven by a
negative-control script that corrupts data seven ways and confirms each is
caught. One copy means the two corpora cannot drift apart on what "valid" means.

It enforces the emission spec's hard rules directly: `arguments` is a JSON
string (never a dict — Arrow's `Dataset.from_list` unifies struct schemas and
null-fills, and the model learns to emit the nulls); `tools` byte-identical
across rows; `content` always a plain string; at least one supervised token per
row; and the full mask audit — tool calls and `<|im_end|>` supervised, system
prompt, tool schemas, user question, `<tool_response>` payloads and the
assistant header all masked.

Two rules it cannot check, which this harness owns instead:

- **Load with `load_dataset("json", data_files=…)`, never `Dataset.from_list`.**
  Even with string `arguments`, `from_list` corrupts the `tools` column — on the
  dry-run fixture it injects +157 junk tokens per row into the masked system
  block, where nothing raises and nothing shows in the loss. The validator
  prints this as an advisory on every run; it is not a defect in the file.
- **Never set `use_liger_kernel`** (trl#3781 silently disables masking).

### Decision 7 — drop, don't truncate; but do cap tool results

Two different things that are easy to confuse.

*Trajectories* are dropped whole when they exceed `max_length`. A truncated
trajectory teaches the scout to stop mid-exploration, and truncation that eats
the prefix silently drops **every** supervised token while raising nothing —
measured: a 933-token example encoded at `max_length=200` yields
`supervised=0, truncated=True` and no error.

*Tool results* are capped, at `max_tool_chars` (4000, with `bundle` allowed
12000). They are masked, so trimming them costs no training signal. It is not
free — it changes the context each supervised turn was actually produced from —
so the elision is explicit (`... [N characters elided] ...`), keeps both ends,
and is a configured knob rather than a constant. This is the lever that decides
whether trajectories fit an 8k budget: the SWE-QA-Pro corpus sits at a 19k
median almost entirely because of untrimmed tool output, and at the 4096 length
actually proven on this box exactly **one** of its 926 rows fits.

### Decision 8 — the teacher's tools are narrowed at the source

`generate.py` writes a per-cell `opencode.json` that denies everything the
student will never own: `edit`/`write`/`patch` (the snapshot must stay
byte-identical, or the pin is a lie), `webfetch`/`websearch`, `glob`/`list`,
`todowrite`, `external_directory`, the `task` subagent, and lore's own
`expand`/`status` — the MCP server exposes four tools, only two of which the
student has, and the `search` description actively tells its caller to expand a
hit before quoting it. What is left maps onto the five-tool surface: lore's
`bundle` and `search` over MCP, plus opencode's native `grep`, `read` and
`bash`.

A call the teacher never makes is a trajectory that never has to be thrown away.
`convert.py` still rejects on an unmapped tool, because a config that silently
stops applying should fail loudly rather than quietly emit off-surface calls.

Argument names are renamed to the student's surface at conversion:
`lore_bundle`/`lore_search` → `bundle`/`search`, `read.filePath/offset/limit` →
`read.path/start/end`, `grep.path`+`grep.include` → `grep.glob`. Every dropped
key is counted into `meta.dropped_arg_keys`, so a renaming that starts losing
information is visible in the data rather than only in this paragraph.

Two of those renames were wrong until the first pilot measured them, and both
were wrong in the quiet direction — the output stayed plausible:

- **`read.offset` is 1-based, not 0-based.** Measured at opencode 1.18.23:
  `offset: 302` returns a body whose first line is the file's line 302, and
  `offset: 1` returns line 1. The conversion added one, so every read span in
  every trajectory was shifted by a line, in a corpus whose entire purpose is
  teaching a model to cite line spans.
- **`grep` scopes with `path` as well as `include`.** Keeping only `include`
  collapsed two greps over different subtrees into two byte-identical
  supervised calls with different tool results. Both now fold into the one
  `glob` the student has.

---

## What is real and what is stubbed

**Real, exercised by `--dry-run`:** the event-stream parser, message
reconstruction including parallel tool calls, tool-name and argument mapping,
path normalisation and the leak gate (with positive and negative controls), tool
result elision, all structural gates, the citation parser (checked against all
260 real reference answers: 250 with files, 219 with spans), file-recall and
span-overlap scoring, manifest read/write, both validator invocations, and the
row shape — which passes all 33 validator check classes.

**Real, and executed once for real** (batch `pilot-01`, qibo, five questions,
2026-08-27): the `opencode` invocation and its per-cell tool gate, snapshot
checkout, and the lore daemon preflight. All five cells completed. What the
first contact changed is recorded above under Decisions 2 and 8 — the read
off-by-one, the lost grep scope, `lore_expand`/`lore_status` left un-denied, and
a preflight that read `embeddings.ready` (the daemon reports
`embeddings.state`) and accepted a 22%-embedded project as healthy.

**Real, and executed again at width** (batch `pilot-02`, qibo + pybryt, six
questions, concurrency 2, 2026-08-27): a second repository registered from
scratch and indexed, two cells in flight throughout, and the `over_length`
drop gate rejecting a real row. All six cells completed. See "What the second
pilot measured".

Still unexercised at any scale: concurrency above 2, batches beyond six cells,
and the 24 repos in the corpus that are neither qibo nor pybryt.

**Stubbed:**

- The dry-run fixture repository is fabricated. `citations_checked` is `false`
  for those rows because there is no tree to resolve paths against.
- `[lore] bundle_limit` / `bundle_budget_tokens` are read but not yet plumbed
  into the MCP call; the daemon's own defaults apply.
- No *cost* accounting. Token usage is collected — every cell's `meta.json`
  carries `tokens` summed over its `step_finish` events (`input`, `output`,
  `reasoning`, `cache_read`, and the `steps` that divide them), and the
  end-of-batch summary totals them — but opencode reports `cost: 0` for a
  subscription-quota model, so there is no money figure to record. Two things
  to know before reading those numbers. The fields **partition** the step's
  total rather than nesting — verified across all 40 steps of pilot-01,
  `total == input + output + reasoning + cache.read + cache.write` with no
  mismatches — so `input` is the *uncached* prompt only, the whole prompt is
  `input + cache_read`, and `reasoning` is charged beside `output`, not
  inside it. And every field is summed over steps, because a tool-calling
  session re-sends its transcript each step and that is what is metered.
- No deduplication pass. With one trajectory per question and 260 distinct
  questions there is nothing to dedupe yet; sampling several trajectories per
  question would change that.

**Not built, deliberately:** the training script. This directory produces a
dataset. Training it is a separate concern with a separate failure surface.

---

## Tests

```bash
python -m pytest train/tests -q      # ~3 s, no network, no GPU, no daemon
```

Python **3.11 or newer** (`common.py` uses `tomllib`) plus `pytest`. Nothing
else: every external service is fixtured, and the tests write only into pytest's
own tmpdir.

They were written as a separate pass, against README.md rather than against the
implementation, on the principle that a worker's own green tests only confirm
its own understanding. Where the two disagree the test asserts this document and
is marked `xfail` with the discrepancy named in its reason string, so the gate
stays green and the disagreement stays visible.

| file | seam |
|---|---|
| `test_paths.py` | Decision 5 — snapshot-root rewriting, the leak gate, and the detector's positive and negative controls (`https://` is not drive `s:`; `and/or` and `src/qibo/` are not rooted) |
| `test_grading.py` | Decision 4 — component-wise suffix path matching, the ±20 line tolerance at its boundaries, the 0.5 / 0.34 thresholds, and the ungradeable reference |
| `test_opencode_parse.py` | Decision 3 — reconstruction from the event stream: one call, parallel calls in one `messageID`, interleaved parts, a torn log, a `tool_use` with no `state.output`; plus every structural gate |
| `test_question_echo.py` | the 80-character echo threshold, at and either side of the boundary, checked against the validator rule it is pinned to |
| `test_row_structure.py` | the emitted row against the output gate — string `arguments`, byte-identical `tools`, string `content`, a byte-stable JSON round trip, and the mask |
| `test_manifest.py` | Decision 2 — what the pin records, what rides in `meta`, and each stage's refusal to run without its inputs |
| `test_output_gate.py` | Decision 6 — how the referenced validator is invoked (against a stub) and the read-only daemon preflight |
| `test_config.py` | the shipped example config and the documented default thresholds |
| `test_token_accounting.py` | per-cell teacher token capture from `step_finish` (shapes copied from a real captured session), and the port lease's invariant that no two cells in flight share a port |
| `test_dry_run_e2e.py` | `--dry-run` through all three stages in a subprocess, in a tmpdir |

`tests/emission_spec.py` replicates the checks of `~/lora-prep/validate_dataset.py`
that a unit test can own honestly: the schema and role grammar, string-only
`arguments` with no null-valued keys, and a character-level reproduction of the
render so the mask can be asserted — at least one supervised span, one per
assistant message, tool calls and `<|im_end|>` supervised, and the system
prompt, tool schemas, user question and every `<tool_response>` payload never
supervised. It is a second opinion, not a substitute: **token counts,
`max_length` and the Arrow round trip need the real tokenizer and `datasets`,
so those remain the referenced validator's job** and are checked only when a
real batch is converted.

### Residual risk the tests cannot reach

Everything under "Real but never yet executed" stays unverified here, by
construction: the `opencode` invocation and its per-cell tool gate, snapshot
checkout, the daemon preflight against `/v1/resolve` and `/v1/status`, and
whether `lore-mcp` really surfaces `bundle`/`search` under `LORE_PROJECT`. The
tests fixture the *shape* of an opencode event stream taken from the README's
description of it; if the real stream differs, they will not notice. The first
real cell is still the first time any of that runs.

---

## The pilot

One repository, five questions. Its job is to produce the numbers this design
is currently guessing at: keep rate, token length distribution, tool calls per
trajectory, and whether the grading thresholds are anywhere near right.

```bash
cd train
cp config.example.toml config.toml       # then set [lore].mcp_bin

# 1. Clone the snapshot. This exits telling you the exact `lore add` to run.
python3 generate.py --batch pilot-01 --repos qiboteam/qibo \
    --limit-per-repo 5 --prepare-only

# 2. Register and index it (the operator owns index state, not this harness).
lore add  work/snapshots/qiboteam__qibo
lore index

# 3. Capture the pin, then generate.
python3 generate.py --batch pilot-01 --repos qiboteam/qibo --limit-per-repo 5

# 4. Convert, grade, validate.
python3 convert.py --batch pilot-01
python3 grade.py   --batch pilot-01
```

Run it from WSL, with the lore daemon up and its embedding server ready —
`generate.py` refuses to start otherwise, because a batch generated while lore
is degraded to lexical-only is not the corpus it claims to be.

Expect roughly 3–5 minutes and 30–60k teacher tokens per cell at max effort,
so 15–25 minutes and 150–300k tokens for five cells at concurrency 1, plus a
one-time clone and index of qibo. The teacher runs through the existing
opencode/ChatGPT setup, so the cost is subscription quota and wall time rather
than metered API spend.

Check before scaling: the keep rate, the reject-reason histogram (a taxonomy
where one reason dominates is a harness bug, not a quality signal), and the
token-length distribution against `max_length`.

### What the first pilot measured (2026-08-27, qibo, five questions)

13m01s wall at concurrency 1 — 78s to 219s per cell, median 173s — and 28k to
65k teacher tokens per cell, which lands inside the estimate above. Every cell
called `bundle` exactly once and called it first; none reached for a tool
outside the surface. Tool calls per trajectory ranged 9 to 40, median 19.

The numbers the design was guessing at, and what they say:

- **`max_tool_calls = 30` is the wrong bid.** It was the *only* reject reason at
  the convert stage, and it took two of five — one of which then graded 1.00 /
  1.00 on the answer key. A trajectory-shaped filter that mostly rejects good
  trajectories is a threshold, not a gate. The teacher reads more files than
  this anticipated: 69 `read` calls across five cells.
- **The grading thresholds are, if anything, slack.** File recall came in at
  1.00, 1.00, 1.00, 0.50 and 0.43; span hit rate at 1.00, 1.00, 0.67. Nothing
  landed near the 0.5 / 0.34 bars from below except the one row that failed
  outright, so the pilot neither confirms nor refutes where they sit.
- **`max_length = 8192` is unreachable and `max_tool_chars` is why.** Rows
  rendered at 12.4k, 13.1k, 22.9k and 31.8k tokens — every one over, none within
  a factor of the bar, and only ~8.4% of each row supervised. Tool results are
  the whole mass. Either the budget rises to 32k, where all four fit but the
  largest fits by 3%, or the caps come down hard.
- **Decision 7's "drop, don't truncate" is not implemented.** Nothing in
  `convert.py` or `grade.py` drops a row for length; `max_length` is only
  handed to the validator, which fails the *batch* and leaves the over-length
  rows in the file. The gate has to exist before a real batch is trained on.

### What the second pilot measured (2026-08-27, qibo + pybryt, six questions)

Pilot-02's job was the axes pilot-01 could not reach: two repositories in one
batch, a repository registered from scratch, concurrency 2, and the
`over_length` drop gate in the wild. `microsoft/pybryt` was chosen as the
second repo for size (1.8 MB, 134 tracked files) — the axis under test is
whether a *new* repo works, not whether a big one does.

**9m02s wall for six cells at concurrency 2, all six OK.** Summed per-cell
wall was 1052.7s, so the batch ran at 1.94× — 97% of the ideal speedup for two
workers. That is the headline: on this box, at this width, the cells are
independent and the wall scales.

- **No interference of any kind was detectable.** Every cell's `agent.ndjson`
  carries exactly one `sessionID` and no `sessionID` appears in two cells;
  every `agent.stderr` is zero bytes; the daemon's `generation` held at 40
  across the whole batch, so nothing re-queued an index under concurrent
  query load. Teacher spend per cell went *down* against pilot-01's serial
  run (187k vs 219k mean), which is the opposite of what contention looks
  like.
- **Per-cell wall is dominated by teacher nondeterminism, not by the neighbour.**
  The same three qibo questions ran 108.9 / 218.9 / 173.1s serially in
  pilot-01 and 95.3 / 176.1 / 292.6s here. Two got faster. The one that got
  slower did 42% more work — 11 model steps against pilot-01's 9, 44 tool
  calls, 410k tokens — and it is the same cell the length gate then dropped.
  Reading that 292.6s as contention would be reading the wrong variable.

The gate and the caps, now that both have met real data:

- **The `over_length` gate fired, once, and cleanly.** Rows rendered at 10,816
  / 15,372 / 15,516 / 19,332 / 26,841 / **42,932** tokens; the last was dropped
  as `over_length:42932` against the effective budget of 32,704, and the five
  survivors each carry their `render_tokens` in `meta`. The validator then
  passed the file at 32,768 — which is the whole point of the gate, because
  before it existed that one row failed the *batch*.
- **The 64-token margin was never load-bearing here, and the counter and the
  validator agreed exactly.** The nearest survivor to the bar sits 5,863
  tokens under it, and the validator's reported quantiles (10,816 / 15,516 /
  26,841) reproduce the counter's numbers to the token — pilot-01's
  one-token disagreement did not recur. The margin remains cheap insurance
  rather than something the data has yet leaned on.
- **`max_tool_calls = 60` is now correctly slack, and that is what let the
  length gate speak.** Survivors used 8 to 22 calls, median 15. The dropped
  cell used 44 — over pilot-01's old bid of 30, under 60, so it was rejected
  for the thing actually wrong with it (length) rather than for a proxy.

Keep rates, per stage: **6/6 generated, 5/6 converted (the one `over_length`),
4/5 graded — 4/6 end to end, 67%.** `over_length` is now the single largest
sink, and it is a `max_tool_chars` question rather than a threshold question.

- **The new repo cost nothing in friction and little in quality.** `lore add`
  plus a scoped `lore index microsoft__pybryt` had it at 124 files / 479
  chunks / 100% embedded in under two minutes, with no `.loreignore` tuning
  needed. Keeper grades: qibo 1.00/1.00 and 1.00/1.00, pybryt 1.00/0.80 and
  1.00/0.75. File recall is identical at 1.00 across both repos; pybryt's
  span hit rates are slightly lower, which is two rows and not yet a trend.
  The one grade reject (`low_file_recall:0.33`) was pybryt's.
- **`lore index <project>` does scope, so pilot-01's warning has an answer.**
  Bare `lore index` queues everything; `lore index microsoft__pybryt` reported
  `queued 1 project(s)` and left the shared daemon's other projects alone.

Two things worth fixing that this pilot surfaced and did not:

- **`lore add` writes `.lore.toml` into the snapshot.** Both checkouts now
  carry an untracked file that is not in the pinned commit, so "the snapshot
  must stay byte-identical" (Decision 8) is already false before the teacher
  starts, and a `grep` or `bash` cell could see harness scaffolding. It is
  outside `train/`'s control — registration is the operator's job by
  Decision 2 — but the pin's claim should either be narrowed to tracked
  content or the file should live outside the tree.
- **`--dry-run` still cannot exercise concurrency.** The port lease is unit
  tested, but no dry path runs two fabricated cells at once, so the
  interaction of the pool with `--resume` is still only argued.

At these numbers, 260 cells at concurrency 2 projects to roughly **6.5 hours
and 49M teacher tokens**, yielding about 174 keepers at the measured 67%.
