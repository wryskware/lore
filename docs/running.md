# Running Lore

Everything operational: installing, running the daemon, wiring up semantic
search, and connecting an agent. The [README](../README.md) is the overview;
this is the manual.

## Requirements

- Windows. Linux via WSL2 is where the benchmarks run, but it is not yet a
  supported target ([#3](https://github.com/wryskware/lore/issues/3)).
- Rust 1.89+ to build.
- An OpenAI-compatible embedding endpoint for semantic search. Optional —
  without one, Lore degrades to lexical-only search and says so.

## Install

```sh
cargo install --path crates/lore --locked
cargo install --path crates/lore-mcp --locked
```

`--locked` is not optional politeness. `cargo install` ignores `Cargo.lock`
without it and re-resolves to the newest compatible dependencies, which is how
you end up running a build nobody has tested.

Both binaries need to be on `PATH`.

### A Windows linker footnote you should not skip

If you build with `rust-lld` as your linker rather than the MSVC default, note
that this repo ships a `.cargo/config.toml` adding `-Clink-arg=/OPT:NOREF`. It
is load-bearing: without it `rust-lld` strips the wasmtime C API functions that
`tree-sitter` reaches through synthesized import thunks, and the daemon
segfaults on the first wasm grammar it loads. The config file explains the
mechanism and how to verify a binary; the short version is that the LNK4217
warnings you will see on every Windows build are expected and are *not* a
signal for that bug.

## The daemon

```sh
lore start
```

That backgrounds the daemon, logs to `<data-dir>\daemon.log`, and returns once
it is answering. It is idempotent — run it again and it reports the daemon it
found rather than starting a second one. It will also start your embedding
server first, if you have told it how (see below). `lore stop` shuts the
daemon down cleanly; `lore daemon` runs it in the foreground, which is what
you want when you are reading its logs live.

One daemon per machine owns the index — a single writer, enforced with a
kernel file lock. The data directory is `%LOCALAPPDATA%\lore\` on Windows
(`~/.local/share/lore/` under WSL), and it holds the SQLite database, the
config, the handshake file, and the logs.

`scripts/install-autostart.ps1` registers the daemon and the embedding server
as logon tasks, so the whole stack is up before the first agent session asks
for it.

## Registering a repository

```sh
cd path/to/your/repo
lore init          # write a .loreignore based on what the repo looks like
lore add           # register this directory with the daemon
lore status        # watch it index
```

A filesystem watcher picks up edits from there and re-indexes on a debounce.
`lore index <project>` forces a pass immediately — prefer the scoped form,
because bare `lore index` queues **every** registered project on the daemon.

`lore search "how does the scheduler handle cancellation"` queries the same
surface agents get, which makes it the fastest way to check what the index
knows.

`.loreignore` is sovereign over what gets indexed (D-0020). `lore init` writes
a sensible starting point; tune it when the index is much larger than the
repo's real source, or when search returns build output and vendored noise.

## Semantic search

Lore does not run an embedding model for you — you point it at one. Any
OpenAI-compatible endpoint works: `llama-server`, Ollama, TEI, vLLM. Write
`config.toml` in the daemon's data directory:

```toml
[embeddings]
endpoint = "http://127.0.0.1:8090/v1"
model = "qwen3-embedding-4b"
dimensions = 2560
```

Every key is optional and unknown keys are rejected rather than ignored — a
misspelled `endpoint` fails loudly instead of presenting as "embeddings
mysteriously never turned on". `scripts/serve-embeddings-vllm.ps1` is the
launcher used on the development machine.

Add `start_command` and `lore start` will launch that server for you when the
endpoint is not already answering:

```toml
[embeddings]
endpoint = "http://127.0.0.1:8000/v1"
start_command = [
  "wsl.exe", "-d", "Ubuntu", "-e", "bash", "-lc",
  "exec bash /mnt/c/path/to/lore/scripts/serve-embeddings-vllm.sh",
]
```

It is argv rather than a shell line, because the interesting case is exactly
where nested quoting goes wrong. `lore start` probes the endpoint first and
only runs the command if nothing answers, then waits up to three minutes for
it to come up, logging to `<data-dir>\embed.log`.

Nothing supervises the server after that. `lore stop` does not stop it, a
server that dies is not restarted, and a server that never comes up is
reported rather than treated as an error — an absent or unhealthy endpoint is
a state the daemon is built to degrade through (search goes lexical-only and
says so), which is what lets this be a launcher instead of a process manager.
The daemon re-probes on its own, so a server that arrives late is picked up
within a minute either way.

Changing the model or its dimensions re-embeds the index.

## Wiring an agent to it

Lore speaks MCP through the `lore-mcp` binary, which offers four tools:
`bundle` (ask a question, get a finished evidence bundle), `search` (steer the
retrieval yourself), `expand` (pull more context around a result), and
`status`. Run this from the repo you want indexed:

```console
$ lore setup mcp
project scope   C:\path\to\repo\.mcp.json
  registered lore -> C:\path\to\lore-mcp.exe
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
in.

### The agent-side skills

`lore setup claude-code` installs three skills, and bare `lore setup` reports
on all of it without writing anything:

- **`lore-search`** — the method for using the index: bundle first, search to
  steer, read a hit's provenance and authority, expand before quoting, when to
  fall back to grep.
- **`lore-ignore`** — the method for tuning what a repo indexes.
- **`lore-vault`** — the method for standing up a `lore-v1` design vault in a
  repo that has none.

The skills exist because tool descriptions cannot carry a procedure.
Benchmarking found that agents told nothing about Lore never reach for it, and
that agents told only that it exists still re-derive every hit by hand — the
steering is what turns a returned result into a saved read. `lore setup`
treats the host as the extension axis: it ships prompt assets for the hosts it
knows about and never edits a repo's `CLAUDE.md` or `AGENTS.md`.
