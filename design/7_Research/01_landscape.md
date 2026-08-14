# Candidate Landscape — Local Code Context + Project Memory

Survey date: 2026-08-14. Method: three gpt-5.6-luna research passes (source-level survey of 6 shallow clones; web discovery sweep; embeddings calibration), cross-verified by the orchestrator against local clones. Full evidence in `raw/`. Constraints applied: Windows-native, serious C#, local-only embeddings, single authoritative owner of index state.

## Serious candidates

### codesearch (flupkede) — best architecture, incomplete retrieval
The only candidate with the target topology actually implemented: `codesearch serve` owns per-repo stores, watchers, index tasks and write locks; `codesearch mcp --mode client` is a genuinely thin stdio→HTTP proxy carrying no index state. Repo lifecycle states (Write/Warm/Readonly/Conflicted), cancellation-aware shutdown, worktree HEAD polling, and real Windows hardening (antivirus-safe Tantivy merge behavior, UNC normalization). C# gets compiler-grade references via an optional Roslyn/SCIP helper (`scip-csharp`, needs .NET 10). Active: v1.2.10 on 2026-08-12, zero open issues, but effectively one maintainer.
**Verified flaw:** the advertised tree-sitter chunking is a literal `// TODO` — production chunking is sliding line windows. Embeddings are CPU fastembed with no custom-endpoint support, so the RTX 5090 is unreachable without extension. No memory system. Apache-2.0 → fork-friendly.

### codebase-memory-mcp (DeusData) — already installed; the graph daemon done right, retrieval done cheap
Strongest process discipline in the set: native C coordination daemon, client admission (rejects version/cache-root mismatches), project locks, crash-isolating index supervisor, incremental baselines committed only after successful reindex. C# is a dedicated "light semantic pass" (`cs_lsp.c` — namespace/using/alias binding, verified present), better than grammar-only but below Roslyn. **Why its search underwhelms:** the "semantic" vectors are algorithmic (TF-IDF, random indexing, vendored static int8 table) — no learned embedding model, and no way to plug one in. That is a plausible root cause for the "coherence" complaints. Active Windows bug queue (silent persistence failure #402, path/stdio issues). MIT.

### opencode-codebase-index (Helweg) — best retrieval core and fork base
Cleanest hybrid retrieval implementation: Rust tree-sitter chunking with comment attachment and merge/split policy, dense+BM25 RRF, reranking, branch-aware snapshots, low-token context packs. The cross-process filesystem lease (`index-lock.ts`: owner tokens, liveness checks, stale-owner quarantine, multi-process regression tests) is the strongest reusable "one writer" implementation surveyed. **Passes the GPU test:** custom OpenAI-compatible embedding endpoint supported. Verified gap: `CALL_GRAPH_LANGUAGES` excludes C# — parsing yes, call graph no. No daemon (lease-mediated, watcher per host), no memory. Very active (4 releases in Aug 2026), 2 contributors. MIT.

### serena (oraios) — the C# truth source, as a sidecar only
Uses the official Microsoft Roslyn Language Server (pinned, SHA-256-verified, per-platform) — the best C# semantic evidence in the set. Symbol tools with token-budget controls; simple human-readable Markdown memories. No vector/BM25 retrieval, no shared daemon, real Windows/.NET startup issues on record. Valuable paired with a retrieval index, or as the pattern reference for Roslyn acquisition. MIT, mature, active.

### CodeGraph (codegraph-ai) — deep-dived (raw/D_codegraph.md); adoption dead, valuable pattern source
Audit verdict 7/10: substantive engineering (~150k lines of real Rust, 2,168 tests, a golden-file JSON-RPC harness), but solo-authored in ~11 weeks (heavily AI-assisted) with three README claims failing audit: "HNSW" search is actually brute-force cosine over all vectors in RAM; "Windows first-class" is 3 `cfg(windows)` occurrences and zero CI; "38 languages" includes goldens that bless empty results. Root architectural flaw: the whole graph lives in RAM with RocksDB as a full snapshot rewritten every 15s, and counter-based node IDs churn on every re-parse — orphaning vectors and rotting memory links; its elaborate crash recovery treats symptoms of that design. `codegraph-csharp` is a *negative* reference (no fields/events/delegates/lambdas/local functions/preprocessor directives, no partial-class merging — `[SerializeField]` and `#if UNITY_EDITOR` invisible; no `Library/` exclusion). **Patterns worth copying** (file refs in raw report): daemon heartbeat + atomic write-then-rename single-writer handshake; `EmbedMode` tri-state debounce (sync on save / defer on change / skip under RAM pressure); Markdown heading-tree chunking with `heading_path` provenance; flag-not-block prompt-injection handling for doc chunks; `model_id_tag` persisted with vectors; stale-LOCK recovery via advisory-lock probing; the golden-file MCP test harness. Apache-2.0.

## Memory-focused systems

None integrate a Markdown design vault as the source of truth; all are DB-first. The gap between them and the Lexomancy vault process is the clearest "nobody built this" finding of the survey.

- **agent-memory-mcp** (ipiton, Go, MIT) — schema deep-dived (raw/D_codegraph.md part 2); the most mature memory data model found. Four orthogonal axes (cognitive type ⟂ engineering type ⟂ derived lifecycle ⟂ sediment/retention); `dead_end` as a first-class record type; **provenance defaults to untrusted and canonical promotion is gated on verification** (`ErrPromotionRequiresVerification`) — a structural defence against an agent laundering its own hallucination into canon; a human steward inbox with 10 typed finding kinds; structural (not similarity-based) contradiction detection; canonical-health scanning; documented ranking formulas. Gaps for Lore: no document anchoring, no bindingness axis, canon-as-a-DB-row. Lesson adopted: Lore's promotion gate should end in a **diff proposed against the Markdown vault**, not a flag flip. No Windows release; runtime not wanted — schema only.
- **mindkeg-mcp** (TS, MIT) — atomic ≤500-char curated learnings, conflict detection, staleness scoring, TTL, audit. Smallest sensible sidecar; tiny community.
- **memory-palace** (Python, MIT, Windows installers) — knowledge graph with centrality-weighted recall; no decision workflow; idea source.
- **prism-coder** (TS, Apache-2.0 v20+) — session continuity/handoffs/drift detection across Claude/Codex/Cursor; commercial cloud tier attached; sandbox-evaluate only.
- **CCE's memory schema** (reference) — the best *ideas* remain here: decisions, code-area routing memories, session summaries with promotion, zero-LLM decision extraction (`memory/decision_extractor.py`), pending-compression queues. Take the schema, leave the runtime.

## Useful components / secondary candidates

- **project-rag** (Rust, MIT) — hybrid BM25+LanceDB with fs cross-process locking; C# in AST set. Quiet project.
- **srclight** (Python, MIT) — SQLite-per-repo code+git+build intelligence, C# in parser set; no Windows docs.
- **octocode** (Rust, Apache-2.0) — has the rare honest retrieval benchmark (keyword-heavy RRF beats dense-only on its 127-query corpus); no C# AST support.
- **aft** (Rust, MIT) — persistent per-project-root process shared across sessions; C# outline/edit/AST coverage; no MCP adapter or Windows matrix yet. Architecture reference.
- **SylphxAI/coderag** — (worker B misattributed the owner) TF-IDF+vector hybrid, AST chunking, LanceDB; unverified further.

## Rejected (evidence in raw/B_discovery.md §3)

elastic/semantic-code-search-mcp-server (requires Elasticsearch deployment) · yumeiriowl/repo-graphrag-mcp (not local end-to-end) · FarhanAliRaza/claude-context-local (Mac/Linux beta, GPL-3.0) · adam-hanna/semantic-search-mcp (subsumed) · Lordymine/codegraph (narrow, in-progress) · CodeGraphMCPServer-class clones (no maintenance/platform evidence) · palacememory.com (no public repo evidence) · Reddit-only projects (no stable repos) · aider repo-map (component, not a service) · **zilliztech/claude-context** (Milvus is a required external service; default config funnels to OpenAI/Zilliz Cloud — fails local-only) · **elara-labs/code-context-engine** (verified: per-process asyncio locks, per-process watchers, no cross-process owner → runaway concurrent indexers, issue #159; reference source only).

## CCE post-mortem (why it destroyed the machine)

1. `_PIPELINE_LOCKS` is `dict[str, asyncio.Lock]` — process-local; two processes share nothing (verified in `indexer/pipeline.py:73`).
2. Every MCP/CLI process may create its own watchdog watcher and invoke the shared indexing pipeline.
3. Watcher-triggered index writes generate filesystem events that wake *other* processes' watchers → mutual re-indexing cascade.
4. Loopback hook servers + `serve.port` files add stale lifecycle state after hard kills.
5. Public confirmation: issue #159 "36 detached indexers, ~66 GB RAM, host unusable."

Design lessons: cross-process ownership must be structural (daemon admission or fs lease), not a language-runtime lock; watchers belong to the owner, not to every client; index writes must be distinguishable from user edits in event streams.
