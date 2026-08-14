---
design_status: decided
last_reviewed: 2026-08-14
---

# Lore Decision Ledger

Append-only. Newest entries at the bottom. Schema per [[README]].

## D-0001 — Vault authority and certainty model

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** All Lore design documentation and planning work
- **Decided by:** Wrysk (instruction to structure this vault like the Lexomancy design vault)
- **Decision:** This vault adopts the Lexomancy authority model: tentative-by-default documents, `design_status` lifecycle frontmatter, local certainty callouts, an append-only user-authorized decision ledger, and modality preservation in all synthesis.
- **Rationale:** The model is field-tested against authority laundering — the exact failure mode a design vault consumed by AI agents must resist.
- **Consequences:** Agents consult this ledger before treating any document as binding; promotion requires Wrysk's explicit authorization.
- **Supersedes:** None
- **Canonical sources:** [[README]]

## D-0002 — Build Lore: a new integrated Rust context daemon

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Project direction; repository purpose
- **Decided by:** Wrysk
- **Decision:** Build Lore as a new local context daemon in Rust — modern idioms, current crates — rather than adopting or forking an existing system. This repository (`wryskware/lore`) is the project home.
- **Rationale:** A five-report research phase ([[../7_Research/00_summary]]) established that the commodity layer (tree-sitter, BM25, local embeddings, watchers, SQLite) is solved everywhere, while the capabilities that motivated the project — GPU-quality retrieval over code plus design vaults, and vault-integrated decision memory with a single-owner daemon — exist nowhere. The last adopt candidate (CodeGraph) failed source audit ([[../7_Research/raw/D_codegraph|D report]]).
- **Consequences:** Surveyed projects serve as pattern sources (copy list and avoid list in [[../7_Research/01_landscape]]); planning proceeds in this vault.
- **Supersedes:** None
- **Canonical sources:** [[../7_Research/00_summary]]

## D-0003 — Hard platform constraints

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** All architecture and dependency choices
- **Decided by:** Wrysk
- **Decision:** Lore runs Windows-native (no WSL requirement). C#/Unity is the flagship language target. Embeddings are local-only — no cloud embedding providers (local GPU via an OpenAI-compatible endpoint is the intended default). One authoritative owner of index state; multi-process indexing free-for-alls of the CCE variety are disqualifying by construction.
- **Rationale:** Primary use is Lexomancy (Unity/C#) developed on Windows 11 with an RTX 5090; CCE's multi-process architecture crashed the machine on 2026-08-14 during this project's own research phase.
- **Consequences:** Cross-platform support beyond Windows is welcome but never at the expense of Windows behavior; cloud-embedding code paths may exist only as generic OpenAI-compatible endpoints pointed at local servers.
- **Supersedes:** None
- **Canonical sources:** [[../7_Research/00_summary]]

## D-0004 — v0.1 is a retrieval-first vertical slice with schema-aware seeds

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Milestone scoping (W-planning round 1, Q1)
- **Decided by:** Wrysk
- **Decision:** v0.1 delivers the daemon + repo registration + watcher + Markdown/code indexing + hybrid search over MCP, dogfooded on this repo and the Lexomancy vault. The indexer understands `design_status` frontmatter and D-NNNN references from day one, but no further memory machinery ships in v0.1.
- **Rationale:** The single-owner daemon is the hard systems part and great search is immediately useful daily; schema awareness at parse time is cheap and shapes the data model early.
- **Consequences:** Memory features build on an already-working index; v0.1 is judged as a grep/CCE replacement, not a memory system.
- **Supersedes:** None
- **Canonical sources:** [[../1_Architecture/1.1_Overview]]

## D-0005 — No graph subsystem; structural queries are out of scope

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Subsystem inventory
- **Decided by:** Wrysk
- **Decision:** Lore builds no code-graph subsystem. Semantic + lexical retrieval carries navigation; extra graph tool calls are sharply diminishing returns when the goal is saving tokens. codebase-memory-mcp may keep running beside Lore for structural queries as long as it earns its keep.
- **Rationale:** Research showed graph is the most commodity-duplicated, least-differentiating subsystem, and tree-sitter-grade C# call graphs are actively misleading in Unity code (bare-name `Update()` collisions). Compiler-grade C# (Roslyn/SCIP helper) remains a possible future entry, gated on demonstrated need.
- **Consequences:** No graph tables, no graph MCP tools, no tree-sitter call extraction anywhere in Lore.
- **Supersedes:** None
- **Canonical sources:** [[../7_Research/01_landscape]]

## D-0006 — Two-tier memory: repo-resident lore docs + session ledger

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Memory architecture gestalt (schema details remain open)
- **Decided by:** Wrysk
- **Decision:** Tier 1 — "lore" docs/memories live in the repo as first-class human-readable Markdown (the vault); the DB is only a derived, rebuildable index; there is no separate MCP write API for durable memories — agents write files, canon flows through the promotion gate. Tier 2 — a session/thread ledger (CCE's good idea): threads record heavily-compacted execution summaries, indexed for recall, answering "where'd we leave off / what's left / did we do this already" and pointing back to thread names. The ledger is working memory for the developer and agents, not canon, and may live outside the repo. Agents should post a short summary before signing off rather than relying on transcript mining.
- **Rationale:** Portability and human-readability for durable knowledge; day-to-day continuity needs a cheaper, noisier channel that should not pollute the repo.
- **Consequences:** `2_Memory/` designs two subsystems with different storage, trust, and retention rules; session summaries get their own capture convention and index.
- **Supersedes:** None
- **Canonical sources:** [[../2_Memory/2.1_Memory_Model]]

## D-0007 — Interface shape: loopback HTTP daemon, thin MCP proxy, CLI on the same API

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Process interfaces (planning round 1, Q5/Q6)
- **Decided by:** Wrysk
- **Decision:** The daemon exposes one versioned loopback HTTP API (axum). A thin `lore-mcp` stdio binary proxies MCP to it; the CLI uses the same API. No client ever touches index state. Embeddings: v0.1 consumes an external OpenAI-compatible endpoint from config (lexical search degrades gracefully when absent); daemon-managed llama-server is a later convenience.
- **Rationale:** The one architectural pattern every surveyed project validated (codesearch hub) or died without (CCE); external embedding endpoint keeps GPU process management out of the critical path.
- **Consequences:** Transport alternatives (named pipes) slot behind the same client later; `lore status`/`lore index` are HTTP calls.
- **Supersedes:** None
- **Canonical sources:** [[../1_Architecture/1.1_Overview]]

## D-0008 — Session ledger v1: data-dir Markdown, sign-off convention plus hook net

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Tier-2 memory storage and capture (planning round 2, Q1/Q2)
- **Decided by:** Wrysk
- **Decision:** Session summaries live in the daemon's data dir (e.g. `%LOCALAPPDATA%\lore\<project>\sessions\`) as Markdown files, one per session, indexed like everything else. Capture is agent-authored: convention instructs agents to post a short summary via `session_log` before signing off, backed by a Stop-hook safety net that nudges/captures when an agent forgets. No full transcript mining.
- **Rationale:** Zero repo noise; Markdown keeps the "DB is only an index" principle for tier 2; agent-authored summaries beat mined ones.
- **Consequences:** A later `lore export-sessions` can promote keepers into the vault. Wrysk flags the storage location as the kind of choice real users should eventually weigh in on — treat as default, not dogma.
- **Supersedes:** None
- **Canonical sources:** [[../2_Memory/2.1_Memory_Model]]

## D-0009 — Early end-to-end benchmarks on established OSS repos with free/local models

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Testing/benchmark strategy (planning round 2, Q5)
- **Decided by:** Wrysk
- **Decision:** Build benchmark/e2e tests early, not after the fact. Fixture corpora: a few mid-size repos from established OSS projects — a ubiquitous Python library, a JS/TS library, and a C# project — for language coverage. Driving models: gpt-5.6-luna while it is free-tier, and/or a local distilled Qwen code 27B on the RTX 5090; possibly a cheap capable API model (e.g. DeepSeek) later.
- **Rationale:** Real coding tasks against known repos beat retrieval-only metrics (research benchmark-skepticism findings). Free/local models make repeated runs costless.
- **Consequences:** Caveat recorded: luna-free measures capability, not cost savings — token-reduction claims need a model where tokens actually cost something. Bench harness lands early in the milestone plan.
- **Supersedes:** None
- **Canonical sources:** [[../5_Implementation/5.1_Milestones]]
