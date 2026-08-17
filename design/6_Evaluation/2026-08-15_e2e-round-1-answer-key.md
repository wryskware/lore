---
design_status: exploration
last_reviewed: 2026-08-15
---

# E2E round 1 — frozen prompts and answer keys

> **Revision A (2026-08-17):** grading criteria are amended by
> [[2026-08-17_e2e-round-1-key-addendum]], which resolves the `key_gaps` the
> blind graders logged. Read it alongside this doc and the plan doc; where they
> disagree, the addendum wins for round 2 and later. Nothing here is rewritten
> — the text below is what round 1 was actually graded against, and the
> **prompts are unchanged and stay unchanged**.

Companion to [[2026-08-15_e2e-round-1-plan]]. **Never shown to benchmark
models.** Prompts are verbatim and hint-free (Wrysk lazy style); grading
keys live in the plan doc's per-repo task bullets — this doc is the runner's
copy of what to paste and what to grade against.

## Pins (frozen 2026-08-15)

| Repo | Pin |
| --- | --- |
| lore | git `977364a` (re-pin if more lands before runs) |
| latent-music-terrarium | git `3b1eacd56f` |
| Lexomancy code | cm `cs:134` (`/main@Lexomancy/Lexomancy@lexomancy@unity`) |
| Lexomancy design vault | git `d5e0d53310` |
| Lexomancy tools | git `35a45a26ad` |

Working trees: lore + terrarium via `git worktree` from the pin;
Lexomancy via `C:\Users\perag\Unity\Lexomancy-bench` (junctions →
`Lexomancy-alt` at cs:134, `design`, `tools`). **Retrieval arm for Lexomancy
queries `project=Lexomancy`** (the main root already indexes code + vault +
tools at cs:134; project-relative result paths resolve identically inside
the bench workspace). Keep the main Lexomancy workspace untouched while
runs are in flight.

## Runner

Both models run through opencode (OpenAI sub for luna, ollama for qwen):

```
opencode run -m openai/gpt-5.6-luna       "<prompt>"
opencode run -m "ollama/qwen3.8:latest"   "<prompt>"
```

Harness: `bench/run.ps1` (see `bench/README.md`). Arms select
`bench/opencode-{on,off}.jsonc` via `OPENCODE_CONFIG`; configs *merge* with
the global one, so the off arm explicitly disables lore (verified — a
merely-omitted block leaks the global lore config into the off arm).
Reasoning: `--variant high` for luna; qwen runs at model default. Metrics
parsed from the `--format json` event stream + opencode's session table
(`time_compacting` covers the qwen compaction protocol). Retrieval targets:
`lore-bench` / `terrarium-bench` (registered pinned worktrees under
`C:\Users\perag\bench-e2e\`) and `Lexomancy` (main root; see plan).
Pilots 2026-08-15: luna/lore/on/T4 correct (26 s, 23.9k in, 3 lore calls);
qwen/lore/off/T4 correct (29 s, 61.5k in, 0 lore calls).

## Prompts (verbatim — do not editorialize when pasting)

### Lexomancy (cwd: Lexomancy-bench)

- **T1:** how does a surge cast i submit actually end up damaging an enemy?
  walk me through the code path, files and classes at each step
- **T2:** do axioms cost lexic residue? is there a slot limit or tiers?
- **T3:** i thought we removed residue from the design. is it still
  referenced in the code anywhere? list everything you find
- **T4:** why is battle targeting rng-free? whats the pick order
- **T5:** add a tiebreaker to enemy targeting: prefer lowest shield
  percentage, between effective damage and hp fraction. keep it
  deterministic and add tests

### latent-music-terrarium

- **T1:** how does stem loudness end up changing how bright a species is on
  screen? trace it from the python side all the way through
- **T2:** does the web app still use embedding.json? and is docs/handoff.md
  still an accurate picture of what we're doing
- **T3:** list every place in web/src that reads timeline channel or event
  data after load
- **T4:** why did we go with the random projection driver bank thing instead
  of just using the raw embedding
- **T5:** the server's track content version only hashes the timeline files
  but we serve audio.wav too. include the wav in the version and update the
  test

### lore

- **T1:** walk me through what happens when an mcp search call comes in,
  from the proxy to the ranked results. files and functions at each hop
- **T2:** is the fixed 50-candidate pool per search arm still how it works?
  is that the decided design
- **T3:** what are all the ways an index pass can get triggered? list every
  code path
- **T4:** why does registry reconciliation apply the whole project set
  atomically instead of row by row
- **T5:** headings like "# Learning C#" lose the trailing # in heading
  paths. fix that per commonmark rules and add a test

## Grading

Per plan: 0 / 0.5 / 1 against the plan doc's parent- or luna-verified keys.
Record per run: tool calls (grep/read vs lore split), tokens, wall time,
compactions (qwen), score. T5 additionally: suite green (terrarium: `cd
analysis; uv run --extra dev --extra server pytest -q`; lore: `cargo test
--workspace`; Lexomancy: EditMode run by the grader against an open editor
— the agent never touches Unity).
