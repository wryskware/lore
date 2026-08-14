# Research Handoff: Ideal Local Code Context + Project Memory Engine

## Objective

Conduct a **comprehensive research and validation phase** for a local context system for AI coding agents.

The goal is not to find “another MCP search server.” The goal is to determine the best path toward a **persistent, shared context service** that combines:

1. high-quality code retrieval,
2. structural / graph-aware code navigation,
3. durable project memory,
4. explicit decision state and bindingness,
5. efficient multi-agent use,
6. robust local operation across Claude Code, Codex, and similar tools.

Do **not** assume we should build from scratch. Research existing projects deeply enough to decide whether we should:

- adopt one existing system,
- combine 2–3 mature services with a disciplined runbook,
- fork / extend one strong foundation,
- Frankenstein together reusable components from several repos,
- or build a new integrated service where the existing ecosystem is insufficient.

The preferred outcome, if practical, is a **single coherent service / daemon** hosting the major capabilities. However, a small composition of independently proven services is acceptable if it genuinely covers the requirements better and does not create substantial operational or agent-routing complexity.

---

# Background / Motivation

Current agentic coding workflows burn large amounts of context repeatedly rediscovering the same codebase structure, design decisions, implementation history, and subsystem relationships.

The desired system should reduce repeated exploration while improving—not degrading—agent task success.

A previous proposed stack included:

- `elara-labs/code-context-engine` for semantic/hybrid retrieval and persistent memory.
- `DeusData/codebase-memory-mcp` as a structural code-graph layer for callers, callees, dependencies, impact analysis, and execution tracing.
- normal source reads after narrowing.
- Grep/Glob as fallback for exact strings and cases poorly represented by semantic/graph indexing.

CCE has some useful product ideas, particularly persistent decisions and “code area” memories, but its process/resource architecture is currently viewed as a poor foundation. Treat it as a **feature/reference source**, not as the presumed implementation base.

A more promising architectural reference is `flupkede/codesearch`, especially because it appears to use a Rust-based persistent multi-repository service with thin client/MCP access. Validate this rather than taking the description at face value.

---

# Core Architectural Principle

The ideal system should have **one authoritative owner of shared context state and expensive work**.

Conceptually:

```text
Claude / Codex / other agents
            │
      thin MCP adapters
            │
            ▼
     persistent local daemon
            │
   ┌────────┼───────────────┐
   │        │               │
 retrieval graph         memory
   │        │               │
   └────────┼───────────────┘
            │
     indexing scheduler
            │
   persistence / models
```

A stdio MCP process may exist for compatibility, but it should ideally be a **thin client shell** connecting to the daemon rather than launching an independent heavy context runtime.

The daemon should be the central arbiter for:

- repository registration,
- index state,
- filesystem/git events,
- index mutation scheduling,
- coalescing rapid changes,
- embedding generation,
- global CPU/GPU/memory budgets,
- worktree awareness,
- persistent memory,
- concurrent clients,
- session identity,
- database mutations,
- version / schema migration,
- lifecycle and crash recovery.

Do not assume “SQLite can serialize writes” is sufficient coordination. SQLite is an excellent persistence layer, but repository indexing and embedding are semantic jobs whose duplicate execution can consume substantial compute before a DB lock is ever relevant.

---

# Functional Requirements

## 1. Code Retrieval

Evaluate systems for:

- semantic vector retrieval,
- BM25 / lexical retrieval,
- hybrid ranking,
- reciprocal rank fusion or comparable techniques,
- exact-symbol search,
- exact-text search,
- metadata-first / progressive-disclosure retrieval,
- chunk expansion,
- path/language/type filtering,
- repository scoping,
- cross-repository retrieval,
- relevance scoring,
- reranking,
- query rewriting where justified.

The system should minimize context payload without destroying recall.

Do not accept “token savings” claims without validating what baseline is being measured.

---

## 2. Code-Aware Chunking

Prefer syntax-aware indexing over arbitrary token windows.

Investigate:

- Tree-sitter chunking,
- language-specific AST boundaries,
- symbol-level chunks,
- class/function/module relationships,
- import context,
- docstrings/comments,
- nested symbols,
- partial classes / generated code,
- large files,
- non-code project files.

The user's primary project is Unity/C#, so **C# support must be taken seriously**.

Also evaluate handling of Unity-specific material such as:

- `.unity`,
- `.prefab`,
- `.asset`,
- `.meta`,
- GUID references,
- serialized YAML,
- shader files,
- configuration,
- string/reflection-based references that static graph tools can miss.

---

## 3. Structural / Graph Retrieval

Structural reasoning is important and may overlap heavily with the semantic engine.

Evaluate support for:

- definitions,
- references,
- callers,
- callees,
- imports,
- dependencies,
- reverse dependencies,
- inheritance,
- interface implementations,
- symbol relationships,
- subsystem mapping,
- impact analysis,
- call-chain traversal,
- graph expansion from retrieved code.

Explicitly inspect `DeusData/codebase-memory-mcp` because it was previously useful for this role.

Determine whether graph retrieval should be:

1. a first-class subsystem inside the integrated daemon,
2. delegated to an LSP,
3. generated from Tree-sitter,
4. sourced from compiler/language-server indices,
5. provided by an independent proven service,
6. or assembled from multiple sources.

Avoid duplicating graph systems unless each contributes meaningfully distinct information.

---

# 4. Persistent Project Memory

This is a **core requirement**, not an optional feature.

The user already maintains design books, decision logs, and memory Markdown files manually. The system should formalize and improve this workflow rather than merely indexing source code.

Memory types should at least consider:

### Decision
A choice made by the project, including rationale.

Example:
- “Use X instead of Y because…”
- “The resonance engine treats socket affinity as…”

### Code Area / Routing Memory
A learned map of where to investigate a concept.

Example:
- “Word evaluation starts in A; multiplier resolution is handled by B.”
- “For save migration bugs, inspect X then Y.”

This is a particularly valuable idea from CCE and should be preserved.

### Convention
A project-specific pattern agents should follow.

### Gotcha / Hazard
A surprising behavior or recurring trap.

### Investigation / Learned Fact
A durable conclusion discovered while debugging or researching.

### Rejected Approach
Something previously considered and intentionally not chosen.

### Open Question
Important unresolved design or engineering question.

### External Constraint
API/platform/tooling facts that materially constrain implementation.

---

# 5. Decision State, Authority, and Bindingness

Memory must support more than “fact text + embedding.”

The user has an existing design-book discipline where documents have semantic states such as:

- proposal,
- ratified,
- exploration,
- canon.

The context engine should model this kind of distinction directly.

Research and propose a memory/decision schema supporting at least:

## Lifecycle / Epistemic State

Examples:

- `exploration`
- `proposal`
- `provisional`
- `ratified`
- `canon`
- `deprecated`
- `superseded`
- `rejected`

Do not assume these exact labels are final; propose a clean model.

## Bindingness / Strength

A separate axis from lifecycle state.

Examples might include:

- informational,
- preference,
- default,
- strong convention,
- hard boundary / invariant.

A decision can therefore be both:

```text
state: ratified
bindingness: hard-boundary
```

or:

```text
state: canon
bindingness: preference
```

The system must not flatten these distinctions.

An agent needs to know whether something means:

> “We happened to choose this and changing it is fine.”

versus:

> “Do not cross this boundary without an explicit design change.”

Consider whether bindingness should be numeric, ordinal, categorical, policy-based, or represented through separate authority metadata.

Also investigate:

- who/what ratified the decision,
- replacement/supersession links,
- contradictions,
- scope,
- effective date,
- expiration/review date,
- confidence,
- provenance,
- references to source/design docs,
- references to commits/issues/PRs,
- automatic staleness detection.

---

# 6. Markdown / Design Book Integration

Do not assume all memory should be trapped inside a proprietary database.

The project already uses Markdown documentation as a human-facing source of truth.

Research a model where the engine can:

- ingest existing design-book Markdown,
- preserve document metadata/state,
- link memories back to documents,
- optionally write or propose structured memories,
- avoid silently overriding human-authored canon,
- distinguish extracted memory from manually ratified decisions,
- reindex doc changes,
- understand front matter or structured metadata,
- surface the original source when a memory is recalled.

Strongly consider **human-readable source-of-truth + indexed database representation** rather than DB-only memory.

---

# 7. Provenance and Staleness

Every durable memory should ideally answer:

- Where did this come from?
- When was it created?
- Who/what created it?
- What files/symbols/docs did it depend on?
- At what commit was it valid?
- Has the relevant code changed substantially since then?
- Has another decision superseded it?

Potential fields:

```text
id
type
state
bindingness
scope
title
content
rationale
created_at
updated_at
created_by
session_id
commit_sha
source_documents[]
source_files[]
source_symbols[]
supersedes[]
superseded_by
confidence
last_verified_at
staleness_status
```

Research best practices rather than blindly implementing this exact schema.

---

# 8. Memory Retrieval

Memory retrieval should not simply dump every vaguely related note into context.

Evaluate:

- semantic recall,
- lexical recall,
- typed filtering,
- state filtering,
- bindingness prioritization,
- project/subsystem scoping,
- recency,
- authority,
- provenance,
- contradiction detection,
- “hard boundary” injection,
- progressive disclosure.

Possible desired behavior:

```text
query: "change how sockets select affinity"

return:
1. hard-boundary canonical decisions first
2. relevant ratified design choices
3. code-area routing memory
4. related unresolved proposals
5. lower-confidence historical notes only if useful
```

A high-bindingness decision should not be outranked by a semantically similar casual observation.

---

# 9. Agent Session Memory

Investigate whether session-level summarization/history is valuable in addition to durable project memory.

Potential capabilities:

- searchable previous-session summaries,
- decision extraction,
- code-area discovery,
- session timeline drill-down,
- provenance back to tool calls,
- automatic candidate memory generation,
- human/agent review before promotion to durable canon.

Avoid creating an unbounded transcript landfill.

Distinguish:

```text
ephemeral session history
        ↓
candidate durable memory
        ↓
ratified/canonical project knowledge
```

This promotion model may be important.

---

# 10. Embeddings

Do not constrain the system to a tiny CPU embedding model merely for zero-config installation.

The user has substantial local GPU capability and would prefer to download and run a strong embedding model on GPU.

Research:

- current strong code embedding models,
- general embedding models that perform well on source code,
- dimensions,
- context lengths,
- latency,
- VRAM usage,
- batching,
- retrieval benchmarks,
- licensing,
- multilingual code performance,
- C# performance where data exists.

The ideal architecture should make embeddings **pluggable**.

Potential providers:

```text
built-in lightweight CPU model
local GPU inference service
Ollama
OpenAI-compatible embedding endpoint
TEI
vLLM / compatible server if appropriate
custom HTTP/gRPC provider
```

Do not force CUDA/Python dependencies into the core daemon unless there is a strong reason. A robust Rust daemon talking to a GPU model service is an acceptable and potentially preferable design.

Evaluate model quality separately from engine architecture.

---

# 11. Persistence

SQLite is acceptable and probably desirable unless research shows a better fit.

Evaluate:

- WAL behavior,
- FTS5,
- vector extensions,
- separate Tantivy index,
- embedded vector indices,
- schema migrations,
- atomic index generations,
- corruption recovery,
- concurrent reads,
- authoritative mutation ownership.

A central daemon should preferably own semantic mutations even if multiple DB connections or worker threads exist internally.

---

# 12. Worktrees and Parallel Agents

This must be explicitly tested.

Research a clean model for:

- one base repository,
- many git worktrees,
- temporary agent-created worktrees,
- branch-specific content,
- shared unchanged content,
- incremental deltas,
- worktree deletion,
- concurrent indexing,
- deduplication.

Avoid naively treating every ephemeral worktree as a wholly unrelated multi-gigabyte project.

Investigate content-addressed sharing or base-index + delta approaches if warranted.

---

# 13. Concurrency / Resource Scheduling

The preferred integrated system should centrally control:

- max concurrent index jobs,
- max embedding batches,
- CPU worker count,
- GPU concurrency,
- memory budgets,
- filesystem event debounce,
- work coalescing,
- priority between interactive retrieval and background indexing.

Interactive search should generally outrank background maintenance.

The engine should survive many simultaneous agent sessions without spawning independent heavyweight universes.

---

# 14. Language / Implementation

Rust is strongly preferred for the long-lived control plane because this is effectively local systems infrastructure:

- daemon lifecycle,
- async IPC,
- process supervision,
- file watching,
- indexing,
- SQLite,
- concurrency,
- resource scheduling,
- cross-platform behavior.

However, do not reject a mature, proven non-Rust service solely because of language.

A hybrid system is acceptable:

```text
Rust control plane
    +
external GPU embedding runtime
    +
optional language-specific helpers
```

Evaluate code quality, maturity, test coverage, issue history, maintainership, and operational robustness above language ideology.

---

# Candidate Projects to Investigate

This list is a starting point, not an exhaustive shortlist.

## High Priority

### `flupkede/codesearch`
Investigate as a possible architectural foundation.

Validate:

- daemon/server topology,
- stdio thin-client behavior,
- multi-repo handling,
- storage/index architecture,
- Tree-sitter support,
- BM25/vector fusion,
- symbol/graph features,
- watcher behavior,
- worktrees,
- embedding abstraction,
- extensibility,
- test quality,
- maintainership,
- license.

Determine how invasive it would be to add:

- durable project memory,
- decision state/bindingness,
- Markdown design-book ingestion,
- stronger external GPU embeddings,
- richer code graph retrieval.

### `DeusData/codebase-memory-mcp`
Investigate as a graph/structural retrieval source.

Determine:

- what graph it actually builds,
- how accurate it is,
- supported languages,
- C# behavior,
- persistence,
- incremental updates,
- runtime model,
- whether its useful graph capabilities can be integrated/reused rather than run independently.

## Feature / Reference Sources

### `elara-labs/code-context-engine`
Treat primarily as a feature/reference source.

Extract useful ideas around:

- persistent decisions,
- code-area routing memories,
- session recall,
- memory lifecycle.

Do not accept benchmark or “savings” claims at face value.

Also document architectural lessons from its multi-process design and recent resource/concurrency issues.

### `Helweg/opencode-codebase-index`
Evaluate its indexing/retrieval/graph implementation and whether components are reusable.

### `oraios/serena`
Evaluate LSP-driven symbolic capabilities and persistent-memory ideas.

### `Zilliz/claude-context`
Evaluate retrieval architecture and practical maturity.

## Memory-Focused Systems

Investigate current memory-oriented MCP/agent projects, including but not limited to:

- Memory Palace
- Mind Keg
- other actively maintained project-memory / decision-memory systems

Do not trust this list to be current. Search broadly for better candidates.

---

# Discovery Requirements

Search GitHub, documentation, issue trackers, benchmarks, release history, and technical writeups.

Look specifically for terms around:

- local code context engine
- repository semantic search
- code RAG
- code graph MCP
- persistent coding memory
- agent project memory
- decision memory
- architectural decision records + agents
- multi-repo MCP
- Rust MCP code search
- Tree-sitter semantic search
- LSP MCP
- code intelligence daemon
- local Augment alternative
- Sourcegraph-like local index
- repo map / code navigation agents

Be alert to SEO/GEO effects.

A project appearing repeatedly in search does **not** establish technical quality.

Trace whether apparently independent sources are actually repetitions by the same maintainers.

---

# Validation Standards

## Do Not Evaluate From README Claims Alone

For serious candidates:

1. inspect the actual source,
2. inspect architecture,
3. inspect recent commits,
4. inspect open and recently closed issues,
5. inspect contributor concentration,
6. inspect tests,
7. inspect release cadence,
8. inspect failure reports,
9. inspect real benchmarks,
10. inspect license.

When possible, validate behavior locally.

---

# Benchmark Skepticism

Differentiate carefully between:

```text
retrieval payload reduction
```

and:

```text
end-to-end agent token reduction
```

They are not the same metric.

A retrieval system cannot claim to know the counterfactual number of tokens an agent would have consumed without it unless it performs a controlled comparison.

Flag benchmarks that compare against unrealistic baselines such as “read every relevant file in full.”

Prefer:

- identical agent,
- identical model,
- identical starting repo state,
- same task,
- retrieval system enabled/disabled,
- repeated runs,
- total input/cache/output usage,
- wall time,
- tool calls,
- build/test success,
- patch quality.

---

# Evaluation Matrix

For each serious candidate, score and justify:

| Dimension | Notes |
|---|---|
| Architecture quality | |
| Persistent daemon / shared service | |
| Multi-client behavior | |
| Retrieval quality | |
| Hybrid lexical + semantic | |
| AST-aware indexing | |
| Structural graph | |
| C# support | |
| Unity-specific usefulness | |
| Incremental indexing | |
| Worktree behavior | |
| Persistent memory | |
| Decision state / authority | |
| Markdown/design-book integration | |
| Provenance/staleness | |
| GPU embedding flexibility | |
| Resource scheduling | |
| Cross-platform | |
| Test quality | |
| Maintainer activity | |
| Bus factor | |
| License/reusability | |
| Ease of extension | |
| Operational complexity | |

Use evidence, not vibes.

---

# Architecture Options to Compare

At the end of research, explicitly compare at least these paths.

## Option A: One Existing System
Adopt a mature system largely as-is.

Only recommend if it covers almost all critical requirements without architectural compromises.

## Option B: 2–3 Independent Proven Services
Example conceptual split:

```text
semantic retrieval service
graph service
memory service
```

This is acceptable if:

- each service is mature,
- capabilities are genuinely complementary,
- agent routing can be made simple and reliable,
- duplicate indexing/storage is tolerable,
- operational overhead is low.

If this approach wins, produce a **clear agent runbook** describing which service to call for which question and in what sequence.

## Option C: Fork / Extend a Strong Foundation
For example, start from a good Rust retrieval daemon and add:

- memory,
- bindingness,
- design-book ingestion,
- GPU embedding providers,
- graph integration.

Prefer this if the foundation already solves the difficult systems problems well.

## Option D: Integrated New System Using Reusable Components
Build a new daemon but reuse libraries / subsystems / code from open-source projects where licensing permits.

Potentially combine:

- proven Rust daemon patterns,
- Tree-sitter parsers,
- Tantivy,
- vector index components,
- graph extraction,
- memory schema ideas,
- Markdown ingestion.

## Option E: Greenfield
Only recommend full greenfield development if existing foundations create more complexity than they save.

---

# Desired Research Deliverables

Produce the following.

## 1. Executive Recommendation
A short recommendation answering:

> What should we actually do?

No more than ~1 page.

## 2. Candidate Landscape
A concise but broad survey of credible existing projects.

Separate:

- serious candidates,
- useful component/reference projects,
- rejected projects.

Explain rejections.

## 3. Deep Dives
For the strongest 3–5 candidates:

- architecture,
- process model,
- indexing,
- retrieval,
- graph,
- memory,
- concurrency,
- extensibility,
- maintainership,
- license,
- important issues,
- observed weaknesses.

## 4. Feature Matrix
Compare candidates against the requirements in this handoff.

## 5. Ideal-System Specification
After surveying the ecosystem, describe the ideal system we would build or assemble.

Keep this at the architecture/product level rather than prematurely writing implementation code.

## 6. Gap Analysis
For the recommended foundation/stack, identify exactly what is missing.

Example:

```text
codesearch
+ already has retrieval/index daemon
+ already has hybrid search
- no durable decision memory
- weak embedding provider flexibility
- graph lacks X
```

## 7. Build-vs-Compose Decision
Explicitly compare:

- integrated service,
- small service stack,
- fork,
- greenfield.

Include complexity and risk.

## 8. Prototype / Validation Plan
Define the smallest experiments needed to validate the recommendation before committing to implementation.

## 9. End-to-End Benchmark Plan
Design a benchmark using real coding tasks rather than retrieval-only questions.

Prefer historical tasks/issues from the actual project if available.

---

# Research Behavior

Be skeptical and evidence-driven.

Do not:

- repeat marketing claims as facts,
- use star count as a proxy for quality,
- assume “Rust = good” or “Python = bad,”
- assume semantic embeddings are always better than lexical search,
- assume more retrieval layers automatically improve results,
- assume memories are correct because an agent wrote them,
- recommend an elaborate distributed architecture when one process would suffice.

Do:

- inspect implementation,
- trace process ownership,
- look for failure modes,
- look for hidden duplicate work,
- identify state ownership,
- distinguish persistent data from runtime state,
- identify where arbitration occurs,
- inspect worktree handling,
- inspect update/index invalidation semantics,
- think about how an actual coding agent will interact with the tools.

---

# Special Attention: Agent UX

The end goal is not merely a technically elegant index.

It should make an agent reliably do the right thing without enormous prompt instructions.

The interface should ideally make it obvious when to:

- search semantically,
- search structurally,
- read exact source,
- recall project memory,
- inspect canonical decisions,
- record a newly established decision,
- record a newly discovered code area,
- challenge or supersede stale memory.

Avoid exposing 25 overlapping MCP tools if five well-designed operations would suffice.

Consider whether one higher-level query endpoint can internally combine retrieval, graph context, and memory ranking.

---

# Working Hypothesis

A promising target architecture is:

```text
                 thin MCP clients
                        │
                        ▼
                ┌──────────────┐
                │ local daemon │
                └──────┬───────┘
                       │
       ┌───────────────┼────────────────┐
       │               │                │
       ▼               ▼                ▼
 hybrid retrieval   code graph     project memory
 AST + BM25 + vec   symbols/LSP    decisions/routes
       │               │                │
       └───────────────┼────────────────┘
                       ▼
             ranking/context planner
                       │
                       ▼
                    agent
```

With:

- Rust as preferred control plane,
- pluggable external GPU embeddings,
- human-readable Markdown/design-book integration,
- explicit decision lifecycle and bindingness,
- central indexing/resource scheduling,
- worktree-aware incremental state,
- provenance and staleness tracking.

Treat this as a hypothesis to challenge, not a predetermined solution.

---

# Final Question to Answer

At the end of the research phase, give a defensible answer to:

> **Can we get essentially the ideal system from 2–3 strong existing services without significant compromises, or is the cleaner path to build/fork one integrated local context daemon using the best existing components?**

Do not begin a major implementation until this question has been answered with evidence.
