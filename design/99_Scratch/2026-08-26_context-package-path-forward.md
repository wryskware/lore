# Context packages: path forward (draft plan)

*2026-08-26. Unclassified exploration — a proposal drafted by Fable at Wrysk's
request, informed by the FastContext/CodeScout benchmark series. Nothing here
is decided; nothing here creates canon. The stated goal, per Wrysk, is to
**reduce agent wall time first and cost second without reducing answer
quality**; token volume is one diagnostic rather than the objective itself.*

## What the benchmark series established

RCB benchmark series on the pinned agent-framework corpus, 2026-08-26. The
early rounds were first summarized stats-only; a Sonnet judge overlay was added
later (see below). Steering asymmetry disclosure: subagent arms ran under
imperative steering (wording stored per-row in `steering_text` in every round
file), which is stronger than the lore on-arm's steering.

| arm | wall/cell | frontier tok | subagent tok | reads | sub-calls |
|---|---|---|---|---|---|
| lore on (published r1) | 46.2 s | 138k | — | — | — |
| luna off (published r1) | 68.2 s | 292k | — | — | — |
| luna + CodeScout pointers (`luna-cs-1`) | 156.7 s | 259k | 224k | 12.2 | 4.0 |
| luna + CodeScout **packages** (`luna-cs-pkg-1`) | 89.6 s | **122k** | 143k | 7.0 | 1.9 |

1. **A pointer-returning search subagent is a net loss.** Perfect leads
   (402/402 locations real) still cost 2.3× wall vs plain Luna, because the
   frontier model re-reads everything and re-queries iteratively.
2. **Package return shape plus consumption discipline is the lever observed
   here.** Converting the same subagent locations into *mechanically verified
   evidence packages* (extracted `path:start-end` spans with the code, source
   pre-verification, verdict line, ≤4k budget) **and** changing the steering to
   once-only/no-reread cut frontier tokens 53% vs pointers and **58% vs plain
   Luna — below even lore-on's 138k** — while wall dropped to 89.6 s. This arm
   does not isolate format from discipline; both are part of the measured
   intervention.
3. **Residual wall gap is dominated by subagent latency.** ~1.9 calls × 20–30 s on the
   local 5090 (~5 s process cold-start + prefill-dominated loop; only ~680
   output tokens/call). A warm, optimized serving stack might reduce that to a
   few seconds per call; whether this is enough to land at or below plain
   Luna's wall is an explicit benchmark question, not an established result.
4. **Verification changes agent behavior.** Because spans were mechanically
   verified before injection, steering could say "cite without re-reading" and
   the agent (mostly) complied — that is where the read/token savings came
   from. FastContext's 56%-phantom output made the same trust impossible.
5. **The unanswerable task became cheap, but the current verdict did not
   semantically abstain.** It went from the most expensive cell (pointer
   round) to the cheapest (30.6 s), but its package verdict was `found`: the
   renderer had verified real retry code, and the main agent inferred that no
   failover policy was supported. The current verdict reports resolution/turn-
   cap state, not relevance or task coverage; semantic abstention remains open.
6. Caveats: one benchmark, one corpus, a later single-pass unreplicated quality
   overlay, imperative steering, greedy sampling. CodeScout is Python-trained;
   the corpus is Python+C# and the C# symbol heuristic over-counts (recorded
   per-location).

**Opus-medium pair (completed after the first draft; no prior Opus data
existed in RCB — round 1's Claude arm was `claude-sonnet-5 @ high`):**

| arm | wall/cell | recounted tok | turns | tool calls |
|---|---|---|---|---|
| opus-med off (`opus-med-off-1-fixed`) | 35.0 s | 225k (204k cache-read) | 9.4 | 8.4 |
| opus-med + packages (`opus-cs-pkg-1`) | 71.4 s | 152k (139k cache-read) | 6.0 | 5.0 |

Paired per-cell: tokens −73k mean, package wins 16/20; wall +36.5 s mean,
package loses 20/20.

**Opus-medium lore-on baseline (added later the same day,
`opus-med-on-1-fixed`): 40.5 s / 261k — lore-on does not help Opus-medium.**
It loses to off on both axes (wall wins 5/20, token wins 6/20): Opus made
only 1.45 lore calls/cell and then ran its native Grep/Read exploration
anyway, paying lore's overhead without displacing its own strategy — the
round-1 Sonnet "displaced a strategy rather than trimming one" pattern, but
with a net-negative cost outcome. Full within-Opus triangle: off 35.0 s/225k
· lore-on 40.5 s/261k · packages 71.4 s/152k. Disclosure: the on arm ran
round-1's neutral on-arm steering while the package arm ran the imperative
once-only steering, so part of the package arm's token win is *discipline*,
not format — which is itself evidence for Track A: retrieval + package
format + discipline at millisecond latency is exactly the untested quadrant
that could plausibly reach package-level tokens at off-level wall. The measured
package-plus-discipline intervention still cuts tokens (−32%) and nearly halves
turns/tool calls on a strong agent — but Opus-off is already so fast (35 s)
that ~2 local subagent calls at 20–30 s each double the wall. Opus also
satisfies "don't re-read" via bash greps (2.7/cell, tracked separately as
`bash_inspect_calls`) rather than the Read tool (0.3/cell). Conclusion
sharpened: **package-plus-discipline reduced frontier tokens for both tested
main agents; its wall value requires either millisecond Lore-only assembly or
a trained explorer fast enough to displace more main-agent work than it adds.**
This strengthens Track A and bounds the current local CodeScout-behind-the-
contract configuration to cheap/slow-main-agent use. Prefetch is not yet shown
to remove the dependent follow-up calls that dominated most package cells.
(Loose end: the pkg round's recounted `output_tokens` column reads
implausibly low (~48/cell vs off's 2380) — recount path for the new runner
needs a check; conclusion unaffected, totals are cache-read-dominated.)

## Quality overlay (judged 2026-08-26, sonnet-5 batch judge, single
unreplicated rounds — treat small deltas as noise)

| round | judge quality | pass rate | gold-claim recall |
|---|---|---|---|
| fc-sft-1 / fc-rl-1 / cs-1 (standalone models) | 0.10 / 0.08 / 0.09 | 0.00 | ~0.05 |
| luna-fc-1 / luna-cs-1 | 0.62 / 0.60 | 0.80 | 0.59 / 0.57 |
| **luna-cs-pkg-1** | **0.59** | 0.75 | 0.54 |
| opus-med-off-1 | **0.76** | 1.00 | 0.79 |
| opus-med-on-1 | 0.75 | 0.95 | 0.75 |
| **opus-cs-pkg-1** | **0.66** | 0.90 | 0.62 |

Three findings that sharpen the tracks:

1. **On Luna, the package contract's −58% tokens cost ≈nothing in quality**
   (0.60 → 0.59, inside noise). For cheap/fast agent lanes the contract is
   close to a free lunch on everything but local-latency wall.
2. **On Opus, the same contract cost real quality** (0.76 → 0.66, gold-claim
   recall 0.79 → 0.62): the trust-don't-reread discipline suppresses exactly
   the exploration that made Opus the quality leader. On strong agents the
   discipline needs an escape hatch — e.g. "explore freely when the verdict
   is `partial` or coverage feels thin" — or the package should be an
   *input* to exploration, not a substitute for it. Track A's lore-as-package
   design should treat this as a first-class requirement, not a tuning knob.
3. **lore-on on Opus is quality-neutral too** (0.75 vs 0.76) — with the cost
   numbers, the current lore-on integration is confirmed a no-op for strong
   agents on this benchmark: no token win, no wall win, no quality win.

(`file_recall` scored 0.00 across all nine rounds — almost certainly a
non-functional metric for these row formats rather than a real zero; do not
quote it either way until checked.)

## The reframe this suggests

The valuable artifact is not "a search subagent." It is a **context-package
contract**: *source-verified spans + explicit coverage state + budget,
delivered in one round-trip and trustworthy as source text*. Anything can sit
behind that contract — lore retrieval, a Lore-assisted trained explorer, grep,
or a cascade of all three. The benchmark series is strong evidence that this
contract is worth testing as Lore's product layer; it has not yet established
quality-non-inferior wall-time value on strong coding agents.

## Proposed tracks

Ordered by leverage-per-effort. A/B/C are lore feature work; D/E are
experiment lanes. Not a commitment to sequence — Wrysk picks.

### Track A — lore ships the package contract (highest leverage)

lore already finds relevant spans (search + expand). Proposal: a `context`
operation (MCP tool and/or CLI) that returns exactly the benchmarked format:
verdict, N verified spans with line-numbered code under a caller-set token
budget, "further reading" overflow paths. The extract/verify/render machinery
exists and is benchmarked (`bench/rcb/sandbox/cs_package.py` — needs a test
pass and a proper home if productized). Key design questions: how retrieval
confidence maps to `found/partial/none`; per-language span extraction (the
C# heuristic needs replacing with something honest); whether `expand` output
should simply adopt the package rendering. Measurable next: an RCB arm
"lore-as-package" — same steering discipline, lore instead of CodeScout —
directly comparable to `luna-cs-pkg-1` at near-zero marginal latency, since
lore retrieval is milliseconds, no 4B in the loop.

### Track B — speculative prefetch and exact-query caching

Fire context assembly the moment a session prompt arrives, concurrent with
the main agent's first thinking/prefill turn. By the time the agent asks, the
package may be warm. This requires harness/session integration: lore watches
project files, but the daemon does not receive the agent's session prompt.
The raw task prompt is also not necessarily the retrieval brief the main agent
would write, and 17/20 Luna package cells made a dependent second call despite
a first `found` package. A prompt-prefetched package can therefore hide only
speculative first-call latency, not assume an exact later tool-call cache hit.
Cross-session content-addressed query→package caching remains possible with
index-version invalidation, but its likely hit rate and value are unmeasured;
semantic reuse through source-validated cards is a separate Track E mechanism.

### Track C — context editing at the harness (compounds every turn)

The main agent resends its whole history every turn (~26k/turn for Luna).
Replacing exploration debris (raw tool outputs already distilled into the
package) with the compressed package cuts every subsequent turn's prefill.
Requires harness cooperation (opencode plugin / Claude Code hook), so it is
the least portable track — but it is the only one that scales savings with
conversation length rather than per-query.

### Track D — trained explorer, reframed as a Lore-assisted manifest planner (the funding-shaped lane)

The roll-our-own model idea survives, but its target output changes: not bare
locations (CodeScout), not prose (FastContext), and not model-copied source
text, but a structured **package manifest** that Lore verifies and renders.
Lore `search`/`expand` is in the agent loop both while training trajectories
and during execution: the model iterates against Lore's hybrid retrieval,
authority metadata and parser-backed chunks to select evidence, identify gaps
and map task facets to source handles. Terminal calls may remain a fallback.
Concretely: trajectories = (task/retrieval brief, lore-search/expand + optional
terminal calls, final manifest); reward = source-handle validity + relevance/
coverage against gold evidence + budget compliance + calibrated missing-facet
reporting + search/turn efficiency. Lore then deduplicates spans, checks source
generation/hashes and emits the exact package text. This may improve manifest
quality while reducing broad terminal exploration, but that is a comparison to
measure against the Lore-only Track A baseline.

CodeScout's released recipe (GSPO, localization-F1, training code, roughly
54.8k published rollouts) is useful precedent and an implementation reference,
not validation of Lore's different objective. SWE-QA-Pro/SWE-Smith trajectories
are format references. Qwen3.5-4B-Base per Sol's brief; the 2B sibling is
worth benching for the latency floor (CodeScout-1.7B is a separate Qwen3-based
baseline, not a Qwen3.5 sibling). Prerequisite before any training
spend: Track A's "lore-as-package" arm, to establish how much a trained
explorer could even add over retrieval + mechanical extraction.

### Track E — index-time exploration (amortize the model away)

Run an explorer continuously/nightly, writing findings into the index as
distilled cards (a proposed derived-card lane, not a currently shipped Lore
feature).
Query time then needs no model at all. Cheapest test: take the CodeScout
trajectories already on disk from cs-1/luna-cs rounds and evaluate whether
their exploration, replayed as cards, improves lore retrieval on the same
RCB tasks. Complementary to A–D, not competing.

## Agreed plan (Wrysk × Fable, 2026-08-26 — supersedes the sequence below-listed tracks implied)

Plain-language framing, per Wrysk's review of this doc and Sol's commentary:

**Phase 1 (approved, in build): `luna-lore-pkg`, Luna ONLY.** Same
once-only/trust consumption discipline as `luna-cs-pkg-1`, with lore
search/expand assembling the bundle instead of the 4B. What it tests: the
**consumption layer** — how an agent behaves when handed pre-verified
snippets — with lore's retrieval held fixed. What it does NOT test
(explicit, per Wrysk): lore's retrieval accuracy (an independent metric —
measurable separately, agent-free, against RCB gold evidence), and it is
NOT a gate for the trained-scouter lane (LLM retrieval and semantic search
are different mechanisms; success in one implies nothing about the other).
Phase-1 outcome is also expected to be sensitive to the agent's
instruction-following disposition — productization-level signal, not a core
lore performance metric. Success bar: wall ≤46 s AND main tokens ≤122k at
quality parity (glm-judged).

**Bundle-format improvements are internal lore-core work on their own
merits** (not contingent on the packaging thesis): mechanical integrity,
honest "couldn't find X" verdicts, rendering from parser-backed chunks
rather than regex heuristics.

**Decided dead:** translator (main agent writes queries — permanently),
bundle caching (seconds of generation never earns invalidation complexity),
multi-language/C#-repo test matrices, 2B latency-floor benching,
opus-as-target. Cheap-model performance is the niche and is taken
seriously as such.

**Phase-1 outcome + decisions (Wrysk, 2026-08-27).** Same-judge (glm)
luna table: off 68.2s/292k/0.73 · lore-MCP 46.2s/138k/0.75 · lore-bundles
50.4s (median 32.5)/110k (median 32k)/0.71. Median bundle cell is faster
and ~4× cheaper than lore-MCP; the mean is dragged by 3–4 cells where the
agent broke the trust discipline; quality −0.04 vs lore-MCP (iterative
search stays the quality leader). Decisions:
- **Bundles roll into standard lore as a NEW MCP tool alongside the
  existing search/expand surface** — both kept, for comparison and so
  hosts can switch over deliberately. Existing search API is preserved
  unchanged.
- **Tail iteration (steering/compliance tuning) is deferred**: it is
  model-specific productization tailoring; doing it honestly means
  multi-model averaging at real spend, which only makes sense once
  bundles-as-the-move is settled.
- **Scouter instruction shape (for the later trained model):** same
  contract Luna just ran — start from a lore bundle, then explore
  (additional bundle/search calls, and possibly grep/rg/read/bash) to fill
  what the bundle reports missing.

**Trained scouter (LoRA) lane: motivated intrinsically** — Wrysk wants to
build it for the learning and experience, independent of phase-1 results.
Shape per the Track D manifest-planner design (lore tools in the loop,
manifest of handles, lore renders). Prerequisites when started: pinned
index snapshot, trajectory generation/grading (RCB harness covers most),
reward choice. Not gated, not scheduled; starts when Wrysk says so.

## Loose ends carried from the benchmark series

- `cs_package.py` has no test pass (extractor had two real bugs during
  build; both fixed, neither would have been caught without a reviewer).
- C# `function_ok`/span heuristic over-counts call sites (`cs-paren`).
- `package` verdict branches `partial`/`none` never exercised live.
- Serving for 16-turn/broad-query CodeScout runs needs `MAX_LEN=65536`.
- A one-pass Sonnet judge overlay is complete. Replication or a stronger
  correctness gate remains useful for later product comparisons.

## Review commentary — 2026-08-26

> [!NOTE]
> This section is review commentary on an unclassified scratch proposal. It
> records recommendations and testable hypotheses, not accepted Lore decisions.
> It also incorporates Wrysk's clarification that the trained model is expected
> to use Lore search while training trajectories are generated and at runtime.

### Overall verdict

The context-package direction is worth pursuing, but the benchmark currently
supports a narrower claim than “the thesis won.” It shows that a source-verified
package plus strict consumption discipline can reduce main-agent context use.
It does not yet show a quality-noninferior wall-time improvement for the target
Opus workflow: the Opus package arm was slower and lost the one-pass judge
comparison on this QA workload. That is still a useful result because it gives
Lore a concrete baseline to beat and identifies package assembly—not package
rendering—as the present bottleneck.

The project objective should be treated as lexicographic:

1. Meet a quality non-regression floor appropriate to the task.
2. Among variants that pass, minimize end-to-end wall time (median and tail).
3. Among similarly fast variants, minimize actual cost.

Quality is therefore a gate, not the last-place metric. Raw token volume is a
diagnostic rather than cost: cache-read, cache-write, input, output, local GPU
time and API charges need separate accounting. This framing matches the stated
preference for time over cost while preventing a fast but worse system from
winning.

### Package contract: useful, but its guarantees need splitting

The current package builder proves that at least one cited path/span can be
resolved and rendered. It does not prove that the evidence is relevant,
sufficient, or covers every facet of the task. The unanswerable item returning
`found` is the clean counterexample. The contract should expose two independent
dimensions:

- **integrity:** source handle exists, requested span/chunk resolves, generation
  or hash is current, and rendered text came from the authoritative source;
- **coverage:** each requested task facet is supported, missing, ambiguous, or
  deliberately omitted under the budget.

Lore can guarantee integrity mechanically. Coverage is a planner judgment and
must remain explicit and calibrated. A package should be able to say “all
handles valid, one facet still missing” instead of collapsing both facts into
`found` or `partial`. The production implementation should render Lore's
parser-backed chunks/expansions rather than port the benchmark's regex span
heuristics.

The benchmark also changed two things together: return shape and main-agent
consumption policy. The once-only/no-reread discipline is plausibly responsible
for part of the gain. Keep that combined policy for the product probe, but do
not claim that serialization alone caused the result until an ablation warrants
it.

### Query formulation: test a skill before adding a translator model

Track B should not assume that the user/session prompt is already a retrieval
query. A coding request mixes desired outcome, constraints, history and often
irrelevant context; the useful scout request is a narrower retrieval brief. In
the benchmark the main agent wrote that brief, and it was often more specific
than the original question.

The cheapest clean experiment is an offline paired replay through the exact
same Lore package path:

- raw benchmark task as the query;
- the recorded first CodeScout/retrieval brief;
- optionally, a brief produced under a small fixed main-agent instruction or
  skill.

Score evidence recall/precision, facet coverage, package size and retrieval
latency. If the recorded briefs materially win, first expose a tool schema such
as `task` plus optional `retrieval_focus`, with concise instructions teaching
the main agent what belongs in the focus. Only add a separate translator model
if that policy remains inconsistent and the measured retrieval gain pays for
another inference. The eventual trained explorer can internalize task-to-search
translation, so a dedicated translator risks becoming temporary architecture.

### Prefetch and exact-query caching: defer them

The prefetch proposal does not presently have a reliable trigger. The daemon
does not automatically see the agent's next tool query, and the raw task is not
necessarily the query the agent will formulate. More importantly, 17 of 20
Luna package cells issued a dependent second CodeScout call even after the first
package reported `found`; speculative first-query work would not eliminate that
dependency chain.

There are three different ideas here and they should not be conflated:

- keeping the Lore daemon, index, model weights and common data pages warm;
- memoizing an identical or canonicalized search within a session;
- persisting source-validated findings as semantically reusable derived cards.

The first is ordinary latency engineering and is worth doing. The second needs
trace evidence of real hit rates before product work. The third is Track E and
is promising even if exact queries never repeat. Query-specific prefetch should
stay out of the first causal test because it adds timing and hit-rate variables
without addressing dependent follow-up searches.

### Track D: a Lore-assisted search policy and manifest planner

The user clarification materially improves the trained-model proposal. The
model should not receive a task once and hallucinate a package manifest from
its weights. It should learn a bounded search policy over Lore and return a
manifest of source handles that Lore verifies and renders:

```text
task + optional retrieval focus
    → trained planner
    → batched Lore search / expand (repeat only when a facet is missing)
    → structured manifest with per-facet coverage
    → Lore integrity checks, deduplication and rendering
    → context package
```

During supervised-data generation and RL/on-policy rollouts, the model should
have tool access to a pinned Lore index snapshot. Supervised optimizer steps can
train on the recorded tool transcripts; they do not need a live daemon call for
every minibatch. At execution, the model again uses the actual Lore tools. Pinning
the source/index generation in each trajectory is important for reproducibility
and for distinguishing policy mistakes from index drift.

A minimal manifest should contain:

- task facets and their status (`supported`, `missing`, or `ambiguous`);
- selected chunk/source handles and requested expansion bounds;
- priority/order and a package budget;
- a stop reason and any recommended follow-up query.

It should not copy source text. Lore remains the sole authoritative owner of
index state and source rendering, checks handle generations/hashes, and emits
the exact package. That preserves D-0003 while making the learned component
replaceable.

This loop could improve quality because the planner can inspect actual search
results, notice missing facets and reformulate. It could improve time because
Lore search/expand should be cheaper than broad terminal exploration and the
model no longer spends tokens reproducing source. Neither benefit is automatic:
several sequential local-model turns can still dominate millisecond retrieval.
Given the time-first objective, start with batched parallel searches and a hard
one- or two-refinement budget, then measure planner inference, Lore calls,
rendering and main-agent work separately.

Mechanical handle validity is not an adequate training reward. Reward/evaluate
at least:

- gold-evidence recall and irrelevant-evidence penalty by task facet;
- calibrated `missing` behavior on unanswerable facets;
- valid, current handles and budget compliance;
- search turns, model forward passes and end-to-end latency;
- downstream main-agent correctness/non-regression.

The downstream metric must remain decisive; otherwise the model can learn to
produce small, perfectly valid, semantically incomplete manifests.

### Adapter-first model path

“LoRA first, then full-parameter fine-tuning if an adapter ceiling is measured”
is the sensible cost-of-learning order. LoRA is already a form of fine-tuning;
the later distinction is adapter training versus updating the full checkpoint.
Use 4B as the primary capacity point, benchmark 2B as the latency floor, and
bring in 9B as a quality challenger only if 4B misses the coverage gate. Compare
Base and Instruct initialization on a small controlled set rather than assuming
CodeScout's choice transfers to a tool-using manifest objective.

The official Qwen3.5 Base cards explicitly position their control-token setup
for LoRA-style parameter-efficient tuning: [2B](https://huggingface.co/Qwen/Qwen3.5-2B-Base),
[4B](https://huggingface.co/Qwen/Qwen3.5-4B-Base), and
[9B](https://huggingface.co/Qwen/Qwen3.5-9B-Base). CodeScout is useful evidence
that a small specialized explorer can be trained, but not an exact recipe for
Lore: [CodeScout-4B](https://huggingface.co/OpenHands/CodeScout-4B) starts from
Qwen3-4B-Instruct-2507, uses full GSPO training, targets location F1, reports a
Python-only limitation, and was trained on eight H100s. Lore changes the base
family, tool interface, language/domain distribution, output schema, objective
and deployment hardware at once.

Training data should therefore be Lore-native trajectories: strong-teacher or
successful-agent retrieval briefs, Lore search/expand observations, optional
fallback terminal calls, the final manifest, and downstream outcome. Split by
repository and task family—not just by question—to prevent memorized file maps
from looking like retrieval skill. RCB-W and real coding/write tasks are
important because the current RCB series mostly measures repository QA, while
the product claim concerns agentic coding.

### Derived cards: retain results, do not call that prefetch

Keeping good manifests/findings and adding them to the index is the strongest
part of Track E. Treat these as derived, lower-authority artifacts rather than
source truth. Store the originating task/retrieval brief, source handles and
generations/hashes, planner/config version, creation time, validation state and
observed downstream outcome. On reuse, Lore should rehydrate from current
source and invalidate or rebuild a card when cited sources change.

The first evaluation must not replay a trajectory's card onto the same RCB task
and call the result generalization; that only tests storage and lookup mechanics.
Use held-out related tasks, later edits to the same area, or cross-task reuse
within a repository. A background/nightly explorer that invents topics to
prefetch is a separate and much less grounded proposal. Query-time manifests
can seed the card lane without needing it.

### Recommended decision gates

Before funding training, the Lore-only package arm should establish the lower
bound on assembly latency and package quality. The next evaluation should then
compare, on identical tasks and index snapshots:

1. main agent with native repository tools;
2. raw task → Lore package;
3. main-agent retrieval brief → Lore package;
4. retrieval brief → Lore-assisted trained planner → Lore package.

Use repeated paired runs and report correctness/claim recall, median and p95
wall time, time to first useful evidence, actual priced API usage, local GPU
occupancy, package precision/coverage and downstream tool calls. Run both the
existing QA set and coding/write tasks on C#/Unity-heavy repositories. Track D
earns a place only if its extra inference reduces more downstream time than it
adds while meeting the same quality gate. Full-parameter training earns a place
only after the LoRA result shows a capacity/optimization ceiling rather than a
data, reward, index or tool-interface problem.
