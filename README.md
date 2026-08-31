# Lore

**A local context daemon for AI coding agents.**

Coding agents rediscover your codebase on every task. They grep, they read
files that turn out to be irrelevant, they burn tokens reconstructing what the
last session already worked out — and they never find the design document that
explains why the code is the way it is, because grep does not know that
`ThreadingModel.md` answers a question about `Scheduler.cs`.

Lore indexes your repository once and serves it back over MCP. Symbol lookup
stops depending on repo size, prose questions get answered from the documents
that actually answer them, and your design vault becomes part of what the
agent can see.

Everything runs on your machine. No code leaves it.

> **Status: pre-1.0 and moving.** The retrieval core and the bundle surface
> are built, tested, and used daily on real repositories. Windows is the
> supported platform today. Interfaces are still changing.

## What an agent gets

Four MCP tools, in the order an agent should reach for them:

**`bundle`** answers a retrieval question outright. One call returns a
finished evidence bundle: a verdict, verified line-numbered spans rendered
from the files on disk at that moment — never from the index — and an honest
list of what was *not* found. Asked about its own verdict logic, Lore returns:

```text
VERDICT: found (3 verified span(s) from 2 file(s))
NO MATCH FOR: between
=== design/4_Interfaces/2026-08-27_bundle-mcp-tool.md:155-170 [… > Why term coverage, not score] ===
155  ## Why term coverage, not score
156
157  lore's fusion is RRF: score = Σ 1/(60+rank) — a pure function of rank.
158  Measured on the bench corpus: a nonsense query's top hit (0.0294)
159  outscored the #2 hit of a well-answered query. Any score threshold
160  therefore manufactures confident `found` on empty results. …
=== crates/lore/src/daemon/bundle.rs:1356-1408 [tests.verdict_cuts_sit_where_the_calibration_put_them] ===
     …
FURTHER READING: design/0_Canon/DECISIONS.md:288-307, crates/lore-core/src/lib.rs:549-646, …
```

Every span is mechanically verified before it renders — the path resolves,
the range exists, the text is what is on disk — so an agent may quote and
edit from it without re-reading the file. The verdict comes from query-term
coverage, not retrieval score, because scores rank; they do not tell you when
nothing matched. The `NO MATCH FOR:` line is the honest-gap signal: what the
bundle could not cover is named, not papered over.

**`search`** is for steering the retrieval yourself — filters, narrowing
passes, ranked hits with provenance. **`expand`** pulls more context around a
hit. **`status`** reports index health, including whether semantic search is
degraded to lexical-only.

## Why it is different

**Retrieval is size-independent.** Grep re-reads the repository every time, so
its cost scales with bytes on disk. Lore answers from an index. Measured on
this machine, same symbol lookup, `ripgrep` vs `lore search`:

| Repository | Size | ripgrep | lore |
| --- | --- | --- | --- |
| lore | 1.7 MB | 0.33 s | 0.55 s |
| Lexomancy | 603 MB | 45.1 s | 0.52 s |

On a small repo grep wins, and it should — Lore is not trying to replace it
there. The point is the second row: Lore's cost barely moved. That is the
difference between an agent that can afford to look things up and one that
guesses instead.

**It reads your design documents as documents.** Markdown is chunked on its
heading tree, so a returned chunk is a section that knows where it sits.
Frontmatter is parsed. If a repository keeps a decision ledger, Lore reads it —
and a document claiming to be settled only ranks as settled if the ledger
actually says so. A design vault stops being dead weight the agent never
opens.

**Hybrid search, not just vectors.** Full-text (SQLite FTS5) and semantic
search run together and are fused by reciprocal rank. Exact identifiers stay
findable; so does the paragraph that never mentions them.

**Local embeddings.** Point Lore at any OpenAI-compatible embedding endpoint
you run yourself — `llama-server`, Ollama, TEI, vLLM. There is no hosted tier
and no telemetry.

## Measured, not promised

Lore is developed against a standing evaluation program — its own end-to-end
bench, an external benchmark we cannot tune
([RepoContextBench](https://github.com/CodeAlive-AI/repo-context-bench)), a
write-task bench, and agent-free retrieval scoring. The full story, with
links to every report, is in **[docs/evaluation.md](docs/evaluation.md)**.
The short version:

- **No task in any end-to-end round scored worse with Lore on**, outside one
  weak-model round whose loss vanished when the same cells re-ran on a
  stronger model. Round 1: −17% input tokens, −24% wall time on the
  frontier arm.
- **On the external benchmark, the effect scales inversely with the agent's
  own strength.** Sonnet-class: wall and cost roughly halved. Mid-tier
  models: consistent token wins across repeats, and one hosted 27B model
  improved cost *and* judged quality together. Opus-class: a no-op — it
  re-explores natively.
- **The `bundle` contract was benchmarked before it shipped**: as an evidence
  package it cut frontier-agent tokens −58% (luna) / −32% (opus) against
  iterative search, and the shipped assembler ran the median cell faster and
  cheaper than the search loop with quality within noise.
- **The honest side:** a small repo the agent can read whole gets more
  expensive with retrieval on; write-task value is suggestive but not
  demonstrated; and a trained local "scout" model beats the mechanical
  assembler's citations offline but has not earned a place in the query path.
  Most results are single runs on one machine.

The harnesses, prompts, answer keys, and grades are all in
[`bench/`](bench/) and [`design/6_Evaluation/`](design/6_Evaluation/), so you
can disagree with any of this on the evidence.

## Quick start

Windows, Rust 1.89+. Full setup — including semantic search, autostart, and a
load-bearing linker note — is in **[docs/running.md](docs/running.md)**.

```sh
cargo install --path crates/lore --locked
cargo install --path crates/lore-mcp --locked

lore start         # background daemon; idempotent

cd path/to/your/repo
lore init          # write a .loreignore based on what the repo looks like
lore add           # register this directory with the daemon
lore status        # watch it index

lore search "how does the scheduler handle cancellation"
```

Wire an agent to it from the repo you want indexed:

```sh
lore setup mcp           # writes .mcp.json pointing at the right lore-mcp
lore setup claude-code   # installs the agent-side skills
```

The skills matter more than they sound: benchmarking found that agents told
nothing about Lore never reach for it, and agents told only that it exists
still re-derive every hit by hand. The steering is what turns a returned
result into a saved read.

Semantic search needs an embedding endpoint you run yourself; without one,
Lore degrades to lexical-only search and says so. Configuration lives in
[docs/running.md](docs/running.md#semantic-search).

## How it works

One daemon per machine owns the index — a single writer, enforced with a
kernel file lock, because concurrent agents racing to reindex the same
repository is the failure mode that sinks tools like this. Agents get
read-only verbs; `bundle` assembly happens daemon-side, so every surface
serves one implementation.

Files are chunked on syntax rather than by line count: tree-sitter finds
symbol boundaries for C#, Rust, Python, JavaScript and TypeScript, Markdown is
cut on its heading tree, and anything else falls back to overlapping line
windows. Additional languages arrive as WASM chunker plugins rather than core
changes. Chunks, full-text index, and vectors live in one SQLite database and
update in a single transaction per pass, so a search never sees a
half-written index.

A filesystem watcher picks up edits and re-indexes on a debounce;
`lore index <project>` forces a pass immediately.

## Roadmap

Where this is going, with the caveat that only the
[decision ledger](design/0_Canon/DECISIONS.md) makes anything binding:

- **Consumption-side bundle rounds.** The bundle assembler is measured; how
  much it saves each class of agent in practice is the number the program is
  collecting next.
- **Session memory.** A two-tier memory model — repo-resident lore docs plus
  a session ledger with recall — is designed (D-0006/D-0008) and not yet
  built.
- **The scout, offline.** The current leaning is that the trained scout's
  value is assembler-side — building better bundles ahead of time — not as a
  query-time subagent. See [the evaluation story](docs/evaluation.md#the-scout-can-a-trained-model-beat-the-assembler).
- **Unity, properly.** The flagship consumer is a C#/Unity codebase (D-0003);
  Unity-specific formats ride the chunker-plugin contract (D-0023).
- **Linux** ([#3](https://github.com/wryskware/lore/issues/3)) — the
  benchmarks already run under WSL2; first-class support has to earn its CI.
- **Earned upgrades only.** Storage and transport swaps (Tantivy, arroy,
  named pipes) happen when measurement demands them, not before.

## Documentation

| where | what |
| --- | --- |
| [docs/running.md](docs/running.md) | install, daemon, embeddings, MCP wiring — the manual |
| [docs/evaluation.md](docs/evaluation.md) | the testing story: every lane, every report, the honest reading |
| [design/](design/) | the design vault — Lore is built using itself, so it is unusually complete |
| [design/0_Canon/](design/0_Canon/README.md) | how to tell a decision from a proposal; the decision ledger |
| [bench/](bench/), [train/](train/) | the benchmark harnesses and the scout training pipeline, each with its own README |

Start with [design/0_Canon/README.md](design/0_Canon/README.md) before
treating any design document as binding — written, polished, or implemented
does not mean canonical, and the vault says so itself.

## License

Not yet chosen — see [#1](https://github.com/wryskware/lore/issues/1). Until
one is declared here, no license is granted.
