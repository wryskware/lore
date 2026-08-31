# What we've measured

Lore's claims are supposed to trace to runs, not vibes. This page tells the
story of the evaluation program so far — what each lane asks, what it found,
and what it honestly does not show — with links to the authoritative reports.
The reports live in [`design/6_Evaluation/`](../design/6_Evaluation/) and the
harnesses in [`bench/`](../bench/); everything here is a summary of those, and
where they disagree, they win.

## How to read these numbers

A few disciplines apply across every lane, and they matter more than any
single result:

- **Everything ran on one development machine, at small n.** Most rounds are
  a single run; where repeats exist the reports say so. Treat every number as
  an estimate with error bars nobody has computed.
- **The retrieval arm is steered, and that is disclosed.** An unsteered pilot
  showed agents never reach for an MCP server they were not told about, which
  would have measured nothing. The steering text is recorded per run.
- **There is a measured noise floor.** Running one answerer's no-retrieval arm
  twice under identical conditions moved the round mean by 0.025 on
  required-claim recall. Effects smaller than that are not evidence, and
  several results below sit inside it.
- **Numbers are not comparable across harnesses, judges, or rounds** unless a
  report says they are. Token counting differs per CLI, judge models were
  switched mid-program, and task sets are frozen within a round, not across
  rounds. The reports flag every boundary.

## The lanes at a glance

| lane | question | headline |
| --- | --- | --- |
| [End-to-end bench](#end-to-end-does-an-agent-do-better-work) | does an agent with Lore do better work on our repos? | round 1: no task scored worse with Lore on; tokens −17%, wall −24% on the frontier arm |
| [Retrieval bench](#retrieval-quality-in-isolation) | are the search results themselves any good? | recall + judged precision machinery; picked the embedding stack |
| [RepoContextBench](#someone-elses-benchmark-repocontextbench) | does it hold up on a benchmark we didn't write? | wins scale inversely with the agent's own strength — sonnet cost halved, opus a no-op |
| [RCB-W](#write-tasks-rcb-w) | does it help *fix* bugs, not just answer questions? | cost premium collapsed +79%→+23%; fix counts suggestive, not demonstrated |
| [The bundle](#the-bundle-a-contract-measured-before-it-shipped) | what should retrieval hand an agent? | evidence bundles cut frontier tokens −58% (luna) / −32% (opus); shipped as D-0025 |
| [The scout](#the-scout-can-a-trained-model-beat-the-assembler) | can a small trained model out-retrieve the mechanical assembler? | 2× recall over its base; better citations than the assembler renders; no value yet as a query-time tool |

---

## End-to-end: does an agent do better work?

The first question was the bluntest: same coding tasks, same model, retrieval
on and off, graded blind against pre-written answer keys. The corpora were
this repo, a TypeScript/WebGPU project, and a large C#/Unity game — the repos
Lore actually exists for (per [D-0011](../design/0_Canon/DECISIONS.md)).

**Round 1** ([report](../design/6_Evaluation/2026-08-16_e2e-round-1-report.md))
set the tone for everything after it: **−17% input tokens and −24% wall time**
on the frontier-model arm, and **no task anywhere scored worse with Lore on**.
The less flattering parts are in the same report: a small repository the agent
could simply read end-to-end got *more* expensive with retrieval on, and a
small local model scored exact parity between arms. What the round ruled out
is that Lore makes an agent worse — its cost is tokens, not correctness.

**Round 2** ([report](../design/6_Evaluation/2026-08-17_e2e-round-2-report.md))
scored a perfect 8.0/8.0 on *both* arms — which is a finding about task
difficulty, not a triumph: cells that both arms ace measure nothing, and the
round-2 protocol now pilots for exactly that failure.

**Rounds 3–5** are one arc on the large Unity corpus, and they taught the
program its most important lesson. Round 3
([report](../design/6_Evaluation/2026-08-17_e2e-round-3-lexomancy.md)) produced
the first retrieval-*loses* result — off 4.0, on 2.5 — on a mid-strength
model. Round 4 ([report](../design/6_Evaluation/2026-08-17_e2e-round-4-lunamax.md))
re-ran the identical cells with only the model's effort raised, and the loss
vanished: 4.0/4.0, with the retrieval arm spending **27% fewer input tokens**.
Round 5 ([report](../design/6_Evaluation/2026-08-18_e2e-round-5-qwen27b.md))
added a 27B open-weight model: 4.5 off vs 4.0 on, tokens −37%. The lesson:
comparing two failing arms is comparing garbage to garbage, and an agent too
weak to use retrieval well can be actively confused by it.

## Retrieval quality, in isolation

Agent rounds are expensive and noisy, so [`bench/retrieval/`](../bench/retrieval/)
measures the search results themselves, two ways that must not be confused:
**recall** against hand-verified answer keys (did the known-correct file come
back, at what rank?) and **precision** against a judged result pool (of what
came back, how much was worth reading?). The machinery began life as an
embedding-model comparison — it is what selected the default embedding stack
([D-0014](../design/0_Canon/DECISIONS.md)) — and runs fully isolated from the
live daemon. Metric definitions and rationale:
[relevance bench proposal](../design/6_Evaluation/2026-08-17_relevance-bench-proposal.md).

## Someone else's benchmark: RepoContextBench

Our own tasks can flatter us, so the program's center of gravity moved to
[CodeAlive's RepoContextBench](https://github.com/CodeAlive-AI/repo-context-bench):
20 tasks over `microsoft/agent-framework`, a task set we did not write, scored
by a rubric we did not design. Our scorer is a port of theirs, **validated
exact against 41 of their published runs and 820 task scores with zero
mismatches**. Runs execute in a network-jailed sandbox with web tools removed
at the tool layer — the gold answers are public on GitHub, so containment is
verified per run, not assumed.

Every task is answered twice by the same model — lore reachable over MCP
(`on`) and not (`off`) — so each task is its own control. The consolidated
record of every run is the
[master results ledger](../bench/rcb/RESULTS.md); the finding that organizes
all of it:

**Lore's effect scales inversely with the main agent's own search strength.**

| answerer | with lore on |
| --- | --- |
| claude-sonnet-5 @ high | wall and cost roughly **halved** (~$4 saved per 20-task round) |
| gpt-5.6-luna @ high | wall −30%, tokens −40..−70%, across four independent repeats |
| qwen3.8-27b (hosted) | the only run where cost **and** judged quality improved together (−35% cost, pass 0.80→0.90) |
| gemini-3.7-flash | mild, mixed win |
| claude-opus-5 | **a no-op to slight regression** — it makes ~1.5 lore calls, then re-explores natively |

The strongest agents already know how to search; what they need is not
pointers but something better than pointers — which is what the next two lanes
went looking for. Full methodology in the
[bench README](../bench/rcb/README.md) and the
[round-1 report](../design/6_Evaluation/2026-08-19_repo-context-bench-round-1-report.md).

## Write tasks: RCB-W

Answering questions is not fixing bugs, so
[`bench/rcb/rcbw/`](../bench/rcb/rcbw/) poses five real bugs from the same
corpus — each one fixed upstream *after* the model cutoff, so the golden fix
is memorization-free. Grading is deterministic first: a regression test
authored in a separate pass (fails at the pin, passes with the golden fix),
plus the collateral suites of every touched package.

Two rounds so far
([round 1](../design/6_Evaluation/2026-08-19_rcb-w-round-1-report.md),
[round 2](../design/6_Evaluation/2026-08-20_rcb-w-round-2-report.md)). Round 1
found retrieval-on carrying a **+79% cost premium** for the same 3/5 fix rate.
A search-payload recalibration (inline excerpts for the top hits only, pointer
headers for the tail) collapsed that to **+23%** in round 2, with fixes at
off 3/5, on 4/5. The honest reading is in the report: the one on-arm-only fix
involved zero lore calls, so it is trajectory variance, not attributable
retrieval value. Write-side value is suggestive, not demonstrated — these are
single runs, and repeats are a pending decision.

## The bundle: a contract measured before it shipped

The RCB finding — strong agents don't want pointers — turned into a series of
contract experiments, all in the [master ledger](../bench/rcb/RESULTS.md):
what if retrieval returned a *finished evidence package* (verified,
line-numbered spans with a verdict) instead of ranked hits?

Prototyped first with a local 4B localizer model assembling the packages, the
contract cut main-agent tokens **−58% on luna and −32% on opus** against
iterative search. It also surfaced a real cost: on opus, the
"trust-the-package, don't re-read" discipline suppressed the exploration that
made it the quality leader (judged quality 0.76 → 0.66). Strong agents need
an escape hatch — the package as input, not substitute.

The mechanical version of that contract shipped as the `bundle` tool
([D-0025](../design/0_Canon/DECISIONS.md),
[design](../design/4_Interfaces/2026-08-27_bundle-mcp-tool.md)): daemon-side
assembly, spans rendered from the files on disk at answer time, and a verdict
computed from **query-term coverage rather than retrieval score** — because a
measured nonsense query outscored a real query's #2 hit under rank fusion, so
any score threshold manufactures confident "found" on empty results.
Benchmarked before merging: the assembled-bundle arm ran the median RCB cell
faster and cheaper than the MCP search loop with quality within noise, and
every span in every bundle verified — the tail was agent compliance, not
assembler error. Symbol-following shipped **default-off** because its
pre-registered improvement bar was stated before measurement and not met;
that pre-commitment is honored until a consumption-side result earns the
default.

Underneath the agent rounds sits an agent-free check
([`retrieval_eval.py`](../bench/rcb/retrieval_eval.py)): score `/v1/search`
and the bundle assembler directly against gold evidence spans — deterministic
and free. Its second corpus, [SWE-Atlas](../bench/rcb/atlas/) (47 tasks over
11 repositories we did not choose), exists because the first corpus was small,
single-repo, and written by us.

## The scout: can a trained model beat the assembler?

The most recent lane asks whether a small fine-tuned local model — a
**scout** trained to call `bundle` first, keep exploring, and answer with
verified `path:line` citations — can out-retrieve the mechanical assembler.
The training harness is [`train/`](../train/README.md): a teacher agent
answers repository questions inside commit-pinned snapshots with lore wired
in, and its recorded trajectories are converted, path-normalized, graded
against human-validated reference answers, and gated hard (the README
documents every gate and why it exists). The training questions
(SWE-QA-Pro-Bench) and the eval corpus (SWE-Atlas) are strictly disjoint,
and that contamination boundary is the harness's first stated constraint.

Three evals close the loop, all on the held-out Atlas tasks (mean file recall
/ span hit rate against gold evidence, ±20-line tolerance; raw rows in
`train/work/eval/`, untracked — recorded here 2026-08-31):

| configuration | file recall | span hit |
| --- | --- | --- |
| base model, same scaffold | 0.188 | 0.104 |
| scout v1 (flawed corpus) | 0.285 | 0.207 |
| scout v2 (196 clean rows) | **0.372** | **0.235** |
| `bundle` alone, rendered spans (12k budget) | 0.236 | 0.130 |
| `bundle` alone, + further-reading refs (~43 locations named) | 0.482 | 0.343 |

Three honest findings:

1. **The training works.** The v2 scout roughly doubles its base model's
   recall, and the fine-tune survived a real defect: the v1 corpus was
   generated with every `bundle` call erroring (an environment-variable bug in
   the harness, since fixed, with erroring lore calls now rejecting the row)
   and still improved — v2, generated clean, improved again.
2. **As a citation-maker, the scout beats what the assembler renders** (0.372
   vs 0.236 file recall) while citing a handful of spans instead of sixteen.
   The assembler only pulls ahead when allowed to *name* ~43 locations without
   rendering them — far more breadth, far less precision.
3. **As a query-time tool it has not earned its place.** Two runs per arm
   exist, and in both the ordering is the same: a host agent simply calling
   `bundle` first and exploring itself wins (0.784/0.675 in the final run),
   and giving that host a `scout` tool instead never beats it (0.710/0.612) —
   in the final run the scout arm was indistinguishable from the same host
   with *no retrieval at all* (0.715/0.616). The current leaning (not a
   decided position) is that the scout's value is offline and assembler-side —
   building better bundles — rather than as a subagent in the query path.

## Where this leaves us

What we currently believe, on the evidence:

- **Lore pays off where the agent is weakest, the repo is largest, or the
  answer lives in prose.** Weak-to-mid models get large token, wall, and
  sometimes quality wins; the largest corpus produced the largest savings.
- **The strongest explorers are a no-op.** Opus-class agents re-derive
  everything natively; giving them ranked pointers changes little. The bundle
  contract is the measured response to that, and its consumption-side numbers
  are still accumulating.
- **Nothing has measured Lore making an agent's answers worse** outside one
  weak-model round that vanished when the model got stronger. Its cost is
  tokens on repos small enough to read whole.
- **Write-side value and the scout lane are open questions**, and the honest
  results above are the current state, not the hoped-for one.

And what the program still owes: repeats where single runs stand, a second
machine, consumption-side bundle rounds, and Linux.
