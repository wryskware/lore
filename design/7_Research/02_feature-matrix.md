# Feature Matrix — Local Context Engines & Memory Systems

Survey date: 2026-08-14. Sources: worker reports in `raw/` (A = source-level survey of local clones, B = web discovery sweep, C = embeddings brief), plus parent spot-verification of load-bearing claims. Legend: ✓ verified/strong · ~ partial/qualified · ✗ absent · ? unverified claim.

## Retrieval / graph engines

| | Single-owner arch | Hybrid retrieval | AST chunking | Code graph | C# semantics | Windows native | Worktrees | GPU embeddings reachable | Memory | License | Activity |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **codesearch** (flupkede) | ✓ serve hub + thin MCP clients, repo lifecycle states, write locks | ✓ Tantivy BM25 + arroy/LMDB vectors | ✗ **TODO in source** — line-window fallback (verified) | ~ C#-only via SCIP | ✓ Roslyn/SCIP helper (needs .NET 10) | ✓ hardened (AV-safe Tantivy, UNC paths) | ✓ HEAD polling for `.git` files | ✗ fastembed CPU only, no custom endpoint | ✗ | Apache-2.0 | ✓ v1.2.10 Aug 12 2026; solo maintainer |
| **codebase-memory-mcp** (DeusData) — *installed* | ✓ C daemon, admission ABI, supervised workers | ~ FTS5/BM25 + algorithmic vectors | ✓ graph-oriented tree-sitter extraction | ✓ persistent SQLite graph, 14+ tools | ~ "light semantic pass" `cs_lsp.c` (verified), not Roslyn | ✓ native, but active Win bug queue (#402 persistence, paths, stdio) | ? | ✗ fixed TF-IDF/random-indexing + vendored int8 vectors — **not pluggable** | ~ graph metadata only | MIT | ✓ very active; large issue surface |
| **opencode-codebase-index** (Helweg) | ~ cross-process filesystem lease (verified tests), no resident daemon | ✓ dense + BM25 RRF, reranking, context packs | ✓ Rust tree-sitter, comment-aware | ~ call graph **excludes C#** (verified) | ~ parsed, no call graph | ✓ MSVC accommodations | ~ branch-aware snapshots | ✓ **custom OpenAI-compatible endpoint** | ✗ | MIT | ✓ 4 releases in Aug 2026; 2 contributors |
| **serena** (oraios) | ✗ per-process LSP manager | ✗ no vector/BM25 | n/a (LSP symbols) | ~ on-demand LSP relations | ✓ **official Roslyn LSP**, pinned/hashed | ~ real Win C# issues (#513, #506) | ✗ | n/a | ~ markdown memories | MIT | ✓ mature, active |
| **claude-context** (zilliztech) | ✗ | ~ Milvus-centric | ✓ incl. C# nodes | ✗ | ~ AST only | ~ Node OK; **Milvus service required** | ✗ | ~ Ollama, but store is Milvus | ✗ | MIT | ~ no releases; vendor-driven |
| **CCE** (elara-labs) — *reference only* | ✗ **asyncio dict lock, per-process watchers (verified)** → issue #159 runaway indexers | ✓ vector+FTS+graph fusion | ✓ incl. C# node lists | ~ file/chunk relations | ~ AST only | ~ FastEmbed ACCESS_VIOLATION workaround | ✗ | ~ FastEmbed/Ollama | ✓ **best schema ideas**: decisions, code areas, session recall | MIT | active but architecture disqualified |
| **CodeGraph** (codegraph-ai) — *deep-dived, see raw/D* | ~ single process but graph-in-RAM + 15s RocksDB snapshot; counter IDs churn on re-parse | ~ BM25 (names/docstrings only) + **brute-force cosine, not HNSW** | ✓ tree-sitter; "38 langs" inflated (goldens bless empty results) | ~ bare-name call resolution — every Unity `Update()` collides | ✗ no fields/events/delegates/partials/preprocessor — negative reference | ✗ 3 `cfg(windows)`, **zero CI** | ✗ | ~ local ONNX, `model_id_tag` idea is good | ~ HNSW-backed doc/memory store, full-rebuild-only | Apache-2.0 | 7/10 credibility: real code, oversold claims; harvest patterns only |
| **project-rag** (Brainwires) | ~ fs cross-process locking | ✓ BM25 + LanceDB | ✓ incl. C# | ~ lightweight | ~ AST only | ~ plausible, Linux-centric build docs | ? | ~ fastembed local | ✗ | MIT | small, quiet |
| **srclight** | ~ SQLite-per-repo | ✓ FTS5 + Ollama RRF | ✓ incl. C# | ✓ call/dep/impact | ~ AST only | ? undocumented | ? | ~ Ollama | ✗ | MIT | small |
| **octocode** (Muvon) | ? | ✓ w/ real benchmark (RRF > dense-only) | ✓ **no C#** in AST table | ✓ GraphRAG + LSP bridge | ✗ | ✓ installer | ? | ~ cloud-default, local possible | ✗ | Apache-2.0 | modest |
| **aft** (cortexkit) | ✓ persistent process per project root | ✓ | ✓ incl. C# outline/edit/AST | ✓ call graph + LSP | ~ | ? no platform matrix | ? | ✓ Ollama/OpenAI-compatible | ✗ | MIT | active monorepo; no MCP adapter yet |

## Memory-focused systems (all DB-centric; none integrate a Markdown design vault as source of truth)

| | Decision lifecycle states | Provenance/staleness | Retrieval | Local | Windows | License | Notes |
|---|---|---|---|---|---|---|---|
| **agent-memory-mcp** (ipiton) | ✓ active/outdated/superseded/canonical + confidence/drift | ✓ freshness, owner, verification, conflict | hybrid + trust weighting | ✓ Ollama/llama.cpp | ~ Go builds; no Win release | MIT | closest to decision-state requirement |
| **mindkeg-mcp** | ~ typed learnings, conflict/staleness scoring | ✓ TTL, dedup, audit | FastEmbed + FTS5 | ✓ | ~ Node OK | MIT | atomic 500-char learnings; 10 stars |
| **memory-palace** (jeffpierce) | ✗ no decision workflow | ~ graph links | semantic + graph centrality | ✓ Ollama | ✓ install.bat/ps1 | MIT | knowledge graph, not code-aware |
| **prism-coder** | ~ session/handoff-oriented | ✓ drift detection | associative | ✓ local models | ✓ Win CI | Apache-2.0 (v20+) | commercial cloud tier attached |

## What is commodity vs. scarce (verified pattern across all candidates)

**Commodity** — tree-sitter grammars, BM25, local ONNX/Ollama embeddings, content-hash incrementality, debounced watchers, MCP stdio, SQLite metadata, "hybrid search" branding.

**Scarce** — single-owner process architecture (3 real implementations: codesearch hub, codebase-memory daemon+admission, opencode lease); compiler-grade C# (2: serena Roslyn LSP, codesearch SCIP helper); GPU-reachable pluggable embeddings (1: opencode); worktree awareness (1: codesearch); durable decision memory with lifecycle (1: agent-memory-mcp, plus CCE's schema as an idea).

**Absent everywhere** — Markdown design-vault as memory source of truth; decision bindingness as a distinct axis from lifecycle; promotion gates (candidate → ratified) with human authorization; Unity-specific awareness (.unity/.prefab/GUID refs).
