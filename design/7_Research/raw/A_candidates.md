# Brief A — Named-Candidate Survey

Date: 2026-08-14  
Scope: local code-context engines for Claude Code/Codex, with Windows 11, Unity/C#, local-only embeddings, and one authoritative owner of index state as hard adoption constraints.

## Executive frame

The commodity layer is already solved: Tree-sitter grammars, BM25, local Ollama/ONNX embeddings, content-hash incrementality, file watchers, MCP stdio, and SQLite-backed metadata appear repeatedly. The scarce layer is different: an authoritative cross-client owner, reliable Windows lifecycle behavior, and C# semantics that go beyond syntax trees. On the code checked here, `codebase-memory-mcp` is the closest fit to a daemon-owned graph/index, `opencode-codebase-index` is the cleanest forkable retrieval base, and `codesearch` has the strongest multi-repository daemon plus optional Roslyn/SCIP path. Serena is valuable as an LSP symbol sidecar, while CCE is an idea source only. Claude Context is a poor fit for the stated local-only, self-owned-storage target.

## `codesearch` — flupkede/codesearch

### 1. Verdict

**Component source / conditional adoption-grade.** The Rust service topology is unusually relevant: `codesearch serve` owns per-repository stores, file watchers, index tasks, cancellation, and write locks, while `codesearch mcp --mode client` is a thin stdio-to-HTTP proxy. It is Windows-aware, multi-repo, fully local for embeddings, and has a real C# semantic path through the separately bundled `scip-csharp` Roslyn helper. The reason I would not adopt the default path blindly is concrete: `src/chunker/tree_sitter.rs` still marks semantic Tree-sitter chunking as TODO and currently calls the line-window fallback. That leaves ordinary C# retrieval coarse unless the SCIP symbol path carries the task. Treat the service/process model and C# helper as high-value source, and validate the chunker before making it the primary Lore index.

### 2. Feature inventory

- Retrieval: Tantivy BM25 over content/signature/kind plus persisted vector search through arroy/LMDB; query parsing boosts signatures and defaults to conjunction behavior (`src/fts/tantivy_store.rs`).
- Chunking: parser and language registry are Tree-sitter-capable, but production `TreeSitterChunker::chunk_file` currently falls back to sliding line windows (`src/chunker/tree_sitter.rs`).
- Graph: no general code graph; C# SCIP symbol definitions/references and `find_impact` are the graph-like capability (`src/symbols/csharp.rs`).
- Memory: no durable agent/session memory system; metadata, groups, query cache, and repository registrations only.
- Daemon: one Tokio executable can run a persistent HTTP/MCP hub (`src/main.rs`, `src/serve/mod.rs`).
- Multi-client: thin stdio MCP clients forward to the hub; direct local/stdio modes also exist, so deployment must explicitly select service/client mode (`src/mcp/mod.rs`).
- Watching: debounced `notify` watcher plus a Git HEAD watcher; `.gitignore`, `.codesearchignore`, worktree `.git` files, UNC paths, and Windows path normalization are handled (`src/watch/mod.rs`).
- Incremental: content hashing, debounced changed-file indexing, Git worktree HEAD detection, and C# `--filter-project`/merge updates.
- Embeddings: local fastembed ONNX models, CPU execution provider by default; persisted arroy vectors in LMDB (`src/embed/embedder.rs`, `src/vectordb/store.rs`).
- Languages: Tree-Sitter grammars include C# and many mainstream languages; ordinary C# semantic references require the optional SCIP/Roslyn helper.

### 3. Architecture & process model

In service mode, `ServeState` keeps a `DashMap` of repository names to `Arc<SharedStores>`, and `IndexManager` owns the per-repository index/watch tasks. Repository state has explicit `Write`, `Warm`, `Readonly`, and `Conflicted` modes. A filesystem lock prevents a second writer; a second process becomes read-only or must use the HTTP hub. The implementation comments in `src/serve/mod.rs` document the previous detached-task failure that produced runaway CPU/disk work and the current cancellation/await-before-delete path. The MCP client mode deliberately carries no repository context: it forwards tool calls to the running server. A second thin client therefore shares the authoritative service. A second independently launched local indexer would not; configuration must make the hub the only index owner.

### 4. C# + Windows evidence

`src/file/language.rs` maps `.cs` to `Language::CSharp`, and `Cargo.toml` includes `tree-sitter-c-sharp`. More importantly, `src/symbols/csharp.rs` launches `scip-csharp`, which uses Roslyn to build a SCIP symbol-reference index for `.sln`/`.csproj` projects. Rebuilds produce definitions; lazy reference lookup populates a cache; after the debounce window, changed C# files use `--filter-project` and merge affected results. `README_CSharp.md` documents a Windows x86_64 `with-csharp` archive and a .NET 10 runtime requirement. The normal search/find/explore tools remain available if the helper is absent, but impact/reference analysis disappears.

Windows-specific code is not cosmetic: Tantivy retries file-lock failures and disables background merge behavior to avoid antivirus/Windows locking races; watchers normalize UNC and removal paths; Git worktree polling handles `.git` files and transient process-spawn errors. The web pulse is strong: the repository has no open issues and released `v1.2.10` on 2026-08-12 with an MSYS path-translation fix, conflicted-repository retry, and LMDB map auto-resize; the release page lists a Windows x86_64+C# artifact and the project page lists 21 releases. See the [repository](https://github.com/flupkede/codesearch), [latest release](https://github.com/flupkede/codesearch/releases/tag/v1.2.10), and [issue tracker](https://github.com/flupkede/codesearch/issues).

### 5. Embedding provider story

Local-only is a first-class path. `EmbeddingService` uses fastembed models cached on disk, with ONNX Runtime and a CPU execution provider by default; the source includes MiniLM, BGE, Nomic, Jina-code, multilingual, and other local model choices. The vector store is local LMDB/arroy. I found no cloud embedding adapter in the inspected Rust path, which is a positive fit for the brief but means a custom remote provider would require extension. The optional service/TUI remote features are not needed for local embeddings.

### 6. Maintainership pulse

Active and focused, but concentrated. The latest release was 2026-08-12 and the release notes credit `flupkede` alone; the GitHub page shows 34 stars, 14 forks, and no open issues. That is a better release cadence than broad community depth. The recent release sequence is meaningful engineering work—Windows path translation, LMDB sizing, auth/delegation, conflict recovery—not cosmetic versioning. Risk remains single-maintainer continuity and the need to independently test the unimplemented chunker path.

### 7. License

Apache-2.0 (`Cargo.toml`, repository `LICENSE`). Suitable for forking with notice/license preservation and explicit handling of any bundled helper licensing.

### 8. Standout ideas worth stealing

- `src/serve/mod.rs`: service-owned `SharedStores`, explicit repository lifecycle states, cancellation tokens, and awaited task shutdown.
- `src/mcp/mod.rs`: a genuinely thin MCP proxy whose clients do not each own index state.
- `src/symbols/csharp.rs`: isolate compiler-grade C# analysis behind a helper process and merge incremental project results instead of pretending Tree-sitter is a compiler.
- `src/watch/mod.rs`: Git HEAD polling for worktrees where `.git` is a file, plus debounce/filter logic for Windows.
- `src/fts/tantivy_store.rs`: deliberately avoid background merge behavior that is hostile to Windows file locking.

### 9. Disqualifiers / risks

- The advertised Tree-sitter AST chunking is not yet the actual chunking implementation; `src/chunker/tree_sitter.rs` explicitly falls back to line windows.
- High-quality C# references depend on shipping, locating, and running an external .NET 10/SCIP helper.
- A direct/local MCP process can still be misconfigured beside the service and contend for state; the thin client deployment is mandatory for the one-owner rule.
- `chunks_for_file` in `src/vectordb/store.rs` has an acknowledged linear-scan TODO for large repositories.
- One dominant maintainer means a fork may need to own release/build artifacts, especially the Windows C# bundle.

## `codebase-memory-mcp` — DeusData/codebase-memory-mcp

### 1. Verdict

**Adopt-grade for a structural/graph layer, with retrieval-quality qualification required.** This is the strongest match for the hard process constraint: a native C coordination daemon, OS admission barrier, shared watcher/indexing ownership, supervised worker subprocesses, and SQLite persistence. It is native on Windows and does not need WSL or a cloud service. C# support is materially stronger than a grammar-only claim: `internal/cbm/lsp/cs_lsp.c` implements an in-process “C# Light Semantic Pass” with Roslyn-inspired binding, namespace/using/alias handling, and type resolution. The qualification is important: it is not the real Roslyn compiler or an OmniSharp server, and the open issue queue shows continuing parser/graph and Windows defects. I would run the user’s existing deployment through a C#/Unity fixture and compare graph precision before replacing it.

### 2. Feature inventory

- Retrieval: SQLite FTS5 contentless index with BM25 over name, qualified name, label, and path; graph queries and 14–15 MCP graph tools.
- Chunking: vendored Tree-sitter grammars and declaration/extractor passes; graph-oriented nodes rather than only token windows.
- Graph: persistent SQLite nodes/edges, qualified-name lookup, imports/calls/definitions, coverage metadata, Cypher-like/query-graph tooling, similarity edges.
- Memory: graph metadata and project/index coverage; no CCE-style agent-session decision memory.
- Daemon: native coordination daemon shared by clients and owning watchers/indexing/UI.
- Multi-client: Claude, Codex, OpenCode, and other sessions register with the same daemon; admission rejects executable/version/cache-root mismatches.
- Watching: Git HEAD plus dirty-state signature polling, adaptive interval, retry after busy/failed runs; non-Git projects skip Git polling.
- Incremental: `pipeline_incremental.c`, file hashes, successful baseline commit only after completed reindex.
- Embeddings: algorithmic local code vectors—TF-IDF, random indexing, API/type/decorator signatures, graph diffusion, MinHash/LSH similarity; vendored 40,856-token, 768-dimension int8 Nomic code vectors.
- Languages: broad Tree-Sitter language table; C# has a dedicated light semantic pass and tests, with no external C# LSP dependency in the core path.

### 3. Architecture & process model

The architecture is deliberately admission-first. `src/daemon/daemon.c` maintains clients, jobs, watches, subscriptions, and mutex-protected coordination state; `project_lock.c` and the lock registry enforce project ownership. `src/mcp/mcp.c` is a single-threaded stdio JSON-RPC dispatcher. Expensive indexing is launched under `index_supervisor`, which isolates pathological files that could crash or hang the main process; parallel pipeline workers remain inside the supervised, daemon-owned operation. The watcher and index baselines belong to the daemon, not to every client. A second client registers with the same daemon and consumes the shared state. A second incompatible daemon is rejected before it can do work. This directly addresses the brief’s failure mode.

### 4. C# + Windows evidence

`internal/cbm/lsp/cs_lsp.c` is the key evidence: it recognizes C# Tree-Sitter nodes and performs primitive aliases, namespace and `using` resolution, aliases, enclosing-type lookup, and registry-backed symbol binding. The associated tests include `tests/test_cs_lsp.c` and reproducer/invariant tests under `tests/repro/`. It is much more than a grammar-only extractor, but its “Light Semantic Pass” label is also an honest limitation: Unity project/build semantics will not equal Roslyn for every generic, generated, conditional, or package reference case.

Windows support is built into the C path: `CreateProcessW`, bounded Git supervision, wide-character path handling, native Windows binaries/install scripts, and smoke/soak testing are present. The web pulse shows active releases and a large but visible Windows hardening queue. Open issues include persistence silently ignored on Windows (#402), CLI path handling (#431), non-ASCII-path parsing (#636/#700), MCP/stdio hangs, and query crashes; the project’s Windows architecture issue says many earlier path bugs were fixed but reporter confirmation remains outstanding for mapped/SMB cases. See the [repository](https://github.com/DeusData/codebase-memory-mcp), [issues](https://github.com/DeusData/codebase-memory-mcp/issues), [Windows platform task](https://github.com/DeusData/codebase-memory-mcp/issues/394), and [releases](https://github.com/DeusData/codebase-memory-mcp/releases).

### 5. Embedding provider story

It is fully local, but not conventionally pluggable. The semantic subsystem computes TF-IDF/random-indexing/signature features and graph diffusion in C, with a vendored fixed Nomic code-vector table. There is no cloud API key and no network embedding dependency. The tradeoff is provider rigidity: replacing the model means changing the native semantic subsystem rather than configuring an embedding-provider interface. That is acceptable for a self-contained graph engine but less convenient if Lore wants interchangeable models.

### 6. Maintainership pulse

Very active, with a substantial contributor/community surface and frequent release engineering. The release page describes native Windows artifacts, wide-path fixes, smoke/soak gates, crash recovery, and hundreds of tests; the issue tracker shows 41 issues, 10 pull requests, and 325 forks at the checked snapshot. The same breadth means many live bugs: C/C++ crashes, graph edge gaps, Windows path/stdio/persistence issues, and CLI integration regressions. This is active maintenance, not low risk.

### 7. License

MIT (`LICENSE`, copyright DeusData 2025). Straightforward for adoption or fork, subject to vendored grammar/model notices.

### 8. Standout ideas worth stealing

- `src/daemon/daemon.c`: one account-wide coordinator with client registration, shared watches, subscriptions, and job ownership.
- `src/daemon/project_lock.c`: admission before work, rather than letting several agents “eventually converge.”
- `src/mcp/index_supervisor.h`: isolate crash/hang-prone indexing from the MCP request process.
- `src/pipeline/pipeline_incremental.c`: commit the Git/file baseline only after a successful reindex, so failed work does not become falsely current.
- `src/store/store.c` plus `src/store/graph_buffer.c`: batch an in-memory graph with O(1) qualified-name dedup before durable SQLite publication.
- `src/store/store.c`: explicit `lsp_surface` and `index_coverage` tables are useful for telling an agent what the index can and cannot prove.

### 9. Disqualifiers / risks

- The C# resolver is “light,” not compiler-grade Roslyn; validate Unity generics, asmdefs, packages, generated code, and conditional compilation.
- The current issue queue includes Windows correctness and persistence failures, so native Windows packaging is not proof of operational reliability.
- The semantic-vector system is fixed and algorithmic; it is not a drop-in local embedding-provider abstraction.
- Graph breadth can be more valuable than retrieval recall, but the open missing-edge issues show that graph completeness is not guaranteed.
- Shared daemon admission is powerful but creates a version/cache-root ABI that every client must respect.

## `code-context-engine` — elara-labs/code-context-engine

### 1. Verdict

**Ideas only; do not adopt as an index owner.** CCE has some of the best agent-facing ideas in this set: explicit decisions, code-area routing memories, session recall, context compression, semantic AST chunks, local FastEmbed/Ollama, and a useful hybrid memory database. It also contains a genuinely good Windows workaround for FastEmbed multiprocessing. But the concrete process model violates the brief: each MCP/CLI process can own a watcher and call the indexing pipeline, while `_PIPELINE_LOCKS` is only a Python-process-local lock. Multiple clients can write the same vector/FTS/graph/SQLite state, and watcher-triggered writes can wake other watchers. The current GitHub issue “36 detached indexers, ~66GB RAM, host unusable” is a direct confirmation of the reference-only diagnosis. Mine the memory lifecycle and routing concepts; do not fork the ownership model.

### 2. Feature inventory

- Retrieval: hybrid vector, FTS, and graph retrieval; RRF-style session-memory fusion; `context_search`, expansion, related context, and low-token compression.
- Chunking: real Tree-Sitter semantic chunks for C# methods/classes/structs/interfaces/records/enums and `using_directive`, with fallback.
- Graph: local backend graph tables/edges, related-context traversal, file/chunk relationships; not a compiler-grade semantic graph.
- Memory: SQLite `memory.db` v3 with sessions, prompts, events, summaries, decisions, code areas, pending compression, FTS5, and sqlite-vec.
- Daemon: no authoritative index daemon; loopback hook server is an auxiliary service inside the serve process.
- Multi-client: MCP/CLI integrations exist, but each independently launched process can create a pipeline/watcher.
- Watching: watchdog daemon thread with debounce; read-only event filtering was added after an orphan/forkserver failure.
- Incremental: manifest/file hashes, batch streaming, per-file changes, FTS/vector/graph ingestion.
- Embeddings: FastEmbed local ONNX and local Ollama REST; protocol-style backend selection and Windows multiprocessing disablement.
- Languages: Python, JS/TS/TSX, PHP, Go, Rust, Java, C#, with C# AST node lists in `indexer/chunker.py`.

### 3. Architecture & process model

`indexer/pipeline.py` exposes shared indexing code to MCP and CLI, but `_PIPELINE_LOCKS` is a dictionary keyed by project inside one process. It is not a file lock, named mutex, socket lease, or daemon admission barrier. `watcher.py` runs a watchdog observer thread and invokes async reindex callbacks; every process that starts serving can therefore watch and enqueue work. `memory/hook_server.py` adds a loopback aiohttp sidecar and writes project `serve.port` files; stale-port recovery exists after hard kills, but it does not make index state single-owner. Two clients can concurrently mutate SQLite/FTS/vector/graph state and can induce cascades through each other’s filesystem events. That is the exact process-model disqualifier.

### 4. C# + Windows evidence

The C# chunker is real: `_FUNCTION_TYPES` includes `method_declaration` and `local_function_statement`; `_CLASS_TYPES` includes class/struct/interface/record/enum; `_IMPORT_TYPES` includes `using_directive`. That is adequate semantic chunking, not merely extension recognition. There is no equivalent Roslyn project binding in the inspected path.

The code includes a Windows-specific FastEmbed decision: `_resolve_parallel()` returns no multiprocessing parallelism because DLL-handle inheritance caused `ACCESS_VIOLATION`. The watcher fix for issue #66 filters read-only filesystem events after `cce serve` could spawn an orphaned forkserver/pool. The current web issue list is worse for adoption: open #159 reports 36 detached indexers and approximately 66 GB RAM, while #61 tracks low recall in large monorepos and #54 asks for regular index refresh. See the [repository](https://github.com/elara-labs/code-context-engine), [current issues](https://github.com/elara-labs/code-context-engine/issues), [multi-indexer issue #159](https://github.com/elara-labs/code-context-engine/issues/159), and [large-repo scope issue #61](https://github.com/elara-labs/code-context-engine/issues/61).

### 5. Embedding provider story

Good local story and good extension seam: FastEmbed local ONNX is the primary backend, Ollama is another local backend, and `CCE_EMBED_BACKEND` selects behavior. Persistent model cache and the Windows parallelism workaround are practical. The local option is real, but provider selection does not repair the cross-process index ownership problem.

### 6. Maintainership pulse

Active but issue-driven and still operationally immature for this use. The GitHub snapshot shows 14 issues and 2 pull requests, with open requests for scoped retrieval, benchmark correction, regular refresh, embedding drift detection, and pre-built packs. The newest issue is not a corner case; it describes the central concurrency failure at production scale. That makes the project valuable as a design notebook but not a safe base for a persistent Windows service.

### 7. License

MIT (`pyproject.toml`/repository license metadata).

### 8. Standout ideas worth stealing

- `memory/db.py`: a durable, local schema for sessions, turn summaries, decisions, code areas, pending compression, and FTS/vector search.
- `memory/decision_extractor.py`: zero-LLM heuristic extraction of durable decisions from agent text.
- `mcp_server.py` and `memory/hooks.py`: session start/capture, touched-file recall, and dual-write migration ideas.
- `indexer/chunker.py`: language-specific semantic-node inventories with graceful fallback.
- `indexer/pipeline.py`: separate vector, FTS, and graph ingestion streams plus manifest-based reuse—retain the dataflow, replace the ownership boundary.

### 9. Disqualifiers / risks

- No cross-process index lock or single daemon owner; the local asyncio lock is insufficient.
- Watcher events can create reindex cascades across independent client processes.
- Loopback hook servers and port files multiply lifecycle state and can become stale after hard termination.
- The current public issue #159 matches the brief’s cited runaway-indexing failure mode.
- C# is syntax-semantic chunking, not compiler/project semantic analysis.

## `opencode-codebase-index` — Helweg/open-codebase-index

### 1. Verdict

**Adopt-grade as a focused local retrieval base, with configuration and C# caveats.** This is the best fork/build base for Lore’s hybrid retrieval surface: native Rust parsing and usearch, SQLite metadata, BM25, branch-aware retrieval, call-graph/context tools, low-token context packs, and a carefully designed cross-process filesystem lease. It can be fully local with Ollama. Its one-owner story is a lock rather than a resident daemon: every host can still have a watcher, but only the lease holder mutates the index, and tests cover multiprocess contention. C# parsing and symbol extraction are present; the call-graph language set excludes C#, so do not infer Unity call-graph coverage from the parser. Use it if Lore wants a retrieval engine and can choose one host or enforce lock-mediated background indexing.

### 2. Feature inventory

- Retrieval: dense embeddings + BM25 hybrid, RRF/fusion, branch-aware filtering, reranking, exact/symbol lookup, context/peek tools.
- Chunking: Rust Tree-Sitter semantic nodes, leading comments, small-chunk merging, large-chunk splitting with overlap; embedding text includes language/type/path/purpose hints.
- Graph: definition lookup, dependency/call graph and graph paths; C# is parsed but excluded from `CALL_GRAPH_LANGUAGES` in `src/indexer/index.ts`.
- Memory: no general session/decision memory; configuration, benchmark artifacts, and durable index metadata.
- Daemon: no resident indexing daemon; host plugin/MCP process owns orchestration.
- Multi-client: host-neutral packages and MCP integrations; index lease serializes mutation across processes.
- Watching: file watcher plus Git-head watcher; background index requests are deduped and lock-gated.
- Incremental: file content hashes, branch-aware snapshots, changed-file reuse, recovery/publication of temporary artifacts.
- Embeddings: Ollama, GitHub Copilot, OpenAI, Google, and custom OpenAI-compatible endpoint; Ollama is explicit local mode.
- Languages: broad native parser set including C#, with text fallback for unsupported files.

### 3. Architecture & process model

`src/indexer/index-lock.ts` is the differentiator. It uses an atomic filesystem lease, `owner.json` with PID/host/start/op/token, liveness checks via `process.kill(pid, 0)`, dead-owner quarantine/recovery, temporary artifact ownership, and token-validated release. `withIndexLock` wraps initialize, index, force-index, clear, health-check, retry, and recovery operations. `src/watcher/index.ts` can request indexing from file and Git-head watchers, but the lock prevents concurrent state mutation. A second client can read or wait/retry; it cannot become a simultaneous writer. This is a strong safety boundary, though not the simpler operational model of a single resident daemon.

### 4. C# + Windows evidence

`native/src/parser.rs` maps `Language::CSharp` to `tree_sitter_c_sharp::LANGUAGE` for parsing and symbols. `native/src/chunker.rs` extracts semantic nodes and refines large chunks. `native/Cargo.toml` pins Tree-Sitter C# and uses bundled rusqlite. The native target configuration disables simsimd on Windows/MSVC because of `_mm512` compatibility, showing a concrete Windows build accommodation. The limit is call-graph scope: `CALL_GRAPH_LANGUAGES` in `src/indexer/index.ts` does not include `csharp`, so C# definitions/chunks are supported but C# caller/callee traversal is not proven.

The web pulse is unusually good for a small project: no open issues or pull requests were shown, and `v0.23.0` released on 2026-08-11 with watcher lifecycle shutdown, large-worktree startup, config reconciliation, snapshot hardening, MCP signal handling, and tests. The latest release credits Helweg and Nicolas-nwb, so activity is high but concentrated. See the [repository](https://github.com/Helweg/open-codebase-index), [latest release](https://github.com/Helweg/open-codebase-index/releases/tag/v0.23.0), and [release history](https://github.com/Helweg/open-codebase-index/releases).

### 5. Embedding provider story

Excellent for the brief if explicitly configured. Ollama with `nomic-embed-text` is documented as the simplest local option; provider selection also supports cloud and OpenAI-compatible endpoints. The default/auto order can fall through to cloud providers, so Lore must set `embeddingProvider: "ollama"`, pin model/dimension, and test startup with no cloud credentials. The provider interface is materially more pluggable than codebase-memory-mcp or codesearch.

### 6. Maintainership pulse

Very active immediately before the survey date: releases landed on August 2, 7, 9, and 11, 2026, with explicit benchmark, watcher, recovery, and shutdown work. The latest release has only two named contributors, and the public issue surface is empty, so the primary risk is concentration rather than visible neglect. The project’s recent focus on frozen retrieval benchmarks is a positive sign for evaluating Lore-quality recall rather than relying on demos.

### 7. License

MIT (repository license/package metadata; copyright Kenneth Helweg). Suitable for a fork with native dependency notices preserved.

### 8. Standout ideas worth stealing

- `src/indexer/index-lock.ts`: the strongest reusable lease/recovery implementation in the set; copy the owner token, stale-owner quarantine, and publication protocol.
- `tests/multiprocess-indexing.test.ts`: turn “one owner” into a regression test with multiple real processes, not an architectural aspiration.
- `src/indexer/index.ts` and `src/native/embedding.ts`: context-pack construction and embedding headers that carry symbol/type/path purpose into the vector representation.
- `src/watcher/index.ts`: separate file and Git-head triggers, background request dedupe, and host lifecycle shutdown.
- `ARCHITECTURE.md`: clean TS orchestration/Rust native boundary and explicit warning that SQLite/usearch cross-storage mutation is not atomic.

### 9. Disqualifiers / risks

- No resident daemon; duplicated host watchers still exist and depend on lock/retry discipline.
- C# call graph is not in the declared call-graph language set; parser support is not relationship support.
- SQLite metadata and usearch vector updates are separate stores, so a crash can leave cross-store publication inconsistent.
- Auto embedding selection can choose a cloud provider unless local Ollama is explicit.
- Small contributor core and a rapidly changing package surface increase fork-maintenance cost.

## `serena` — oraios/serena

### 1. Verdict

**Component source / adopt as a symbolic sidecar, not as Lore’s index.** Serena offers the strongest agent-facing symbolic interaction: find symbols, references, callers, definitions, hover, rename, and structured edits via LSP. It uses the official Roslyn Language Server for C# rather than pretending Tree-Sitter resolves Unity semantics. Project servers cache language-server-backed project instances and serialize tool execution. Markdown memories are simple, inspectable, and useful. It has no embedding/vector retrieval engine, and its process graph is language-server-centric rather than an authoritative multi-client index daemon. Pair it with a Lore retrieval index if desired.

### 2. Feature inventory

- Retrieval: LSP symbol search, references, definitions, hover, document symbols, pattern search, and file/text tools.
- Chunking: language-server document symbols and file-level scans; no vector-oriented chunk pipeline.
- Graph: on-demand LSP relationships—references, callers, definitions—not a persisted global graph.
- Memory: Markdown files under project/global `.serena` memory directories, with names/topics, references, maintenance template, read-only/ignored patterns, and rename propagation.
- Daemon: MCP server plus optional Flask project server; project server caches loaded projects and per-project load locks.
- Multi-client: MCP transport and query-project HTTP route; not a shared durable index owner across separately launched MCP processes.
- Watching: LSP/project lifecycle and file synchronization; no Lore-style embedding-index watcher.
- Incremental: language servers maintain their own workspace state; no local vector/FTS incremental store.
- Embeddings: none; local-only by definition, but there is no embedding provider to configure.
- Languages: many LSP-backed languages; C# uses Microsoft’s official Roslyn Language Server package.

### 3. Architecture & process model

`src/serena/project_server.py` caches `Project` objects by root and uses per-project load locks plus an active-project lock so concurrent HTTP requests cannot redirect the agent to the wrong project. `Project.create_language_server_manager()` then creates the language-server manager for the project. The MCP agent also uses a task executor to serialize tool calls. This is disciplined within a Serena process, but it does not establish a single OS-wide owner for multiple independently launched Serena MCP instances. The expensive state is the external LSP workspace, not a centralized persistent retrieval index.

### 4. C# + Windows evidence

`src/solidlsp/language_servers/csharp_language_server.py` documents and implements the official Roslyn Language Server from NuGet, with pinned Windows x64/ARM64 package variants, SHA-256 values, .NET 10 runtime handling, solution/project discovery, symbol-name normalization, and rich hover information. That is the best C# semantic evidence in the candidate set. It is also an external runtime lifecycle dependency, not an in-process Unity-aware index.

The web pulse is strong: the project page shows 13 releases, with `v1.5.3` on 2026-05-26, and a large fork/star base. Windows/C# issues are real: #513 reports Roslyn/.NET 9 project-loading timeouts over named pipes; #506 reports a Windows `nul` filename failure; the changelog records fixes for spaces in C# paths, Windows Claude Code startup, and C# initialization/reference timing. See the [repository](https://github.com/oraios/serena), [changelog](https://github.com/oraios/serena/blob/main/CHANGELOG.md), [C# timeout issue](https://github.com/oraios/serena/issues/513), and [Windows filename issue](https://github.com/oraios/serena/issues/506).

### 5. Embedding provider story

There is no embedding subsystem, local or cloud. This is a positive privacy property but means Serena cannot replace semantic retrieval. Lore could use Serena’s symbol tools after a vector/BM25 candidate set is selected, or use its LSP results as authoritative re-ranking/verification.

### 6. Maintainership pulse

Mature and active, with 13 releases, 72 issues, 32 pull requests, and a substantial fork base at the checked snapshot. Recent changelog entries show ongoing LSP, Windows, multi-project, JetBrains, and tool-surface work. The risk is dependency complexity and the normal variability of third-party language servers, not abandonment.

### 7. License

MIT (`LICENSE`, Oraios AI 2025).

### 8. Standout ideas worth stealing

- `project_server.py`: per-project load locks and active-project serialization for safe multi-project queries.
- `csharp_language_server.py`: pinned, hashed, platform-specific Roslyn package acquisition and symbol-name normalization with rich hover preservation.
- `memories/memory_manager.py`: human-readable Markdown memories, global/project scope, safe names, reference propagation, read-only/ignored policies, and an explicit memory-maintenance document.
- `tools/symbol_tools.py`: agent-friendly symbol-depth/body/children controls that keep answers within a token budget.

### 9. Disqualifiers / risks

- No vector/BM25 retrieval or persisted code graph; not a complete code-context engine.
- No cross-process authoritative owner for multiple independently launched MCP servers.
- Roslyn/.NET/BuildHost named-pipe startup and project-load behavior can fail on Windows, especially across SDK versions.
- External LSP packages and per-project language-server processes add substantial lifecycle/memory cost for Unity solutions.

## `claude-context` — zilliztech/claude-context

### 1. Verdict

**Skip for this adoption target; component source only for embedding/provider wiring.** It has an honest AST splitter, including C# methods/classes/interfaces/structs/enums, and supports local Ollama embeddings. However, its core storage is Milvus, with configuration defaults oriented to OpenAI plus Zilliz Cloud/Milvus address/token resolution. The inspected MCP/core path has no durable local SQLite/usearch alternative and no authoritative local index daemon. Even when embeddings are local, the vector database is still an external service or a separately operated Milvus instance. That violates the spirit of Windows-native, local-owned Lore infrastructure and creates an additional service lifecycle.

### 2. Feature inventory

- Retrieval: dense vector search in Milvus; repository docs/config expose hybrid BM25+dense mode, but the core path is Milvus-centric.
- Chunking: AST Tree-Sitter splitter for JS/TS/Python/Java/C++/Go/Rust/C#/Scala with character/line refinement and overlap; LangChain fallback.
- Graph: no durable code graph, call graph, or LSP symbol layer found in the inspected core/MCP source.
- Memory: codebase snapshot JSON with indexed/indexing/failed status; no session/decision memory.
- Daemon: MCP Node process and Milvus service; no single local index owner coordinating multiple MCP clients.
- Multi-client: MCP/VS Code/Chrome integrations, all sharing Milvus collections if configured identically.
- Watching: indexing/status orchestration exists, but no comparable authoritative local watcher/lease model was found in the inspected path.
- Incremental: snapshot state and codebase index metadata; exact local transactional ownership is externalized to Milvus.
- Embeddings: OpenAI, VoyageAI, Gemini, OpenRouter, or Ollama; local Ollama is supported.
- Languages: AST splitter includes C# plus a small set of other languages; unsupported languages use fallback splitting.

### 3. Architecture & process model

`packages/mcp/src/config.ts` defines embedding provider/model and Milvus address/token configuration. `packages/mcp/src/embedding.ts` constructs an embedding instance, while `MilvusVectorDatabase`/`MilvusRestfulVectorDatabase` create and load collections via gRPC/REST. The MCP process does not own the vector state: Milvus does. Multiple clients can share a collection only if collection naming, dimension, address, and lifecycle are coordinated externally. The project therefore shifts the authoritative-owner problem to an external database/service rather than solving it inside the local engine. No Windows-specific single-owner or file-lock path comparable to opencode-codebase-index was found.

### 4. C# + Windows evidence

`packages/core/src/splitter/ast-splitter.ts` loads `tree-sitter-c-sharp` and splits `method_declaration`, `class_declaration`, `interface_declaration`, `struct_declaration`, and `enum_declaration`. This is genuine AST chunking, but not C# semantic binding or project/reference analysis. The package is Node 20+/pnpm 10+, so it is plausibly runnable on Windows; the decisive dependency is still Milvus/Zilliz connectivity and service configuration.

The web pulse shows 78 issues and 59 pull requests but no GitHub Releases. Open/closed issue history includes Claude Code startup failure because Milvus address auto-resolution from token was broken or undocumented (#258, later closed by #310). The official environment-variable page still describes `MILVUS_TOKEN`/address auto-resolution and Ollama as optional. See the [repository](https://github.com/zilliztech/claude-context), [issues](https://github.com/zilliztech/claude-context/issues), [Milvus connection issue](https://github.com/zilliztech/claude-context/issues/258), and [environment configuration](https://github.com/zilliztech/claude-context/blob/master/docs/getting-started/environment-variables.md).

### 5. Embedding provider story

The provider interface is broad and pluggable. `createMcpConfig()` supports OpenAI, VoyageAI, Gemini, OpenRouter, and Ollama; `OllamaEmbedding` defaults to `http://127.0.0.1:11434`, with configurable model/dimension. Local embeddings therefore pass the narrow embedding test. They do not make the overall system local-only because the vector store remains Milvus, and the default provider is OpenAI unless explicitly set to Ollama.

### 6. Maintainership pulse

Community interest is high—913 forks and 78 issues/59 pull requests on the web snapshot—but the absence of GitHub Releases and the visible connection/configuration issue surface make operational maturity harder to assess. The project is clearly being developed, but its center of gravity is Zilliz/Milvus integration rather than a self-contained Windows service.

### 7. License

MIT (`LICENSE`, copyright Zilliz 2025).

### 8. Standout ideas worth stealing

- `packages/core/src/splitter/ast-splitter.ts`: compact AST-first splitter with language map, meaningful-node fallback, oversize refinement, and overlap.
- `packages/mcp/src/embedding.ts`: simple provider factory with a true local Ollama branch and dimension reporting.
- `packages/mcp/src/config.ts`: provider-specific environment selection and explicit dimension/model configuration.
- `packages/core/src/vectordb/milvus-restful-vectordb.ts`: REST fallback for environments where gRPC is unavailable, useful as an adapter pattern if Lore ever supports a remote backend.

### 9. Disqualifiers / risks

- Milvus/Zilliz is a core storage dependency; local Ollama alone does not make the system local-only.
- No authoritative local index owner or robust cross-process lease was found.
- C# is AST chunking, not Roslyn/LSP semantic analysis.
- No GitHub Releases at the checked date complicates binary/runtime reproducibility.
- Milvus address/token auto-resolution has a documented Claude Code startup failure history.

## Cross-cutting conclusions

### Commodity versus rare

Commodity: Tree-Sitter extension/language maps; BM25; vector search; Ollama or local ONNX; SHA/hash-based incremental indexing; debounced watchers; MCP stdio; SQLite metadata; AST splitting; “hybrid retrieval” branding.

Rare and strategically important: a real single-owner process boundary (`codebase-memory-mcp` daemon, `codesearch serve` hub, or opencode’s cross-process lease); compiler/project-level C# semantics (codesearch’s Roslyn/SCIP helper and Serena’s official Roslyn LSP; codebase-memory’s light resolver is promising but must be tested); worktree-aware indexing; explicit index coverage/health; crash/hang supervision; token-budgeted context packs; durable decisions/code-area/session memory; and tests that launch several actual client processes.

### Ranked shortlist

#### (a) Adoption shortlist

1. **codebase-memory-mcp** — best hard-filter fit: native Windows, local semantic subsystem, graph, and true shared daemon. Qualify with a Unity/C# fixture and a soak test because the public Windows/graph issue queue is active.
2. **opencode-codebase-index** — best local retrieval UX and fork base; set Ollama explicitly, rely on `index-lock.ts`, and accept that C# call graphs are not currently covered.
3. **codesearch** — best service/multi-repo architecture and strongest optional C# reference path; require a Lore-owned semantic chunker or verify that the fallback is acceptable.
4. **Serena** — adopt only beside one of the above as a Roslyn/LSP symbol verification and editing sidecar.
5. **Claude Context** — not recommended for the target because Milvus is a core service dependency.
6. **code-context-engine** — do not adopt as an index owner; use its memory ideas only.

#### (b) Best bases to fork/build on

1. **opencode-codebase-index** for hybrid retrieval, context packs, native parsing, provider pluggability, and the index lease.
2. **codesearch** for a persistent multi-repo hub, store lifecycle, worktrees, Windows locking, and the Roslyn/SCIP helper boundary.
3. **codebase-memory-mcp** for the coordination daemon, admission ABI, supervised indexing, graph buffer, and coverage model.
4. **Serena** for a separate LSP/Roslyn symbol service, not for vector storage.

#### (c) Best idea sources

1. **code-context-engine** — decisions, code areas, session recall, compression, and memory lifecycle.
2. **codebase-memory-mcp** — cross-client daemon admission, index coverage, supervised workers, and post-success baselines.
3. **opencode-codebase-index** — cross-process lease/recovery, branch-aware context, low-token context packs, and benchmarks.
4. **codesearch** — thin MCP proxy, service-owned stores, worktree watcher, Windows file-lock discipline, and external compiler helper.
5. **Serena** — markdown memory conventions, symbol-depth controls, and Roslyn-backed C# operations.
6. **Claude Context** — provider factory and AST splitter only.

### Practical Lore direction

The strongest architecture is a composition: use one authoritative local owner modeled on `codebase-memory-mcp` or `codesearch serve`; use `opencode-codebase-index`’s lease/publication and context-pack ideas; use Roslyn/Serena or the codesearch SCIP helper for C# verification; and add CCE’s memory database as a separate, explicitly owned store. Keep the first implementation boring on ownership: one daemon/lease, one watcher, one queue, one durable index state, and a coverage report that tells the agent when a result is syntax-only rather than compiler-verified.
