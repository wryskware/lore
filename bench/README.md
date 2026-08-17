# E2E bench harness

Authority for prompts, pins, protocol, and grading:
**`design/6_Evaluation/2026-08-17_e2e-round-2-task-set.md`**. It is
self-contained and supersedes the round-1 plan, answer-key and Revision A
addendum for round-2 purposes; those three stay on disk as the record of what
round 1 was graded against. This folder is the machinery.

- `run.ps1` — one cell (`-Model luna|qwen -Repo lore|terrarium|lexomancy
  -Arm on|off -Task T1..T5`) or the whole matrix (`-Matrix`; `-Arms on` for
  the on-arm-only shape).
- `setup-worktrees.ps1` — run-day setup on the live machine: second worktree
  per repo, daemon registration, binary pin, **corpus scrub**, **authority
  profile**, index drain. Dry-run by default.
- `opencode-on.jsonc` / `opencode-off.jsonc` — arm configs, selected via
  `OPENCODE_CONFIG`. opencode **merges** configs with the global one, so the
  off arm explicitly sets `mcp.lore.enabled: false` (verified 2026-08-15).
- `prompts.json` — machine copy of the prompts, carrying a `_task_set` id.
  Keys never go here. Prompts are **frozen within a round, not across rounds**:
  round 2 is independent of round 1 by decision, so editing a prompt means
  bumping `_task_set` and re-freezing, not preserving round-1 wording.
- `results/<stamp>-<cell>/` — `events.jsonl` (raw stream, tool arguments
  included), `answer.md` (concatenated assistant text), `metrics.json`
  (wall/tokens/tool-calls/lore-calls/compaction, plus the slot, tree, project,
  pinned-binary hash, `task_set` and `prompt_sha256`; `score` filled by hand),
  `stderr.log`, `diff.patch` + reset for T5, `cm-changed.txt` for Lexomancy T5.

**What a grading criterion may rest on.** Only the artifacts above. Suites are
*not* captured — the grader runs them by hand and records the result. Nothing
should be graded on an agent's reasoning: it survives only as raw events, and
no grader should be asked to adjudicate that.

## Corpus hygiene (round 2)

The pinned lore tree contains `design/9_Scratch/2026-08-15_e2e-round-1-plan.md`,
which spells out the round-1 task list **and the graded answers for all three
repos**. That is the answer key sitting inside the corpus under test.
`setup-worktrees.ps1 -Apply -Scrub` deletes it from both slots and `run.ps1`
refuses to run a cell while it exists. Because it is a *tracked* file, the T5
reset would otherwise restore it: the scrubbed paths are excluded from the
`add -N`/`diff` pathspecs and from the restoring `checkout`, the same mechanism
that already protects `.lore.toml` and `.loreignore`.

`design/9_Scratch/2026-08-14_deferred-backlog.md` stays. It names the ATX
trailing-`#` bug that lore T5 asks about, but it is a genuine repo artifact
that predates the benchmark — a bug tracker naming a bug is not contamination.

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

A round scoped **on-arm-only** (`-Matrix -Models luna -Arms on`, 15 cells)
exercises only slot `a`; slot b buys nothing until a round runs two arms (or
two on-arm variants) concurrently. Note that the round-2 task set is written
for a **two-arm** comparison — every key self-checks against "could the off arm
plausibly succeed, could the on arm plausibly fail" — so an on-arm-only round
measures steering, not retrieval value.

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
3a. **Scrub the corpus:** `.\setup-worktrees.ps1 -Apply -Scrub`. Idempotent,
   acts on both slots. `run.ps1` will refuse to run otherwise.
3b. **Apply the authority profile:** `.\setup-worktrees.ps1 -Apply -Authority`.
   This appends `[authority] profile = "lore-v1", behavior = "rank"` to
   `lore-bench`'s (and `lore-bench-b`'s) `.lore.toml`, restoring the
   configuration the lore repo really runs under; round 1 indexed it with
   `authority: none` because the pin had no committed `.lore.toml`, which
   quietly meant the repo's own authority/modality task ran against a
   neutrally-indexed project. **Terrarium is deliberately left neutral** — it
   has no `design_status` convention and no ledger, so a profile there would
   declare a policy the corpus does not follow. Lexomancy retrieves from the
   main `Lexomancy` root, which already runs `lore-v1 (rank)`. Change the
   `$authorityProfile` table in `setup-worktrees.ps1` if you want uniformity
   instead; it is one line per repo.
   A profile flip re-chunks Markdown but does **not** re-embed, so step 5 is
   short for this on its own.
4. **Restart the daemon on the new build.**
5. **Wait for the full re-embed.** `CHUNK_FORMAT_VERSION` moved 4 → 5 on
   `main`, which invalidates every persisted chunk: the first pass on the new
   binary re-chunks and re-embeds **every registered project**, Lexomancy's
   ~326k chunks included. Starting a round before this settles measures a cold
   index, not the product. `.\setup-worktrees.ps1 -WaitDrained` polls
   `lore status` until every project reads `embedded N/N (100%)`.
6. **Pick one embedding backend and use it for the whole round.** Round 2 is
   not being compared to round 1, so matching round 1's mixed backends buys
   nothing — internal consistency across the round's own cells is what matters.
   Record which one in the results notes.
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
