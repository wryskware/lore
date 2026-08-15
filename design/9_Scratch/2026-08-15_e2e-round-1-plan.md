---
design_status: exploration
last_reviewed: 2026-08-15
decision_refs:
  - D-0009
---

# E2E round 1 — dogfood benchmark plan

First e2e runs at the M1 boundary. Scratch/process doc, not canon.

**Deviation from D-0009, noted deliberately:** D-0009's fixture corpora are
established OSS repos (a Python lib, a JS/TS lib, a C# project). Round 1
instead uses Wrysk's own three repos — they cover the same language spread
(C# + vault, TS + Python + prose docs, Rust + vault) and double as the
dogfood targets D-0004 already mandates. The OSS fixture round still matters
later (reproducibility, no vault-familiarity confound) but is not this round.
If this posture sticks, D-0009 should be amended by Wrysk, not silently.

## Matrix

| Axis | Values |
| --- | --- |
| Model | `gpt-5.6-luna` (high reasoning) · qwen3.8 local via ollama + opencode (high reasoning, 128k ctx) |
| Repo | Lexomancy (`~/Unity/Lexomancy`) · latent-music-terrarium (`~/wryskware/latent-music-terrarium`) · lore (this repo) |
| Retrieval | **on** (lore daemon + `lore-mcp` configured) · **off** (no lore tools; native grep/read only) |
| Tasks | T1–T5 below, instantiated per repo |

2 models × 3 repos × 2 arms × 5 tasks = 60 runs. If that's too heavy for a
first pass, cut to T1/T2/T4 (the most retrieval-sensitive) = 36 runs.

## Metrics (per run, per D-0009)

- Tool calls (count, and grep/read vs lore-tool split)
- Tokens (prompt + completion; luna free-tier measures capability only — no
  token-savings *cost* claim until paired with a cost-bearing model)
- Wall time
- Task success, graded against the answer key (below): 0 = wrong/failed,
  0.5 = partially right or right with wrong citations, 1 = correct and cited
- Compactions used (qwen arm) — see the 128k protocol below; fewer
  compactions with retrieval on is itself a success signal

## 128k context protocol (qwen)

- Tasks are sized so the *retrieval-on* arm fits comfortably; the
  *retrieval-off* arm is allowed to hit the window — that differential is
  part of the signal.
- Compaction is **permitted, never required**. Wrysk's observed recipe (one
  compaction after initial exploration when the window boundary is hit) keeps
  qwen capable — but the headline win we're hoping to see is the
  retrieval-on arm *not needing* compaction at all because retrieval keeps
  exploration out of the window. Record compactions per run; zero-compaction
  completions in the retrieval-on arm vs. forced compactions in the
  retrieval-off arm is a first-class result. A run that dies to context
  exhaustion scores 0 with cause recorded — that is a result, not a protocol
  failure.
- Never point a task at bulk data: Unity YAML/`Library`, `node_modules`,
  `dist*`, `analysis/tracks`, archives are out of scope for every task.

## Pinning and isolation

- Every run happens in a **fresh worktree of a pinned commit**, never the
  live checkout. Provisional pins (re-pin at answer-key freeze):
  - lore: `39da7c25c1`
  - latent-music-terrarium: `3b1eacd56f`
  - Lexomancy design vault (`design/` is its own repo): `d5e0d53310`
- **Lexomancy code is currently unversioned.** The outer `.git` is an empty
  directory and the inner Unity project's git history was archived to a
  tarball on 2026-07-24; only `design/` and `tools/` are real repos. Until
  Wrysk decides whether to re-init git there, runs use a **frozen snapshot
  copy** of `Lexomancy/Assets/Scripts` + the csproj files, one fresh copy
  per implementation run.

## Environment constraints

- **No Unity editor GUI.** Lexomancy tasks are read/trace/audit plus at most
  compile-and-test. Parent-verified: every csproj (including
  `EncounterKernel.csproj`, `Lexomancy.BattleKernel.Tests.csproj`) is
  Unity-generated (SDK-style, Unity analyzers), so plain `dotnet test` is
  unlikely to work; the realistic headless route is Unity batchmode via the
  `unity test` CLI (compiles + runs tests, no GUI). Prerequisite: confirm
  that command empirically at harness setup; if even batchmode is
  objectionable, T5 grading falls back to compile-only plus parent review.
- **No browser.** qwen/opencode has no Chrome access (and codex runs won't
  get it either, for parity). No task may require visual verification or a
  running web app; terrarium grading is headless tests + CLI output only.

## Run protocol

- Same task prompt verbatim for every cell; prompts name the repo root and
  the deliverable format, nothing else.
- Clean working tree per run; implementation tasks (T5) get graded on the
  diff, then reverted.
- Retrieval-on arm: daemon running, repo registered and index drained
  (`status` confirms) before the run starts. Retrieval-off: lore MCP server
  absent from config entirely, not merely unused.
- Order runs retrieval-off first per model/repo so index warm-up can't leak
  hints via daemon logs open in a terminal, etc. (paranoia is cheap here).
- Answer keys are established by Wrysk + Fable *before* any run; keys live
  in this doc's companion (`2026-08-15_e2e-round-1-answer-key.md`, not shown
  to models). First-pass keys came from haiku-tier scouts; a luna-high
  verify-or-refute re-scout of both non-lore repos runs before the freeze,
  and anything neither scout tier verified gets parent-checked directly.

## Task archetypes

- **T1 — Feature location / cross-file trace.** "Where does X happen; trace
  the path from A to B, naming files and symbols at each hop." Graded on
  hop-list overlap with the key. Retrieval should shine on the entry hop.
- **T2 — Authority / modality question.** "What is the *decided* position on
  X, and is document Y binding?" Targets the design vaults (Lexomancy, lore)
  and docs/ prose (terrarium). This is lore's differentiator: `design_status`
  + ledger awareness vs. an agent that reads a polished exploration doc as
  canon. Graded on citing the ledger entry / correct modality.
- **T3 — Recall sweep / impact analysis.** "List every place that consumes
  X." Enumerable key; graded on precision/recall. Punishes grep-only
  strategies when naming is inconsistent.
- **T4 — The "why" question.** "Why is Y built this way?" Answer lives in
  prose (decision rationale, handoff, review report), not code. Graded on
  reaching the right document and reproducing the actual rationale.
- **T5 — Bounded implementation.** A <100-line change whose difficulty is
  *finding the seam*, not writing the code. Graded on diff correctness and
  existing tests staying green.

## Per-repo instantiations

*(to be filled from scout reports — see answer-key companion for the graded
answers)*

### Lexomancy

Repo root `~/Unity/Lexomancy`; code under `Lexomancy/Assets/Scripts/`,
vault under `design/` (ledger: `design/0_Canon/DECISIONS.md`, D-0001–D-0016).
~33 KLoC C# across ~958 files — the scale case for the 128k arm.

- T1: Trace a played word from the board to battle impact, naming file +
  class at each hop. (Key: `Gameplay/WordplayScene/BoardCore/` →
  `Gameplay/WordplayScene/Battle/SurgeCounterScoring.cs` →
  `BattleKernel/Surge/SurgeState.cs` →
  `BattleKernel/BattleSimulator.cs::Step` →
  `EncounterKernel/Effects/Payloads/PayloadExecutor.cs` →
  `BattleKernel/BattleUnit.cs`.)
- T2: "What is the decided design for axiom capacity — do axioms cost Lexic
  Residue, and are there slots/tiers?" (Key, parent-verified: D-0006 —
  unlimited acquisition, fixed/unique, no capped-capacity machinery; D-0002 —
  "Lexic Residue does not exist … no residue currency anywhere in the
  design." `design/1_GameSystems/1.6_Forging/1.6.5_Axioms.md` has NO
  frontmatter, states residue costs (line 20) and slots/tiers (§3), and both
  ledger entries *explicitly* supersede those sections ("Supersedes:
  [[1.6.5_Axioms]] §2 … §3–4"). Full credit = citing the ledger supersession;
  citing 1.6.5 as authority fails. Bonus trap:
  `2.4_GuardianBattle_Surge.md` is `exploration`, partially superseded by
  D-0015/D-0016 via inline callouts.)
- T3: "D-0002 says Lexic Residue does not exist. Audit the codebase: list
  every place that still references residue." (Key, parent-verified, 7
  files: `State/PlayerStats.cs` (full `Spend/GainLexonicResidue` API),
  `State/RunState.cs:252`, `Loot/LootApplicator.cs:55–58,91–108`,
  `Loot/LootTypes.cs`, `Loot/LootTableSO.cs`,
  `Loot/LootRewardDefinitionSO.cs:64–73` (comment calls it "forge currency
  (future feature)" — direct canon contradiction),
  `UI/LootPanelController.cs:154`. Note: code says "Lexonic", the ledger
  says "Lexic" — the naming mismatch is part of the test; a single literal
  grep misses one side.)
- T4: "Why is the battle targeting heuristic RNG-free, and what is its
  ranked ladder?" (Key, parent-verified against D-0016: ranked on the hit
  that will actually land, post tier/counter scaling — kill-secure (lowest
  CurrentHP among killable) → max effective damage → lowest HP fraction →
  lane order; heals keep lowest-HP-ally; rationale: `enemies[0]` defaulted
  every attack onto the Lexomancer, and lanes were rejected as complexity.
  Implementation `BattleKernel/TargetHeuristic.cs`, tests
  `BattleKernel/Tests/AimAndTargetHeuristicTests.cs` — both verified to
  exist.)
- T5: Add one intermediate tiebreaker to `BattleKernel/TargetHeuristic.cs`
  (e.g. lowest shield percentage) between effective damage and HP fraction,
  RNG-free, no per-unit taunt state (D-0016 reserves that), with new cases
  in `BattleKernel/Tests/AimAndTargetHeuristicTests.cs`. (~80 lines.)

### latent-music-terrarium

- T1: Trace how a stem's audio activity becomes on-screen species
  brightness, naming file + function at each hop. (Key:
  `analysis/.../stages/stems.py::run` → `emitter.py::build` →
  `web/src/timeline/loader.ts::loadTimeline` →
  `sampler.ts::TimelineSampler.getChannel('stems')` →
  `mapping/stemfollow.ts::StemFollow.update`/`followMultiplier` →
  `sim/physarum/physarum.ts::uploadSpecies`.)
- T2: "Does the web runtime consume the raw 1024-dim `embedding.json`
  sidecar, and are `docs/handoff.md`'s 'non-negotiables' (reproducibility,
  analytical fidelity) still binding?" (Key: no — plan.md Revision 4 dropped
  runtime consumption in favor of the driver bank; the sidecar is still
  *generated* for future learned mappings. handoff.md predates plan.md
  Revision 1 and is historical; plan.md revised priorities 2026-08-06.)
- T3: List every place in `web/src` that reads timeline channel or event
  data after load. (Key: `mapping/modulation.ts::buildDriverBank`,
  `mapping/stemfollow.ts`, `sim/impulses.ts::ImpulseEngine.onTick`,
  `debug/overlay.ts`, `main.ts` glue; grade precision/recall.)
- T4: "Why does modulation use a seeded random projection over a ~16-driver
  bank instead of the raw embedding?" (Key: plan.md revision rationale —
  variance-ordered, z-scored PCA drivers + structure channels; raw embedding
  dropped from runtime.)
- T5: Graceful fallback for malformed track metadata: if `timeline.json`
  exists but manifest validation fails, construct a minimal valid manifest
  (infer duration, skip segments) instead of failing the load chain.
  (~40 lines; `web/src/timeline/catalog.ts` + `validate.ts`/`main.ts`;
  add an unhappy-path test alongside `web/tests/catalog.test.ts`.)

### lore

- T1: Trace a query from the MCP `search` tool entry to the ranked result
  list: name the crate/file/function at each hop (proxy → daemon HTTP →
  hybrid search → RRF merge → authority multiplier), and state where FTS and
  vector candidates are merged.
- T2: "Is the fixed 50-candidate pool per arm the decided design?" (The
  session-3 review attacked it; the fix wave changed acquisition; older
  scratch docs describe the pre-fix behavior.) Correct answer distinguishes
  ledger canon from scratch/exploration and cites current code.
- T3: List every code path that can trigger an index recompute, and every
  caller that consumes `design_status` from frontmatter.
- T4: "Why does registry reconciliation apply as one atomic set instead of
  per-row?" (Answer: the key-exchange convergence argument — commit
  60b3599 / package-3 design note / session reports.)
- T5: Fix the ATX trailing-`#` bug from the deferred backlog: `scan_headings`
  trims trailing `#` unconditionally, so `# Learning C#` loses its `#` in
  heading paths/anchors. CommonMark rules: a closing sequence only counts
  when preceded by a space. Fix + regression test. (Side benefit: a real
  backlog item gets fixed 12 times over; grade on diff quality, keep the
  best.)

## Prerequisites (order of operations)

1. Dogfood first: daemon + `lore-mcp` running against this repo, then
   Lexomancy vault, then terrarium — shake out registration/indexing
   friction before any measured run.
2. Verify opencode + ollama qwen3.8 wiring and the compaction workflow on a
   throwaway prompt.
3. Freeze task prompts + answer keys.
4. Run the matrix; log per-run metrics into a results scratch doc.
