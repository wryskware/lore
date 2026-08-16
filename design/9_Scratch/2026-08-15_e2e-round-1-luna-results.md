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

| Arm | Score | Notes |
| --- | --- | --- |
| retrieval-on | **15 / 15** | two quality wins the off arm didn't get |
| retrieval-off | **13 / 15** | −0.5 terrarium T3 recall, −0.5 lexomancy T5 diff quality |

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
