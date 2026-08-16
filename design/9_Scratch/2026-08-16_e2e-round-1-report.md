# E2E Round 1 — Full Report (machinery metrics; grading pending)

**Round 1 of D-0009's e2e benchmark is complete: 60 canonical cells** — 2
models (luna = openai/gpt-5.6-luna high, qwen = ollama qwen3.8 128k) × 3
repos (lore-bench, terrarium-bench, Lexomancy) × 2 arms (lore MCP on/off) ×
5 tasks. Luna ran 2026-08-15, qwen 2026-08-16. Protocol authority:
`2026-08-15_e2e-round-1-{plan,answer-key}.md`. Quality **scores are not in
this report** — grading against the answer key is Wrysk's pass; every
`metrics.json` has `score: null` until then. Everything below is what the
harness measures: wall, tokens, tool calls, lore adoption.

## Headline (pending grading)

- **Luna + lore: cheaper and faster overall** (−14% input tokens, −15%
  wall), with the entire win concentrated in Lexomancy — the one repo too
  big to grep casually. On the two small repos the on-arm was flat to
  mildly *more* expensive.
- **Qwen + lore (unsteered): no cost win** (+23% input tokens overall). It
  adopted lore in 11/15 on-cells but shallowly (1–3 calls), and its
  run-to-run variance (up to 3× input tokens on the same cell) dominates
  most per-cell deltas. Where it *did* lean on retrieval (lexomancy T2/T4),
  the deltas look like luna's.
- **Adoption is repo-shaped, not model-shaped**: both models used lore most
  on Lexomancy (luna 22 calls, qwen 11) — retrieval pulls hardest where
  native exploration is most expensive.

## Totals

| model | arm | wall | input tok | output tok | tool calls | lore calls |
|---|---|---|---|---|---|---|
| luna | off | 38m49s | 1,236,914 | 44,814 | 561 | 0 |
| luna | on  | 33m08s | 1,065,459 | 41,510 | 512 | 41 |
| qwen | off | 31m23s | 6,733,968 | 111,617 | 266 | 0 |
| qwen | on  | 34m46s | 8,296,570 | 146,441 | 292 | 29 |

Qwen burns ~6× luna's input tokens for the same matrix — no prompt caching
through ollama and much noisier exploration; free locally, but the 128k
window is the binding constraint (no cell hit compaction:
`time_compacting` null across all 60).

## Per-cell matrices

`din` = on-arm input tokens vs off-arm. **Treat qwen's per-cell deltas as
noise-dominated** (see Replicates); luna's are stable enough to read.

### luna / lore-bench

| task | off wall | off in | off tools | on wall | on in | on tools | lore | din |
|---|---|---|---|---|---|---|---|---|
| T1 | 356s | 102,460 | 34 | 99s | 103,054 | 31 | 2 | +1% |
| T2 | 48s | 33,770 | 17 | 36s | 34,948 | 11 | 3 | +3% |
| T3 | 112s | 75,228 | 26 | 110s | 76,285 | 27 | 1 | +1% |
| T4 | 25s | 19,471 | 9 | 28s | 25,055 | 8 | 4 | +29% |
| T5 | 245s | 59,894 | 37 | 273s | 81,704 | 31 | 1 | +36% |

### luna / terrarium-bench

| task | off wall | off in | off tools | on wall | on in | on tools | lore | din |
|---|---|---|---|---|---|---|---|---|
| T1 | 168s | 121,130 | 65 | 223s | 143,077 | 77 | 2 | +18% |
| T2 | 86s | 50,178 | 32 | 77s | 72,585 | 27 | 3 | +45% |
| T3 | 112s | 75,531 | 39 | 136s | 107,930 | 46 | 1 | +43% |
| T4 | 46s | 42,152 | 17 | 51s | 29,926 | 15 | 1 | −29% |
| T5 | 157s | 56,911 | 44 | 193s | 69,918 | 38 | 1 | +23% |

### luna / Lexomancy

| task | off wall | off in | off tools | on wall | on in | on tools | lore | din |
|---|---|---|---|---|---|---|---|---|
| T1 | 189s | 138,057 | 66 | 135s | 101,387 | 56 | 3 | −27% |
| T2 | 61s | 42,331 | 25 | 16s | 19,261 | 4 | 4 | −54% |
| T3 | 400s | 89,437 | 66 | 170s | 62,060 | 48 | 3 | −31% |
| T4 | 82s | 81,935 | 36 | 52s | 26,851 | 19 | 6 | −67% |
| T5 | 235s | 248,429 | 48 | 380s | 111,418 | 74 | 6 | −55% |

Every Lexomancy task is a large on-arm win for luna; T3 halves wall time,
T2 finishes in 4 tool calls instead of 25.

### qwen / lore-bench

| task | off wall | off in | off tools | on wall | on in | on tools | lore | din |
|---|---|---|---|---|---|---|---|---|
| T1 | 68s | 185,250 | 10 | 65s | 231,531 | 12 | 12 | +25% |
| T2 | 35s | 97,202 | 10 | 29s | 95,084 | 8 | 2 | −2% |
| T3 | 56s | 152,902 | 10 | 83s | 356,933 | 17 | 0 | +133% |
| T4 | 19s | 45,628 | 6 | 17s | 43,020 | 2 | 2 | −6% |
| T5 | 275s | 485,324 | 23 | 467s | 1,434,130 | 46 | 0 | +195% |

### qwen / terrarium-bench

| task | off wall | off in | off tools | on wall | on in | on tools | lore | din |
|---|---|---|---|---|---|---|---|---|
| T1 | 48s | 172,742 | 15 | 71s | 413,289 | 19 | 1 | +139% |
| T2 | 27s | 78,701 | 7 | 54s | 94,196 | 12 | 0 | +20% |
| T3 | 36s | 106,338 | 10 | 48s | 182,441 | 13 | 0 | +72% |
| T4 | 11s | 36,868 | 3 | 11s | 27,777 | 2 | 1 | −25% |
| T5 | 88s | 422,866 | 24 | 139s | 644,856 | 32 | 0 | +52% |

### qwen / Lexomancy

| task | off wall | off in | off tools | on wall | on in | on tools | lore | din |
|---|---|---|---|---|---|---|---|---|
| T1 | 91s | 298,691 | 30 | 85s | 462,198 | 25 | 2 | +55% |
| T2 | 36s | 81,905 | 8 | 33s | 48,435 | 3 | 3 | −41% |
| T3 | 72s | 47,380 | 7 | 81s | 421,659 | 21 | 2 | +790% |
| T4 | 35s | 207,872 | 14 | 12s | 43,358 | 2 | 2 | −79% |
| T5 | 979s | 4,314,299 | 89 | 883s | 3,797,663 | 78 | 2 | −12% |

## Lore adoption (on-arm: lore calls / total tool calls)

| repo | luna T1 | T2 | T3 | T4 | T5 | qwen T1 | T2 | T3 | T4 | T5 |
|---|---|---|---|---|---|---|---|---|---|---|
| lore | 2/31 | 3/11 | 1/27 | 4/8 | 1/31 | 12/12 | 2/8 | 0/17 | 2/2 | 0/46 |
| terrarium | 2/77 | 3/27 | 1/46 | 1/15 | 1/38 | 1/19 | 0/12 | 0/13 | 1/2 | 0/32 |
| lexomancy | 3/56 | 4/4 | 3/48 | 6/19 | 6/74 | 2/25 | 3/3 | 2/21 | 2/2 | 2/78 |

Luna touched lore in **15/15** on-cells; qwen in **11/15**. The on-arm ran
**unsteered** — no AGENTS.md nudge, stock tool descriptions — so these are
organic-adoption numbers. Round-2 steering proposals (tool-description
use-cue vs repo AGENTS.md nudge):
`2026-08-16_round-2-steering-drafts.md`.

## Replicates — qwen T5 variance

The four qwen git-repo T5 cells ran twice (see Validity). Same cell, same
prompt, same config:

| cell | run 1 | run 2 |
|---|---|---|
| lore-off-T5 | 473s / 1,442,872 in / 46 tools | 275s / 485,324 in / 23 tools |
| lore-on-T5 | 410s / 563,322 in / 22 tools / 1 lore | 467s / 1,434,130 in / 46 tools / 0 lore |
| terrarium-off-T5 | 115s / 630,607 in / 30 tools | 88s / 422,866 in / 24 tools |
| terrarium-on-T5 | 211s / 687,953 in / 28 tools | 139s / 644,856 in / 32 tools |

Up to **3× input-token spread** on identical conditions. Any single qwen
cell delta smaller than ~2× is inside noise; only the aggregate rows and
the repeated Lexomancy pattern are worth interpreting. (Luna cells ran
once; its lexomancy deltas are consistent enough across five tasks to
stand.)

## Validity notes / incidents

- **Embedding backend (qwen day only)**: daemon embeddings served by a
  RunPod TEI pod (Qwen3-Embedding-4B fp16) via `scripts/embed-remote-proxy.py`
  (commit 1697ef0) — the 5090 can't hold qwen3.8 + the local embedder.
  Store vectors untouched (fingerprint unchanged; GGUF↔fp16 cosine
  ≥0.9995 verified on probes). Local stack restored after the run. Luna day
  used the local llama-server. Query-embedding latency differs a little
  between days; retrieval results should not.
- **Canonical cell set**: 30 luna (all first-run) + 30 qwen. For qwen,
  the graded T5 cells are the re-runs (see next point) and lexomancy-off-T1
  is the 020450 re-run; `x-`-prefixed dirs and the two dead wedge cells
  (003804, 013529 — exit −1, killed by hand) are excluded from every table.
- **Lost first-run T5 diffs**: run.ps1's `--output=(...)` pwsh parse bug
  (fixed, commit 2b5d5f6) silently discarded the diffs of the four qwen
  git-repo T5 first runs before the tree reset. Their metrics are valid
  observations (used in the Replicates table); T5 grading uses the re-run
  diffs. Luna's 08-15 diffs predate the refactor and are intact. Lexomancy
  T5 uses the cm capture path — never affected.
- **Deadlock, twice**: qwen-lexomancy-off-T1 froze at ~2 turns both
  attempts — headless `opencode run` cannot answer the
  `external_directory` permission ask triggered when a glob crosses the
  Lexomancy-bench junctions into Lexomancy-alt. Fixed by allowing
  `external_directory` in **both** arm configs (commit 9224c28) after the
  30-cell matrix; inert for every other cell (any earlier hit would have
  been an infinite hang, and none occurred). The on-arm never tripped it —
  it used lore_search instead of globbing.
- **Diff noise**: git-repo T5 diffs contain an empty `.loreignore`
  new-file entry (daemon auto-generation swept up by `git add -N`). Not
  agent work; ignore when grading.
- **No compaction anywhere**: `time_compacting` is null for all 60 cells;
  qwen's 128k protocol never engaged.

## Open questions for grading

1. Does luna's Lexomancy token/wall win come with equal-or-better answer
   quality? (The efficiency story is only real if scores hold.)
2. Do qwen's four zero-lore on-cells score worse than off-arm equivalents
   — i.e. did ignoring the tool cost it correctness, not just tokens?
3. lore-on-T1 (qwen, 12/12 lore calls) vs lore-off-T1: purest available
   retrieval-vs-grep comparison in the qwen set.
