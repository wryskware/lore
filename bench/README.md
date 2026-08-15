# E2E round 1 bench harness

Authority for prompts, pins, protocol, and grading:
`design/9_Scratch/2026-08-15_e2e-round-1-answer-key.md` and
`.../2026-08-15_e2e-round-1-plan.md`. This folder is the machinery.

- `run.ps1` — one cell (`-Model luna|qwen -Repo lore|terrarium|lexomancy
  -Arm on|off -Task T1..T5`) or the whole matrix (`-Matrix`, off-arm first).
- `opencode-on.jsonc` / `opencode-off.jsonc` — arm configs, selected via
  `OPENCODE_CONFIG`. opencode **merges** configs with the global one, so the
  off arm explicitly sets `mcp.lore.enabled: false` (verified 2026-08-15).
- `prompts.json` — machine copy of the frozen prompts. Keys never go here.
- `results/<stamp>-<cell>/` — `events.jsonl` (raw stream), `answer.md`,
  `metrics.json` (wall/tokens/tool-calls/lore-calls/compaction; `score`
  filled by hand), `diff.patch` + reset for T5.

Prerequisites per run day: lore daemon up with `lore-bench`,
`terrarium-bench`, `Lexomancy` (and the others) drained to 100% embedded;
ollama serving `qwen3.8:latest` + `nomic-embed-text`; Lexomancy main
workspace and `Lexomancy-alt` frozen at cs:134.

T5 grading: diff captured to `diff.patch`, tree auto-reset (git checkout/
clean; Lexomancy: `cm undo` of only newly-changed files — pre-existing
local changes in Lexomancy-alt are preserved). Suites run by the grader,
not the agent — see the answer-key doc.
