# E2E bench harness

Authority for prompts, pins, protocol, and grading:
`design/6_Evaluation/2026-08-15_e2e-round-1-answer-key.md` and
`.../2026-08-15_e2e-round-1-plan.md`, as amended for grading by
`.../2026-08-17_e2e-round-1-key-addendum.md` (Revision A). This folder is the
machinery.

- `run.ps1` — one cell (`-Model luna|qwen -Repo lore|terrarium|lexomancy
  -Arm on|off -Task T1..T5`) or the whole matrix (`-Matrix`; `-Arms on` for
  the round-2 on-arm-only shape).
- `setup-worktrees.ps1` — run-day setup on the live machine: second worktree
  per repo, daemon registration, binary pin, index drain. Dry-run by default.
- `opencode-on.jsonc` / `opencode-off.jsonc` — arm configs, selected via
  `OPENCODE_CONFIG`. opencode **merges** configs with the global one, so the
  off arm explicitly sets `mcp.lore.enabled: false` (verified 2026-08-15).
- `prompts.json` — machine copy of the frozen prompts. Keys never go here.
  **Frozen**: editing a prompt breaks comparability with every round-1 cell.
- `results/<stamp>-<cell>/` — `events.jsonl` (raw stream), `answer.md`,
  `metrics.json` (wall/tokens/tool-calls/lore-calls/compaction, plus the slot,
  tree, project and pinned-binary hash; `score` filled by hand), `diff.patch`
  + reset for T5.

## Two trees per repo (slots)

Each bench repo has **two** working trees, one per arm, so a T5 cell that
edits files cannot be seen by the other arm and the two arms can run at the
same time.

| repo | slot a (arm `on`) | slot b (arm `off`) | retrieval project |
| --- | --- | --- | --- |
| lore | `bench-e2e\lore-bench` | `bench-e2e\lore-bench-b` | `lore-bench` / `lore-bench-b` |
| terrarium | `bench-e2e\terrarium-bench` | `bench-e2e\terrarium-bench-b` | `terrarium-bench` / `terrarium-bench-b` |
| lexomancy | `Unity\Lexomancy-bench` (→ `Lexomancy-alt`) | `Unity\Lexomancy-bench-b` (→ `Lexomancy-alt-b`) | `Lexomancy` for **both** |

Slot a is the round-1 setup, untouched. The arm→slot map is fixed in
`run.ps1`, so a cell's tree is a pure function of `(repo, arm)`.

Two things about Lexomancy are deliberate, not oversights:

- Both slots retrieve from the **main `Lexomancy` project**. The walker does
  not follow junctions (backlogged since round 1), so a bench root indexes only
  its own two loose files. The main root is frozen at the pin and read-only
  during runs, so both arms sharing it is safe — but it also means Lexomancy
  never had live-index semantics, in round 1 or now. Carried forward as a known
  limitation; fixing it is a daemon-side change.
- Slot b therefore exists purely for **file isolation** during T5, and needs a
  second cm workspace (`Lexomancy-alt-b`). Until that exists, run Lexomancy T5
  one arm at a time. Slot b is still *registered* (as `Lexomancy-bench-b`,
  indexing its two loose files) purely so that both slots resolve a default
  project the same way — an unregistered cwd resolves to nothing, which is a
  behaviour difference between the arms, not just a missing index.

Round 2 is scoped **on-arm-only, 15 cells** (`-Matrix -Models luna -Arms on`).
Under that shape only slot `a` is exercised; slot b buys nothing until a round
runs two arms (or two on-arm variants) concurrently.

## Matrix scheduling

Three waves, in order:

1. **luna T1–T4** — parallel, capped at `-Throttle`. Read-only.
2. **luna T5** — parallel, capped at `-Throttle`. Every T5 cell has a distinct
   `(repo, arm)` and therefore a distinct tree. Held out of wave 1 because a T5
   write would otherwise land under a T1–T4 cell reading the same tree.
3. **qwen** — serial. GPU contention, not file contention.

`run.ps1` asserts both wave invariants (no duplicate cells, no two T5 cells
sharing a tree) and throws before launching anything if they do not hold.

## Round-2 setup (live machine, in order)

1. **Land the code you are benchmarking** and build it:
   `cargo build -p lore -p lore-mcp`. For a steering round this is Lever B from
   `design/99_Scratch/2026-08-16_round-2-steering-drafts.md`.
2. **Pin the binary:** `.\setup-worktrees.ps1 -Apply -PinBinary`. This copies
   `lore-mcp.exe` to `bench-e2e\bin\`. The arm configs point at that copy, so a
   later rebuild of the working checkout cannot silently re-pin a round in
   flight. Record the printed SHA-256 next to the round's results.
3. **Create + register slot b** (only if the round runs two arms concurrently):
   `.\setup-worktrees.ps1 -Apply`. Dry-run it first — no `-Apply` prints the
   plan and changes nothing. For Lexomancy it will tell you to create
   `Lexomancy-alt-b` by hand first.
4. **Restart the daemon on the new build.**
5. **Wait for the full re-embed.** `CHUNK_FORMAT_VERSION` moved 4 → 5 on
   `main`, which invalidates every persisted chunk: the first pass on the new
   binary re-chunks and re-embeds **every registered project**, Lexomancy's
   ~326k chunks included. Starting a round before this settles measures a cold
   index, not the product. `.\setup-worktrees.ps1 -WaitDrained` polls
   `lore status` until every project reads `embedded N/N (100%)`.
6. **Check the embedding backend matches the round you are comparing against.**
   Round 1's canonical cells are mixed: the qwen matrix and the luna/terrarium
   re-run used Qwen3-Embedding-4B; luna's lore-bench and Lexomancy cells used
   nomic-embed-text. See the round-1 report's Validity notes.
7. **Freeze the trees.** Main Lexomancy workspace and `Lexomancy-alt*` at
   `cs:134`; the git worktrees stay detached at their pins.
8. **Serving stack up:** ollama with `qwen3.8:latest` if the qwen arm runs;
   the embedding endpoint `lore status` reports as `ready`.
9. Run: `.\run.ps1 -Matrix -Models luna -Arms on -Throttle 5`.

## T5 capture and reset

Diff captured to `diff.patch`, tree auto-reset (git `checkout`/`clean`;
Lexomancy: `cm undo` of only newly-changed files — pre-existing local changes
are preserved). Suites are run by the grader, not the agent — see the answer
key.

Two things the reset deliberately does **not** touch: `.lore.toml` and
`.loreignore`. The daemon generates them inside a registered root, they are not
agent work, and `git clean -fd` would otherwise delete the project's
`.loreignore` mid-round and silently change what gets indexed. They are
excluded from the staged diff by pathspec and from the clean by `-e`.

## PowerShell quoting

Round 1 lost four T5 diffs to a silent pwsh argument-splitting bug: a bare
`--output=(Join-Path ...)` splits at the paren, so git received an empty
`--output=` and a stray path argument, captured nothing, and exited 0. Every
`--output=` in `run.ps1` is now a single double-quoted argument. Reproduce the
failure, and confirm the fix, with:

```powershell
$argv = "import sys,json;print(json.dumps(sys.argv[1:]))"
python -c $argv diff --output=(Join-Path "C:\tmp" "d.patch")    # 2 args — broken
python -c $argv diff "--output=$(Join-Path "C:\tmp" "d.patch")" # 1 arg  — correct
```
