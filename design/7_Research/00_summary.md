# Executive Summary — Adopt vs. Build "Lore"

2026-08-14 · Checkpoint document. Companion files: `01_landscape.md` (candidates), `02_feature-matrix.md` (matrix), `raw/` (worker evidence: A = source survey, B = discovery, C = embeddings).

## The question

Good-enough existing tooling at ≤2–3 services on Windows with real C# — or build the Lore engine using the recon as a parts list?

## What the survey established

1. **The commodity layer is solved everywhere** (tree-sitter, BM25, local ONNX embeddings, hash incrementality, watchers, MCP, SQLite). Building those from scratch buys nothing; every candidate is a subset of the same feature space, as suspected.
2. **The scarce layer is exactly what matters here**, and no single project has it all:
   - *Single-owner architecture*: 3 real implementations (codesearch serve-hub, codebase-memory daemon+admission, opencode fs-lease).
   - *Compiler-grade C#*: 2 (serena's official Roslyn LSP; codesearch's Roslyn/SCIP helper).
   - *GPU-reachable pluggable embeddings*: 1 (opencode's custom OpenAI-compatible endpoint).
   - *Decision memory with lifecycle*: 1 (agent-memory-mcp), plus CCE's schema ideas.
3. **Nobody integrates a Markdown design vault as memory source-of-truth**, models bindingness separately from lifecycle, or gates promotion behind human authorization. The Lexomancy vault process has no off-the-shelf equivalent — closest are DB-first memory stores.
4. **CCE's failure is fully diagnosed** (per-process asyncio locks + per-process watchers; see post-mortem in `01_landscape.md`) — its memory *schema* remains the best idea source.
5. **Embeddings are a solved commodity given one requirement**: llama-server (native Windows, CUDA) + Jina Code 1.5B (explicit C#) or Qwen3-Embedding-4B ≈ 5–15 nDCG points over the MiniLM-class defaults engines ship. The engine only needs a configurable OpenAI-compatible endpoint + non-384 dims + model-fingerprinted re-index. Most candidates fail this (codesearch: CPU-only fastembed; codebase-memory: fixed algorithmic vectors — the likely cause of its mediocre semantic coherence).

## The two honest paths

### Adopt (2 services, running this week)
`codesearch serve` (multi-repo hybrid retrieval over code + design vault, worktree-aware, thin MCP clients) **+** keep `codebase-memory-mcp` (structural graph). Memory = design vault indexed by codesearch + the existing ledger process — no separate memory service.
**Cost of this path:** retrieval quality is capped at commodity (line-window chunking — the AST chunker is a TODO in source — and CPU MiniLM-class embeddings, no 5090); C# impact analysis requires babysitting a .NET 10 SCIP helper; decision-state awareness stays entirely process-side; two solo-maintainer dependencies.

### Build Lore (Rust daemon, aggressive component reuse)
One daemon owning registration/watching/indexing/scheduling, modeled on codesearch's serve topology (Apache-2.0 — fork or pattern-copy), with:
- opencode's chunker design + lease/publication protocol + context packs (MIT),
- pluggable embeddings via OpenAI-compatible endpoint → llama-server on the 5090,
- CCE's memory schema (decisions, code-area routing, session promotion) reimplemented over the vault: **Markdown as canon, DB as index**,
- codebase-memory's admission/supervision/coverage-reporting ideas,
- serena's Roslyn acquisition pattern (or its MCP alongside) for compiler-grade C# when needed.
**Cost of this path:** it's a real project; the parts are references, not libraries to link (only the licenses and patterns transfer directly).

## Preliminary lean

**Build — but steal the skeleton.** The two capabilities that motivated this whole exercise (GPU-quality retrieval over code + vault, and vault-integrated decision memory) are precisely the two things no candidate ships, and every adopt path caps quality at the commodity layer or demands forking anyway. codesearch is the strongest architectural starting point (fork candidate); opencode is the retrieval-quality reference; CCE is the memory-schema reference. The adopt path remains respectable as an interim: codesearch could index the vault *today* while Lore is built.

**One open question before committing:** CodeGraph (discovered in the sweep) is the only credible "one binary does retrieval+graph+memory, Windows x64, C# crate" candidate and has *not* been source-audited. A single deep-dive would close the last real uncertainty in the adopt column.

## Decision (2026-08-14, Wrysk)

**Build Lore, in Rust** — modern idioms, current crates. This repo (`wryskware/lore`) is the project home. The survey above serves as the parts list and design reference.

Follow-ups — all research complete:
1. ✅ CodeGraph deep-dive + agent-memory-mcp schema extraction → `raw/D_codegraph.md`. Verdict: CodeGraph adoption is dead (brute-force search sold as HNSW, no Windows CI, RAM-graph/ID-churn design flaw, C# extractor is a negative reference) but it is a rich pattern source; agent-memory-mcp's schema — especially untrusted-by-default provenance gating canonical promotion — is the memory-model reference. The last "adopt" option is closed: **build is confirmed by evidence, not just preference.**
2. ✅ Rust stack brief → `raw/E_rust-stack.md`. Parent-verified against crates.io 2026-08-14: mostly exact; corrections — rmcp is at 3.1.2 (not 3.0.0), arroy at 0.8.0 (not 0.6.4; review 0.6→0.8 changes before pinning).
3. ⏭ Next: extensive planning/outlining phase (daemon architecture, memory/vault schema, MCP tool surface, phased build plan).
4. CCE purge: codex-config MCP registrations removed 2026-08-14 after a worker-fan-out crash (each codex worker spawned both registered `cce serve` instances). Remaining (separate session): `cce.exe` binary + caches, Lexomancy vault agent rules, Claude-side MCP configs.
