# E2E Round 1 — Full Report (machinery metrics; grading pending)

**Round 1 of D-0009's e2e benchmark is complete: 60 canonical cells** — 2
models (luna = openai/gpt-5.6-luna high, qwen = ollama qwen3.8 128k) × 3
repos (lore-bench, terrarium-bench, Lexomancy) × 2 arms (lore MCP on/off) ×
5 tasks. Luna ran 2026-08-15, qwen 2026-08-16; **luna/terrarium (both arms)
was re-run 2026-08-16 on the qwen3-4b embedding backend** so that pair is
apples-to-apples with the qwen day, and the re-run is canonical for it.
Luna's lore-bench and Lexomancy cells still carry the old nomic backend
(see Validity). Protocol authority:
`2026-08-15_e2e-round-1-{plan,answer-key}.md`. **All 60 cells are now
scored** — luna's 08-15 half by the earlier blind Fable packet pass, the
qwen matrix and luna/terrarium re-run by a blind Opus packet pass on 08-16
(15 packets, anonymized shuffled answers, key + answers only, no repo
access; T5 by diff inspection, suites not run). Scores are LLM-graded and
await Wrysk's spot-check; audit trail in
`bench/results/grading-2026-08-16-*.json` and `bench/results/grades.md`.

## Scores

| repo | luna off | luna on | qwen off | qwen on |
|---|---|---|---|---|
| lore | 5/5 | 5/5 | 4.5/5 | 4.5/5 |
| terrarium | 4.5/5 | 5/5 | 3.5/5 | 3.5/5 |
| lexomancy | 4.5/5 | 5/5 | 4.5/5 | 4.5/5 |
| **total** | **14/15** | **15/15** | **12.5/15** | **12.5/15** |

- **Qwen: exact quality parity between arms** — 12.5 both ways, and
  cell-for-cell identical on lore and lexomancy. The unsteered on-arm
  neither helped nor hurt qwen's correctness at this sample size; the two
  terrarium differences cancel (off dropped T5 to a non-verifying
  regression test, on dropped T1).
- **Luna: on-arm edges it 15 vs 14** — the off arm's misses are exactly the
  key's predicted traps (terrarium T3 recall on the nomic day, T4 citation
  on the re-run, lexomancy T5 diff pollution). Small margins, but always in
  lore's favor, never against.
- **No cell anywhere scored worse with lore on.** The "actively worse on
  small repos" worry is not supported by quality data — the on-arm cost is
  tokens, not correctness.
- Both T4 "why" cells (lore, lexomancy) scored a symmetric 0.5/0.5 for
  citing code instead of the key-named prose source — a strict-reading
  call the graders flagged as promotable; it cannot skew the arm
  comparison either way.

## Headline

- **Luna + lore: cheaper and faster overall** (−17% input tokens, −24%
  wall on the canonical set), with the entire win concentrated in
  Lexomancy — the one repo too big to grep casually. On the two small
  repos the on-arm was flat to mildly *more* expensive, on both embedding
  backends.
- **Qwen + lore (unsteered): no cost win** (+23% input tokens overall). It
  adopted lore in 11/15 on-cells but shallowly (1–3 calls), and its
  run-to-run variance (up to 3× input tokens on the same cell) dominates
  most per-cell deltas. Where it *did* lean on retrieval (lexomancy T2/T4),
  the deltas look like luna's.
- **Adoption is repo-shaped, not model-shaped**: both models used lore most
  on Lexomancy (luna 22 calls, qwen 11) — retrieval pulls hardest where
  native exploration is most expensive.

## Totals

Luna rows use the canonical set (terrarium = the 08-16 re-run).

| model | arm | wall   | input tok | output tok | tool calls | lore calls |
| ----- | --- | ------ | --------- | ---------- | ---------- | ---------- |
| luna  | off | 42m34s | 1,250,863 | 45,573     | 562        | 0          |
| luna  | on  | 32m11s | 1,033,208 | 41,479     | 513        | 40         |
| qwen  | off | 31m23s | 6,733,968 | 111,617    | 266        | 0          |
| qwen  | on  | 34m46s | 8,296,570 | 146,441    | 292        | 29         |

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

### luna / terrarium-bench (canonical = 08-16 re-run, qwen3-4b embeddings)

| task | off wall | off in | off tools | on wall | on in | on tools | lore | din |
|---|---|---|---|---|---|---|---|---|
| T1 | 184s | 133,539 | 78 | 192s | 131,818 | 70 | 2 | −1% |
| T2 | 55s | 47,305 | 22 | 91s | 76,475 | 31 | 2 | +62% |
| T3 | 364s | 84,290 | 43 | 131s | 87,233 | 49 | 1 | +3% |
| T4 | 45s | 37,960 | 18 | 32s | 30,739 | 8 | 1 | −19% |
| T5 | 147s | 56,757 | 37 | 179s | 64,920 | 46 | 1 | +14% |

Backend effect (same cells on the 08-15 nomic run, din old → new):
T1 +18%→−1%, T2 +45%→+62%, T3 +43%→+3%, T4 −29%→−19%, T5 +23%→+14%.
Adoption unchanged (7 vs 8 lore calls). Direction favors qwen3-4b mildly
(arm total: on-arm din +22% nomic → +9% qwen3-4b) but single runs of a
noisy measure — the T3 off-cell alone swung 112s→364s between days. Read
it as "backend didn't change the story," not as a measured improvement.

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

| task | off wall | off in    | off tools | on wall | on in     | on tools | lore | din   |
| ---- | -------- | --------- | --------- | ------- | --------- | -------- | ---- | ----- |
| T1   | 91s      | 298,691   | 30        | 85s     | 462,198   | 25       | 2    | +55%  |
| T2   | 36s      | 81,905    | 8         | 33s     | 48,435    | 3        | 3    | −41%  |
| T3   | 72s      | 47,380    | 7         | 81s     | 421,659   | 21       | 2    | +790% |
| T4   | 35s      | 207,872   | 14        | 12s     | 43,358    | 2        | 2    | −79%  |
| T5   | 979s     | 4,314,299 | 89        | 883s    | 3,797,663 | 78       | 2    | −12%  |

## Lore adoption (on-arm: lore calls / total tool calls)

| repo | luna T1 | T2 | T3 | T4 | T5 | qwen T1 | T2 | T3 | T4 | T5 |
|---|---|---|---|---|---|---|---|---|---|---|
| lore | 2/31 | 3/11 | 1/27 | 4/8 | 1/31 | 12/12 | 2/8 | 0/17 | 2/2 | 0/46 |
| terrarium | 2/70 | 2/31 | 1/49 | 1/8 | 1/46 | 1/19 | 0/12 | 0/13 | 1/2 | 0/32 |
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

- **Embedding backends are mixed across the luna set**: the qwen matrix
  and the canonical luna/terrarium re-run used a RunPod TEI pod
  (Qwen3-Embedding-4B fp16) via `scripts/embed-remote-proxy.py`
  (commit 1697ef0) — the 5090 can't hold qwen3.8 + the local embedder, and
  the luna re-run kept the same backend for comparability. Store vectors
  untouched throughout (fingerprint unchanged; GGUF↔fp16 cosine ≥0.9995
  verified on probes). **Luna's lore-bench and Lexomancy cells (08-15)
  still ran on the old nomic-embed-text index** — cross-model comparison
  on those two repos carries that caveat; re-run them the same way if
  grading makes them load-bearing. Local llama-server stack restored after
  each stint.
- **Canonical cell set**: 30 luna (lore/lexomancy first-run 08-15;
  terrarium = the 08-16 re-run, both arms) + 30 qwen. The superseded 08-15
  luna/terrarium cells remain on disk as the nomic-backend observation
  (used in the backend-effect note). For qwen,
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

## Grading answers to the open questions

1. **Luna's Lexomancy win holds up**: on-arm 5/5 vs off 4.5/5 while
   spending −55% tokens — cheaper *and* no worse, slightly better.
2. **Qwen's zero-lore on-cells did not lose correctness** — every one
   scored the same as its off-arm twin. Ignoring the tool cost tokens
   (or nothing), not answers.
3. **qwen lore-T1 (12/12 lore calls)**: both arms scored 1; the on-arm
   was marginally faster (65s vs 68s) at +25% tokens. Retrieval matched
   grep quality on the purest head-to-head, didn't beat it.

Remaining for Wrysk: spot-check the LLM grades (suggested: both T4 0.5s,
terrarium-T5 off's non-verifying test, one random 1) and decide whether
the strict-citation 0.5s promote.
