---
design_status: exploration
last_reviewed: 2026-08-15
---

# E2E round 1 — luna half, graded results

Luna-only matrix (30 cells, 41 min wall at throttle 5). Graded by a Fable
thread against the frozen keys, packets only, no repo access
(`bench/results/grades.md` has per-cell rationale; raw cells in
`bench/results/`). Qwen half not yet run.

## Headline

| Arm | Score | Wall | Tokens (in+out) | Tool calls |
| --- | ---: | ---: | ---: | ---: |
| retrieval-on | **15 / 15** | 33.1 min | 1,106,969 | 512 |
| retrieval-off | **14 / 15** | 38.8 min | 1,281,728 | 561 |

(The grader's prose said 13/15 for off; its own per-cell table sums to 14 —
two 0.5s: terrarium T3 recall, lexomancy T5 diff quality. 14 is correct.)

## Per-repo numbers

### lore

| Task | off wall | off tok | off tools | off score | on wall | on tok | on tools (lore) | on score | Δwall | Δtok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| T1 | 357s | 106,637 | 34 | 1 | 99s | 106,439 | 31 (2) | 1 | -72% | -0% |
| T2 | 49s | 34,825 | 17 | 1 | 37s | 35,694 | 11 (3) | 1 | -25% | +2% |
| T3 | 113s | 77,764 | 26 | 1 | 110s | 78,760 | 27 (1) | 1 | -2% | +1% |
| T4 | 25s | 20,113 | 9 | 1 | 29s | 25,694 | 8 (4) | 1 | +14% | +28% |
| T5 | 245s | 63,018 | 37 | 1 | 274s | 84,577 | 31 (1) | 1 | +12% | +34% |

### terrarium

| Task | off wall | off tok | off tools | off score | on wall | on tok | on tools (lore) | on score | Δwall | Δtok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| T1 | 169s | 125,700 | 65 | 1 | 224s | 148,520 | 77 (2) | 1 | +33% | +18% |
| T2 | 87s | 52,158 | 32 | 1 | 78s | 74,415 | 27 (3) | 1 | -10% | +43% |
| T3 | 113s | 78,222 | 39 | 0.5 | 136s | 111,366 | 46 (1) | 1 | +21% | +42% |
| T4 | 47s | 43,305 | 17 | 1 | 52s | 31,028 | 15 (1) | 1 | +11% | -28% |
| T5 | 158s | 60,554 | 44 | 1 | 193s | 73,624 | 38 (1) | 1 | +23% | +22% |

### lexomancy

| Task | off wall | off tok | off tools | off score | on wall | on tok | on tools (lore) | on score | Δwall | Δtok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| T1 | 189s | 143,692 | 66 | 1 | 136s | 106,006 | 56 (3) | 1 | -28% | -26% |
| T2 | 61s | 43,689 | 25 | 1 | 17s | 19,557 | 4 (4) | 1 | -73% | -55% |
| T3 | 401s | 95,656 | 66 | 1 | 171s | 65,600 | 48 (3) | 1 | -57% | -31% |
| T4 | 82s | 84,144 | 36 | 1 | 52s | 28,041 | 19 (6) | 1 | -36% | -67% |
| T5 | 235s | 252,251 | 48 | 0.5 | 381s | 117,648 | 74 (6) | 1 | +62% | -53% |

- **Quality separations (on-arm wins):** terrarium T3 full recall (off
  missed `explore/rig.ts`, exactly the key's predicted drop); lexomancy T5
  clean diff (off's was polluted by a generated `.slnx` hunk); lore on-T5
  additionally bumped `CHUNK_FORMAT_VERSION` — a repo-convention detail
  retrieval surfaced.
- **Cost separations concentrate on the big repo.** Lexomancy T1–T4
  retrieval-on won every metric; T2: 17 s / 19k tokens / 4 tool calls vs
  61 s / 42k / 25. lore off-T1 trace: 357 s vs 99 s on.
- **Retrieval-on is not free:** on terrarium T1/T5 and lore T4/T5 the on
  arm cost *more* — trails show lore calls as additive garnish before the
  same exhaustive manual sweep. Tuning question for M2: when the model
  doesn't trust (or doesn't need) retrieval, the tax is real.
- **No authority-trap falls in either arm.** luna-high finds the ledgers
  unaided given time — the off arm paid a heavy search tax but got there.
  The traps may separate harder on qwen (128k, weaker) — that's the round's
  remaining question, along with the compaction signal.

## Caveats

- Lexomancy sandbox escape: junctions weren't traversed by agent tools, so
  Lexomancy cells worked in `Lexomancy-alt` or the live main workspace —
  on-T5 patched the **main** workspace (restored; cs:134 untouched) and ran
  EditMode via the `unity` CLI against the open editor. Fix before the qwen
  round: register `Lexomancy-alt` directly as the retrieval project (real
  directory, walker-indexable) so result paths keep agents inside the pin.
- Both Lexomancy T5 suites lack grader-side verification (one self-reported
  208-pass via the editor, one not run — classifier blocked the grader-side
  apply into the cm workspace).
- Luna-free measures capability, not cost (D-0009 caveat stands).
- Harness bugs found and fixed during the run: PowerShell `$Models` name
  collision; CRLF-mangled diff capture (now `git --output`); `cm diff
  <path>` GUI block (now `cm getfile` + local diff); two-cells-per-job
  timeout starvation (one cell per job now).
