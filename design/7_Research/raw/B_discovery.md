# Brief B — Discovery Sweep

**Scope.** Web sweep completed 2026-08-14, using primary repository pages and their README/file trees, issue counts, commit history, release notes, and license metadata. The known shortlist was treated as already covered; the candidates below are additions or materially different architectural options. “Windows” means Windows-native is plausible without WSL; a missing Windows claim is recorded as uncertainty rather than treated as support. Unity-specific Roslyn/editor semantics were not claimed unless the project documents them.

## 1. Distinct credible candidates

### [Brainwires/project-rag](https://github.com/Brainwires/project-rag)

- Rust MCP daemon: local FastEmbed embeddings + LanceDB (default) or optional Qdrant, BM25/Tantivy, Tree-sitter chunks, Git-history search, and lightweight definition/reference/call-graph tools in one service.
- The README lists C# among the AST-chunked languages, alongside the major agent languages; it is therefore a plausible Unity source/docs index, though not a Unity-aware analyzer.
- Embeddings are fully local through fastembed-rs/ONNX; the default `all-MiniLM-L6-v2` is modest enough for an RTX 5090 and can fall back to CPU.
- Windows is plausible: the project documents Windows MCP configuration and a native Rust binary, but its build prerequisite examples are Linux-centric (`apt`/protobuf), so a prebuilt Windows release or local toolchain needs checking.
- The important state-management idea is explicit filesystem cross-process locking plus concurrent-access protection; that directly addresses the “one owner of index state” requirement.
- 92 commits, three open pull requests, zero open issues on the checked page; MIT licensed. It earns a place because it combines hybrid retrieval, C# coverage, Git context, relations, and ownership guards rather than being only a vector demo.

### [teknologika/mcp-codebase-search](https://github.com/teknologika/mcp-codebase-search)

- TypeScript/Node MCP service with Tree-sitter-aware semantic chunks, local Hugging Face embeddings, LanceDB vectors, incremental hash-based rescans, staleness warnings, filtering, and a small web management UI.
- C# is explicitly supported at class/method/property/interface chunk level; that is a better stated C# story than most local RAG servers, while Unity project files remain ordinary text/config inputs.
- The embedding path is local and no-cloud by default; the embedded LanceDB design avoids a separate vector service and leaves GPU acceleration to the underlying local model stack.
- The README documents Windows Claude Desktop configuration via `%APPDATA%`, and Node plus LanceDB is a reasonable Windows-native route. It does not document a Windows-specific GPU path or multi-process lock protocol.
- Version history was at 0.1.20, with recent language/rescan fixes and a visible changelog; 51 repository commits were visible. MIT licensed.
- It earns a place as the most directly Windows/C#-oriented lightweight adoption candidate, but the thin-server/embedded-store ownership model should be tested under multiple simultaneous clients before it becomes the canonical index owner.

### [Muvon/octocode](https://github.com/Muvon/octocode)

- Rust MCP/CLI that builds a Tree-sitter symbol graph, hybrid semantic+BM25 search, relationship-aware GraphRAG, structural queries, and optional LSP tools for definitions/references/hover/completions.
- Its documented language table is broad but does not list C# among the 16 full AST languages; the graph/LSP value is strong, but Unity/C# should be treated as an external-language-server validation task, not a solved feature.
- The benchmark is unusually useful: it reports a local FastEmbed/Jina-code configuration and shows keyword-heavy RRF beating dense-only retrieval on its own 127-query corpus. The default setup, however, asks for Voyage API credentials; fully local embeddings are documented as platform-dependent and primarily supported by local model builds.
- Windows is explicitly in the universal installer target, with Cargo/releases as alternatives. The local-embedding story on Windows is weaker than the cloud-provider story, so it is a good graph idea-source but not an automatic privacy win.
- 447 stars, three issues, zero PRs, Apache-2.0. The repository has a real benchmark, changelog, installer, MCP server, and LSP integration, which separates it from listicle-only GraphRAG claims.
- It earns a place versus the shortlist for its measured retrieval work and LSP bridge; it is a candidate to borrow from or adopt only after proving a Windows-local model configuration and C# language-server path.

### [codegraph-ai/CodeGraph](https://github.com/codegraph-ai/CodeGraph)

- Native Rust server with a single persistent RocksDB-backed graph, 42 MCP tools, BM25 plus HNSW embeddings, Tree-sitter parsing, incremental hashing, PR/impact analysis, docs verification, and a separate project-memory tool surface.
- C# is one of 38 parsed languages and ASP.NET handlers are recognized; that is the strongest stated general C# graph story in this sweep, although Unity component/scene semantics are not advertised.
- BGE/Jina/Granite ONNX models and a model2vec “static” option run locally; full-body embeddings are stored beside the graph, so retrieval does not depend on a hosted API. A local RTX 5090 should be ample, subject to the documented RAM gate.
- Windows x64 is a first-class downloaded engine target (ARM runs the x64 build under emulation). The one Rust binary serves MCP and LSP and persists graph/embeddings centrally, matching the authoritative-owner requirement unusually well.
- 148 commits, two issues, two pull requests, 61 stars, Apache-2.0; the project says it is maintained by one developer, so bus-factor risk is real.
- It earns a place because it is the closest discovered “adopt one service” answer: code graph, local retrieval, LSP surfaces, persistent docs, and memory in one process, rather than another standalone semantic-search adapter.

### [srclight/srclight](https://github.com/srclight/srclight)

- Python MCP/CLI built around one SQLite database per repo: FTS5/trigram search, Tree-sitter symbols, call/dependency/impact graphs, git blame/hotspots, build-system analysis, document extraction, and multi-repo workspaces.
- The README lists Python, C/C++, C#, JavaScript/TypeScript, PHP, Dart, Swift, Kotlin, Java, and Go; C# is therefore in the intended parser set, but no Unity-specific handling is claimed.
- It is fully offline with Ollama local embeddings, optional GPU-accelerated vector search, and keyword/semantic RRF. This is a good fit for an RTX 5090 if the Windows Python/CUDA stack cooperates.
- The repository page does not document a Windows install path; Python is portable, but optional Poppler/OCR/CUDA pieces and Ollama integration make native Windows verification mandatory. The README does document the single-file persistence and automatic watcher model, but not a lock protocol.
- 99 commits, two issues, two pull requests, MIT. The file tree includes tests, CI, PyInstaller packaging, docs, and a server manifest; that is meaningful maintenance evidence despite the small community.
- It earns a place for unusually broad “code intelligence plus Git/build intelligence” in a single local SQLite service; it is a strong feature reference and a possible Windows adoption candidate after a concurrency smoke test.

### [sdsrss/code-graph-mcp](https://github.com/sdsrss/code-graph-mcp)

- Rust MCP server using Tree-sitter, SQLite FTS5/sqlite-vec, Merkle incremental indexing, hybrid BM25+vector search, recursive caller/callee traversal, dead-code and impact analysis, context compression, and a file watcher.
- C# is explicitly “smoke-tested” for calls/imports/inheritance, not full-strength like the project’s TypeScript/Go/Python tiers; that distinction matters for Unity code.
- Optional Candle embeddings are local and feature-gated, while the persistent index lives in `.code-graph/index.db`; model artifacts can be installed under `%LOCALAPPDATA%` on Windows.
- Windows is supported in the installation guidance, though the manual model path requires POSIX-style `tar -C` syntax and a C compiler/Rust toolchain for source builds. It is native-plausible, not frictionless.
- The checked page showed 1,039 commits, one issue, no pull requests, 61 stars, and MIT metadata. The large commit count and tests/benchmarks/npm packaging are stronger evidence than the star count.
- It earns a place as a focused graph/retrieval service with the clearest SQLite single-owner shape and C# caveat; it is a useful alternative to a much larger integrated engine if the team wants to keep memory separate.

### [Neverdecel/CodeRAG](https://github.com/Neverdecel/CodeRAG)

- Python engine exposed as CLI, library, REST service, web UI, or MCP: symbol-aware chunks, FastEmbed/ONNX, LanceDB, BM25+dense RRF, optional local cross-encoder reranking, exact file search, line citations, and a debounced watcher.
- Symbol-level parsing is documented for Python, JS/TS, Go, Rust, and Java; C# is searchable through fallback windows rather than a claimed Tree-sitter symbol pipeline. That is acceptable for a docs/code baseline but weak for Unity navigation.
- Local embeddings are the default (`bge-small` via fastembed); optional local Ollama/LM Studio/vLLM/LocalAI answers are separate. The one embedded store contains chunks, hashes, vectors, and BM25 state, reducing cache divergence.
- Windows-native operation is plausible through Python, LanceDB, ONNX, and the documented MCP surfaces, but the installation examples lean Unix (`pipx`, `sudo`, shell exports) and no Windows-specific GPU or lock story is promised.
- 242 stars, 38 forks, Apache-2.0; the repo includes eval harnesses, tests, docs, and a security warning for the unauthenticated local HTTP surface.
- It earns a place as the best measured, composable retrieval engine in the sweep, not as the C# graph answer. It is a strong reuse candidate if a future engine wants a clean search core with thin MCP/HTTP adapters.

### [cortexkit/aft](https://github.com/cortexkit/aft)

- Rust “sensorimotor” core for coding agents: Tree-sitter outlines/zoom, symbol-aware edits, hybrid semantic+lexical search, call graphs/impact, LSP, trigram indexes, watchers, backups, and persistent background tasks.
- C# is listed with outline/edit/AST/semantic/import coverage, making it one of the few candidates that explicitly covers the requested language across more than search alone.
- Embedding backends can be local, Ollama, or OpenAI-compatible; the project’s local model choice and GPU path are configurable, so an RTX 5090 is useful but not required.
- Its architecture is especially relevant: one persistent Rust process per project root is shared across sessions through thin adapters, with indexes and LSP servers persisted under a shared storage root. That is almost exactly the desired ownership model.
- The README only documents OpenCode and Pi adapters today and does not establish a Windows platform matrix; npm platform packages and Rust make Windows plausible, but MCP/Windows integration is not yet an adoption-safe claim.
- The page showed 249 stars, 29 forks, MIT, and an active monorepo with tests/CI; benchmarks were explicitly “in progress,” so performance claims are not yet independently substantiated.
- It earns a place as a strong component/architecture reference and possible future adapter target, not as a drop-in replacement for a generic MCP client today.

## 2. Memory-focused systems

The shortlist’s biggest blind spot is not another vector index; it is durable, inspectable project knowledge with lifecycle and provenance. These are the strongest additions.

### [ipiton/agent-memory-mcp](https://github.com/ipiton/agent-memory-mcp)

- Go service combining typed episodic/semantic/procedural/working memory, repo-aware file tools, document/RAG indexing, session-end capture, session-start summaries, and a shared-service HTTP mode over a local SQLite core.
- It explicitly supports decisions, ADRs, RFCs, runbooks, changelogs, incidents, and postmortems; lifecycle states include active, outdated, superseded, and canonical, with freshness, owner, confidence, verification, conflict, and drift signals.
- Hybrid retrieval combines embeddings with keyword/BM25, recency, source weighting, and trust metadata. Local-only mode uses Ollama or llama.cpp; hosted providers are optional.
- Go makes Windows compilation plausible, but the checked installation path emphasizes Homebrew/Linux/macOS releases and does not list a Windows binary. Build-from-source or a Windows release must be verified.
- The repo showed 137 commits, zero issues, one pull request, MIT, GoReleaser, docs, security/threat-model files, and deployment material. This is the most complete match for the requested “decision memory with stewardship,” not merely a note store.

### [carloluisito/mindkeg-mcp](https://github.com/carloluisito/mindkeg-mcp)

- TypeScript MCP server storing atomic, curated learnings—architecture, conventions, debugging, gotchas, dependencies, and decisions—in SQLite with stdio and HTTP+SSE transports.
- Its deliberately small retrieval unit (one learning, max 500 characters) avoids arbitrary document chunking; repository/workspace/global scopes and tags/groups support project memory without forcing code into the vector index.
- FastEmbed is local/ONNX (`bge-small-en-v1.5`), with OpenAI and FTS5-only fallback options. Conflict detection, staleness scoring, duplicate merging, typed relations, import/export, encryption, auditing, and TTL are unusually practical memory features.
- Node/SQLite is Windows-plausible, but the README does not provide an explicit Windows installer or a documented cross-process lock/daemon ownership guarantee; use one long-lived MCP process as the owner.
- 51 commits, MIT, tests, CI, changelog, security and operational docs; only 10 stars, so maintenance/community risk is higher than functionality risk.
- It is worth adopting as a small, inspectable memory sidecar when the team wants atomic learnings and a low operational footprint; it complements a code graph rather than duplicating one.

### [jeffpierce/memory-palace](https://github.com/jeffpierce/memory-palace)

- Python MCP memory layer for facts, decisions, insights, and context, with semantic search, typed/weighted/directional knowledge-graph edges, auto-linking, centrality-weighted retrieval, and multi-project sharing.
- Local embeddings and local LLM support run through Ollama; the setup wizard detects GPU/model capacity, and the repository includes `install.bat` and `install.ps1`, making Windows-native intent explicit.
- The architecture is a persistent knowledge graph rather than a code index, so it supplies associative context and provenance-like links but not callers/callees, ASTs, or C# semantics.
- It has 86 commits, 46 stars, 10 forks, MIT, tests, docs, extensions, and a Windows installer. The smaller community and lack of a clearly documented decision-state workflow make it a promising idea-source rather than a default production dependency.
- It earns a place because the brief specifically named Memory Palace and the current repository is real, local, cross-provider, and graph-backed—not the unverified vaporware name the shortlist implied.

### [dcostenco/prism-coder](https://github.com/dcostenco/prism-coder)

- TypeScript/npm MCP server for persistent coding-agent sessions, handoffs, open TODOs, local model routing, associative recall, drift detection, and project memory; it runs locally without an account and adds cloud sync/team features only as an optional paid tier.
- The project ships a local `prism-coder` model fleet via Ollama and connects Claude Code, Claude Desktop, Cursor, Gemini CLI, and Codex. It is more agent-lifecycle-oriented than a plain vector database.
- Windows is a documented target: the page mentions stabilized Windows CI and host registration for Claude Desktop on Windows/Linux (beta). Native inference still depends on the local Ollama/model install path.
- Apache-2.0 for the server, but the surrounding extension/web/cloud surfaces have separate licenses/terms; the repo explicitly changed from AGPL-3.0 to Apache-2.0 at v20, so pinning versions and reviewing the exact package is important.
- 155 stars and an active-looking 2026 repository make it credible, but the commercial/cloud coupling and solo-maintainer risk argue for an evaluation sandbox rather than making it the sole institutional memory owner.
- It earns a place as the best discovered option for automatic session continuity and drift-aware recall; it is complementary to a project decision ledger, not a replacement for one.

## 3. Rejected / SEO-noise list

- **[elastic/semantic-code-search-mcp-server](https://github.com/elastic/semantic-code-search-mcp-server)** — credible code search, but it requires a separately indexed Elasticsearch deployment and indexer; that is infrastructure-heavy and fails the simple, single-owner local adoption bar for this survey.
- **[yumeiriowl/repo-graphrag-mcp](https://github.com/yumeiriowl/repo-graphrag-mcp)** — real Tree-sitter/LightRAG code graph with C# support, but its quick start requires a chosen LLM provider and `uv`; it is not convincingly local end-to-end and is closer to an implementation-planning demo than a durable index daemon.
- **[FarhanAliRaza/claude-context-local](https://github.com/FarhanAliRaza/claude-context-local)** — technically relevant local EmbeddingGemma/FAISS project, but the README labels it beta and says installation is tested on Mac/Linux; GPL-3.0 and no Windows validation make it a reference, not a keeper.
- **[adam-hanna/semantic-search-mcp](https://github.com/adam-hanna/semantic-search-mcp)** — functional hybrid Tree-sitter/FTS5 local search, but only five stars, no stated C# chunking in the default list, and no Windows-specific evidence; the stronger CodeRAG/Project RAG candidates subsume the idea.
- **[Lordymine/codegraph](https://github.com/Lordymine/codegraph)** — interesting type-checker-accurate Go/TypeScript graph, but intentionally narrow, in-progress, cgo-dependent, and adjacent to the known DeusData/codebase-memory-mcp graph space; it does not solve C#/Unity.
- **[codegraphcontext/CGC-style projects](https://github.com/nahisaho/CodeGraphMCPServer)** — several similarly named AST-graph MCP projects are easy to conflate; the checked examples lack the maintenance, platform, or local-embedding evidence needed to outrank the graph keepers above.
- **[Palace hosted product](https://palacememory.com/)** — a commercial/self-hosted-memory product page with insufficient public repository evidence for this codebase-focused comparison; it is not the same evidence as the local [Memory Palace repository](https://github.com/jeffpierce/memory-palace).
- **Reddit-only “local RAG,” “GitCortex,” and “CodeGraph CLI” posts** — useful discovery leads, but without a stable primary repository, license, or verifiable history they remain claims/listicle-ware rather than candidates.
- **Aider repo map** — excellent embedded architecture/reference for tree-sitter symbol maps, but it is an agent-integrated component, not a persistent multi-client index service with an MCP/daemon ownership boundary; it belongs in the build-not-adopt toolbox.

## 4. Verdict

The sweep changes the calculus, but not by revealing a single perfect replacement. The known shortlist already spans the main retrieval and structural feature space: lexical/semantic code search, Tree-sitter chunking, persistent stores, AST graphs, and LSP-style navigation. The meaningful omission is durable project memory with lifecycle, provenance, staleness, conflict handling, and session-summary recall; `agent-memory-mcp` is the clearest current answer, with Mind Keg, Memory Palace, and Prism Coder offering different trade-offs.

For a Windows-native C#/Unity evaluation, the most credible two-service pilot is **CodeGraph + agent-memory-mcp**: CodeGraph supplies one persistent Rust owner, Windows x64, local embeddings, C# graph/LSP surfaces, and project docs; agent-memory-mcp supplies decisions/ADRs, session capture, temporal validity, stewardship, and trust-aware recall. If CodeGraph’s solo-maintainer risk or feature surface is too high, **Project RAG** is the practical retrieval fallback; if the team wants the smallest memory sidecar, use **Mind Keg**.

The discovery therefore argues against building a new vector/AST search engine from scratch. It does justify building a thin integration and governance layer: one process owns each index, adapters expose a stable MCP contract, C# language-server/Unity validation is explicit, memory entries have reviewable lifecycle state, and reindex/embedding migrations are auditable. A 2–3-service adoption path is now plausible, but no candidate removes the need for a Windows concurrency smoke test, Unity-specific C# validation, license pinning, and a decision about whether memory is project-scoped or shared across repositories.
