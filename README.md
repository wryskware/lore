# Lore

**A local context daemon for AI coding agents.**

Coding agents rediscover your codebase on every task. They grep, they read
files that turn out to be irrelevant, they burn tokens reconstructing what the
last session already worked out — and they never find the design document that
explains why the code is the way it is, because grep does not know that
`ThreadingModel.md` answers a question about `Scheduler.cs`.

Lore indexes your repository once and serves it back over MCP. Symbol lookup
stops depending on repo size, prose questions get answered from the documents
that actually answer them, and your design vault becomes part of what the agent
can see.

Everything runs on your machine. No code leaves it.

> **Status: pre-1.0 and moving.** The retrieval core is built, tested, and used
> daily on real repositories. Windows is the supported platform today. Interfaces
> are still changing.

## Why it is different

**Retrieval is size-independent.** Grep re-reads the repository every time, so
its cost scales with bytes on disk. Lore answers from an index. Measured on this
machine, same symbol lookup, `ripgrep` vs `lore search`:

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
actually says so. A design vault stops being dead weight the agent never opens.

**Hybrid search, not just vectors.** Full-text (SQLite FTS5) and semantic search
run together and are fused by reciprocal rank. Exact identifiers stay findable;
so does the paragraph that never mentions them.

**Local embeddings.** Point Lore at any OpenAI-compatible embedding endpoint you
run yourself — `llama-server`, Ollama, TEI. There is no hosted tier and no
telemetry.

## What it measures out to

From the round-1 end-to-end benchmark — the same coding tasks, same model, run
with retrieval on and off, graded blind:

- **−17% input tokens and −24% wall time** on the frontier-model arm.
- **No task anywhere scored worse with Lore on** than with it off.

The honest reading, including the parts that are less flattering: the second
result matters more than the first. Token savings were not uniform — a small
repository where the agent could simply read everything got *more* expensive
with retrieval on, and a smaller local model showed exact scoring parity between
arms rather than a win. What the run does rule out is that Lore makes an agent
worse. Its cost is tokens, not correctness, and the wins concentrate on large
repositories and on questions whose answer lives in prose rather than code.

Round 1 was a single run at small n. The harness, prompts, answer keys and
grades are all in `bench/` and `design/6_Evaluation/`, so you can disagree with
it on the evidence.

## Requirements

- Windows. Linux via WSL2 is untested ([#3](https://github.com/wryskware/lore/issues/3)).
- Rust 1.89+ to build.
- An OpenAI-compatible embedding endpoint for semantic search. Optional —
  without one, Lore degrades to lexical-only search and says so.

## Install

```sh
cargo install --path crates/lore
cargo install --path crates/lore-mcp
```

Both binaries need to be on `PATH`. Start the daemon:

```sh
lore daemon
```

Then register a repository and index it:

```sh
cd path/to/your/repo
lore init          # write a .loreignore based on what the repo looks like
lore add           # register this directory with the daemon
lore status        # watch it index
```

`lore search "how does the scheduler handle cancellation"` queries the same
surface agents get.

### Semantic search

Lore does not run an embedding model for you — you point it at one. Write
`config.toml` in the daemon's data directory (`%LOCALAPPDATA%\lore\`):

```toml
[embeddings]
endpoint = "http://127.0.0.1:8090/v1"
model = "qwen3-embedding-4b"
dimensions = 2560
```

Every key is optional and unknown keys are rejected rather than ignored — a
misspelled `endpoint` fails loudly instead of presenting as "embeddings
mysteriously never turned on". `scripts/serve-embeddings.ps1` is the launcher
used on the development machine, and `scripts/install-autostart.ps1` registers
the daemon and the embedding server as logon tasks.

Changing the model or its dimensions re-embeds the index.

### Wiring an agent to it

Lore speaks MCP through the `lore-mcp` binary, which offers three tools:
`search`, `expand` (pull more context around a result), and `status`. Run this
from the repo you want indexed:

```console
$ lore setup mcp
project scope   C:\path	oepo\.mcp.json
  registered lore -> C:\path	o\lore-mcp.exe
```

It writes a `.mcp.json` at the repo root naming the `lore-mcp` that shipped
beside the `lore` you ran, which is how a stale server binary left on `PATH`
stops being a way to lose an afternoon. `--global` registers in
`~/.claude.json` instead, for every session on the machine — off by default,
because most directories on a machine are not Lore projects and a server with
nothing to serve is just a process. An entry lore did not write is never
replaced without `--force`, and nothing else in the file is touched.

The server scopes itself to the project containing the working directory, so
agents never have to name a project or reach into a repo they are not working
in. `lore setup claude-code` installs the agent-side skills — `lore-search`,
the method for using the index (search by concept, read a hit's provenance and
authority, expand before quoting, when to fall back to grep), and `lore-ignore`,
the method for tuning what a repo indexes — and bare `lore setup` reports on all
of it without writing anything.

The skills exist because the tool descriptions cannot carry a procedure.
Benchmarking found that agents told nothing about Lore never reach for it, and
that agents told only that it exists still re-derive every hit by hand — the
steering is what turns a returned result into a saved read.

## How it works

One daemon per machine owns the index — a single writer, enforced with a kernel
file lock, because concurrent agents racing to reindex the same repository is
the failure mode that sinks tools like this. Agents get read-only verbs.

Files are chunked on syntax rather than by line count: tree-sitter finds symbol
boundaries for C#, Rust, Python, JavaScript and TypeScript, Markdown is cut on
its heading tree, and anything else falls back to overlapping line windows.
Chunks, full-text index, and vectors live in one SQLite database and update in a
single transaction per pass, so a search never sees a half-written index.

A filesystem watcher picks up edits and re-indexes on a debounce; `lore index`
forces a pass immediately.

## Documentation

Design lives in `design/`, and it is unusually complete because Lore is built
using itself. `design/0_Canon/DECISIONS.md` is the decision ledger — the
authoritative record of what has actually been decided and why, as opposed to
what merely got written down. `design/1_Architecture/` covers topology and
ingestion; `design/3_Retrieval/` covers chunking, ranking and embeddings.

Start with `design/0_Canon/README.md`, which explains how to tell a decision
from a proposal.

## License

Not yet chosen — see [#1](https://github.com/wryskware/lore/issues/1). Until one
is declared here, no license is granted.
