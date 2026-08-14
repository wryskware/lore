# Report D — CodeGraph Deep-Dive + agent-memory-mcp Schema Extraction

**Purpose:** harvest operation for **Lore** (new Rust local context daemon, C#/Unity-first,
Markdown-vault-as-canon, hybrid retrieval, decision memory with lifecycle + bindingness).
Not an adoption evaluation. Question answered: *what to copy, what to avoid, what is reusable.*

**Method:** direct source audit of the local clones. Every material claim below carries a
file path (absolute) and, where useful, a line number. Where I am inferring rather than
reading, I say so.

- CodeGraph clone: `C:\Users\perag\Documents\codex\2026-08-14\lore-research\repos\CodeGraph`
- agent-memory-mcp clone: `C:\Users\perag\Documents\codex\2026-08-14\lore-research\repos\agent-memory-mcp`

---

# PART 1 — CodeGraph Audit

## 0. Bottom line

**Credibility rating: 7/10 — substantive engineering, oversold claims.**

This is **not README-ware**. It is ~150,000 lines of real Rust with 2,168 test functions, a
golden-file JSON-RPC integration harness, and some of the best crash-recovery scar tissue I
have read in an OSS project of this size. The author has clearly shipped it to real users and
been burned by real production failures, and the comments record those failures honestly.

But three specific claims do not survive contact with the source:

1. **"HNSW embeddings"** — code-symbol semantic search is **brute-force cosine over every
   symbol vector in RAM**, not HNSW. HNSW exists only in the small memory/docs store.
2. **"Windows x64 first-class"** — there are exactly **3** `cfg(windows)` occurrences in the
   whole workspace, all in one file, all about downloading `onnxruntime.dll`. There is no
   Windows CI job, no path canonicalisation strategy, no case-insensitivity handling.
3. **"38 languages / 42 MCP tools"** — the 42 tools are *real and verified*. The 38 languages
   are real crates, but the golden files themselves blesses empty results for non-flagship
   languages as expected behaviour.

The single biggest architectural fact — and the one that most determines what Lore should copy —
is that **CodeGraph is not really a database-backed daemon. It is an in-RAM graph with a
RocksDB snapshot file.** Everything else follows from that.

---

## 1. Provenance, scale, and maintainer pulse

| Fact | Evidence |
|---|---|
| License | Apache-2.0, `LICENSE` + `NOTICE`; every file carries an SPDX header |
| Copyright holder | Andrey Vasilevsky `<anvanster@gmail.com>` — `NOTICE` |
| Workspace version | `0.20.1` — `Cargo.toml:63` |
| Total Rust | ~150k lines across 43 crates |
| Test functions | **2,168** (`#[test]` + `#[tokio::test]`) |
| Git authors | **1 human.** 67 commits `Andrey Vasilevsky`, 8 commits `anvanster` (same person, noreply address). Genuinely solo. |
| History span | first commit `2026-05-25`, last `2026-08-09` |

**The rate is the story.** ~150k lines of Rust, 43 crates, 42 MCP tools, 38 tree-sitter
grammars, an LSP server, a VS Code extension, a JetBrains plugin and a Visual Studio extension —
in **~11 weeks, solo**. That is roughly 2,000 lines/day sustained. This is heavily AI-assisted
code. That is not a criticism by itself; the *quality* is the question, and the quality is
bimodal (see §12).

**Web pulse — I could not verify independently.** My web-research subagent's findings did not
survive a session restart, and I am not going to reconstruct them from memory. Treat the
following as *repo-internal evidence only*:

- **Open-core is real and structural.** `crates/codegraph-server/src/mcp/pro_hooks.rs` and
  `src/lsp_pro_hooks.rs` define `ProProvider`/`ProCommandProvider` traits; the OSS build wires
  in `NoopProProvider` and reports edition `"community"`. `src/main.rs:68-73` documents tool
  profiles including `security` — *"empty on community"*. The test harness at
  `crates/codegraph-harness/cases/` contains `security_scan/`, `security_export_sarif/`,
  `security_generate_sbom/`, `security_trace_data_flow/` cases whose implementations are **not
  in this repo**. So the OSS repo is a deliberate subset of a larger private tree.
- **Telemetry is real, and honestly implemented.** `src/telemetry.rs` emits `TEL:`-prefixed
  JSON on **stderr** only; the doc comment states *"No network calls happen in this binary by
  design — egress lives in the JS layer"*, and a JS wrapper forwards to **PostHog**. Opt-out via
  `CODEGRAPH_TELEMETRY=off`. Comments elsewhere cite PostHog data as design input (e.g.
  `crates/codegraph-server/Cargo.toml`: *"none of these appear in any indexed workspace
  (PostHog, 90d)"*). For a local-first tool this is a defensible design but a real disclosure
  issue — **Lore should not do this**, or should make it explicit opt-*in*.
- Recent commits (`git log`) show active bug-fixing against numbered PRs (#13, #17, #18, #19,
  #20) — aarch64 startup crash, doc chunk-id collisions, npm packaging, per-platform engine
  fetch. The maintainer is fixing real user-reported breakage.

---

## 2. Reality check — substantive or generated filler?

**Substantive, with genuinely excellent parts and genuinely lazy parts.**

### Evidence it is real engineering

The comments are *scar tissue*, not decoration. Nobody writes these unless they got hurt:

- `crates/codegraph-server/src/crash_phase.rs:57-60` — *"every killed process used to leave its
  marker behind permanently: **310 of them accumulated on one machine over two months**."*
- `crates/codegraph-server/src/mcp/server.rs:270-281` — *"0.18.3 used a single sentinel +
  rename-quarantine. Telemetry showed **26/28 machines still looping**: on Windows the
  rename/remove of the DB directory loses to lingering handles (the just-crashed process, AV
  scanners, sibling sessions reopening the shared DB)."*
- `crates/codegraph-server/src/indexer.rs:114-118` — *"Bounty 2026-05-03: a `bounty/` workspace
  containing thousands of `.tar.gz` proof bundles caused a **4.3 GB / 644% CPU runaway** during
  initial embedding because the indexer didn't filter binary file extensions."*
- `crates/codegraph-memory/src/docs.rs:115-131` — a full post-mortem, in a doc comment, of a
  chunk-ID collision bug: *"a bare per-file counter therefore minted `doc-0001` for every
  document, so indexing a second file wrote straight over the first one's chunks… Loss was
  partial and size-dependent, which is what made it look intermittent."*
- `crates/codegraph/src/storage/rocksdb_backend.rs:41-48` — WAL recovery mode chosen with a
  written rationale about torn WAL tails and native `0xC0000005` access violations.

These are load-bearing observations from production. This is the work of someone operating the
thing, not generating it.

### Evidence of laziness / filler

- `crates/codegraph-memory/benches/memory_bench.rs` is **`fn main() {}`** — 4 lines. A benchmark
  is *declared* in `Cargo.toml` (`[[bench]] name = "memory_bench"`) and does nothing. Pure
  scaffolding theatre.
- **BM25 is implemented twice, differently, badly the second time.**
  `crates/codegraph-server/src/ai_query/text_index.rs` (751 lines, weighted fields, camelCase
  tokenizer) and `crates/codegraph-memory/src/search.rs:93-200` (a cruder duplicate). See §4.
- `crates/codegraph-memory/src/docs.rs:401` —
  `.trim_end_matches("()").trim_end_matches("()")` — the identical call twice.
- `crates/codegraph-memory/src/docs.rs:393-399` — a 6-clause `&&` chain terminated by a bare
  `|| trimmed.contains("::")` with no parentheses. Whatever was meant, this is not it being
  expressed clearly.
- The workspace `Cargo.toml:12` comment says `# Language parsers (27)` above a list of **38**.

**Verdict:** the core is real; the periphery is padded. The 38-language and 42-tool counts are
the padding, and they are what the README leads with.

---

## 3. Architecture and state ownership

### Crate layout

```
codegraph              (8.4k)   graph engine + storage traits + algorithms + export
codegraph-parser-api   (2.1k)   CodeParser trait, entity/relationship types, CodeIR
codegraph-<lang> × 38  (~90k)   tree-sitter grammar wrappers
codegraph-memory       (5.9k)   memory nodes, doc store, embeddings, HNSW, RocksDB
codegraph-server       (41.5k)  MCP + LSP server, indexer, watcher, query engine, domain tools
codegraph-harness      (2.9k)   golden-file JSON-RPC integration tests
```

The boundaries are sensible. `codegraph-server` is a 41k-line monolith with two 4-5k-line God
files (`mcp/server.rs` 5,074 lines; `backend.rs` 4,643 lines), but the `domain/` submodule
(25 files, one per analysis tool) is a clean, transport-agnostic split — see
`src/domain/curated_context.rs:4-7`: *"Curated context assembly — transport-agnostic. Extracts
get_curated_context from MCP server."* That refactor direction is correct and Lore should adopt
it from day one rather than after the file hits 5k lines.

### Who owns state — **this is the critical finding**

**The entire graph lives in RAM. RocksDB is a snapshot file, not a query engine.**

`crates/codegraph/src/graph/codegraph.rs:19-30`:

```rust
pub struct CodeGraph {
    storage: Box<dyn StorageBackend>,
    node_counter: NodeId,
    edge_counter: EdgeId,
    nodes: HashMap<NodeId, Node>,          // ← every node
    edges: HashMap<EdgeId, Edge>,          // ← every edge
    adjacency_out: HashMap<NodeId, HashSet<EdgeId>>,
    adjacency_in: HashMap<NodeId, HashSet<EdgeId>>,
}
```

`with_backend()` calls `rebuild_from_storage()`, which `scan_prefix(b"node:")` and
`scan_prefix(b"edge:")` and deserialises **everything** into those HashMaps. Every query — BFS,
Tarjan SCC, `find_all_paths`, the query builder — runs against RAM. RocksDB is never queried
during operation.

The write path mirrors this. `open_persistent_graph` (`src/mcp/server.rs:257`) is documented as:
*"Opens RocksDB … loads all data into in-memory caches, **then detaches storage to release the
database lock**."* `detach_storage()` swaps the backend for a no-op `MemoryBackend`
(`codegraph.rs`), and `persist_to()` later writes the whole graph back as a **full snapshot** —
scanning all existing `node:`/`edge:` keys to delete orphans, then re-`put`ting every node and
edge as `serde_json` blobs in one `WriteBatch`.

**Consequences, all of which Lore must avoid:**

- Persist cost is **O(entire graph)** on every flush, not O(changes). The daemon does this every
  15 seconds (`src/daemon.rs:157`, `PERSIST_INTERVAL_SECS = 15`).
- A hard kill mid-snapshot corrupts the DB. That is precisely the 26/28-machines-looping bug the
  poison-pill quarantine exists to paper over. **The recovery machinery is treating a symptom of
  the storage design.**
- Memory ceiling is the graph size. Export methods hard-error above 100k nodes
  (`check_export_size`), which is a tell about the intended scale.
- `serde_json` per node/edge — the most verbose option available.

### Concurrency

- Tokio throughout (`tokio = { version = "1", features = ["full"] }`).
- **One `Arc<RwLock<CodeGraph>>` for the whole graph** — `src/backend.rs:113`, `src/mcp/server.rs:107`.
  Any write blocks every reader across the entire codebase graph. No per-file or per-subgraph
  locking.
- `DashMap` for file caches, `parking_lot::RwLock` inside `codegraph-memory`.
- `rayon` for parallel *parsing* only (e.g. `codegraph-csharp/src/parser_impl.rs:196-222`).

### Is it single-owner across MCP clients? **No — it is one process per client, with an
optional advisory daemon.**

`crates/codegraph-server/src/daemon.rs` is the most interesting file in the repo for Lore's
purposes. The model:

- A daemon writes `~/.codegraph/daemons/<slug>.json` with `{pid, workspace, slug, started_at,
  heartbeat_at, last_index_at}` (`DaemonHeartbeat`, `daemon.rs:38-52`).
- `HEARTBEAT_INTERVAL_SECS = 10`, `STALE_AFTER_SECS = 30`; `is_fresh()` compares.
- `live_daemon_for(slug)` is the consumer entry point (`daemon.rs:121`): *"`Some` ⇒ load the
  persisted snapshot and skip re-indexing; `None` ⇒ no daemon, proceed normally."* Stale
  heartbeats are opportunistically removed.
- `run()` refuses to start a second daemon for a workspace (`daemon.rs:193-198`).
- Heartbeat write is **atomic**: `write-to-.tmp` then `rename` (`daemon.rs:92-95`), with the
  comment *"so a reader never sees a half-written file."*

So: **single-writer by advisory convention, not by enforcement**, and each MCP client still holds
its own full copy of the graph in RAM. The author knows this is the weak point — `telemetry.rs:31-33`
literally says the RSS metric exists to inform *"whether the shared-RocksDB model needs to upgrade
to a single resident process (see the daemon Model A/B trade-off)."*

> **For Lore:** the heartbeat file format, the freshness window, the atomic write-then-rename,
> and the `live_daemon_for` → *skip your own index* handshake are all directly copyable. But
> Lore should go where CodeGraph is heading, not where it is: **one resident owner process, real
> IPC, clients hold no graph state at all.** That is already stated as Lore's design intent; this
> repo is the evidence for why.

---

## 4. Storage

### RocksDB usage — deliberately minimal

`crates/codegraph/src/storage/rocksdb_backend.rs` (510 lines, ~half tests) is a plain KV wrapper
implementing `put/get/delete/exists/scan_prefix/write_batch/flush`.

- **No column families are used.** `opts.create_missing_column_families(true)` is set but no CF
  is ever created or addressed. Everything is one keyspace.
- **Key scheme is string-formatted:** `node:{id}`, `edge:{id}`, `meta:counters`. Values are
  `serde_json`.
- **No prefix extractor configured**, so `prefix_iterator` degrades to a total-order seek. The
  code compensates correctly with an explicit `starts_with` check and `break`
  (`rocksdb_backend.rs:263-267`) — correct, but it means prefix scans are not the O(matching)
  operation the name implies.
- **Atomicity:** `write_batch` is a real `WriteBatch`, so a snapshot persist is atomic *at the
  RocksDB level*. Individual `add_node`/`add_edge` calls are separate `put`s — a node and its
  edges are **not** written atomically on the incremental path.

### Multi-project namespacing — one global DB

`crates/codegraph/src/storage/namespaced.rs` wraps any backend and prefixes keys with
`"<namespace>:"`. All projects share **one** `~/.codegraph/graph.db`.

The namespace is `project_slug()` (`crates/codegraph-server/src/memory.rs:30-59`), and **it has
two real bugs**:

```rust
let mut hasher = std::collections::hash_map::DefaultHasher::new();
canonical.to_string_lossy().as_ref().hash(&mut hasher);
let short_hash = format!("{:04x}", hasher.finish() & 0xFFFF);
format!("{slug_base}-{short_hash}")
```

1. **`DefaultHasher` is used for a persisted key.** Rust explicitly does not guarantee
   `DefaultHasher` stability across releases. A toolchain upgrade silently changes every
   project's namespace → the entire persisted index orphans inside the shared DB and reindexes
   from scratch, with the old namespace left as permanent dead weight.
   **The author already knows this** — `crates/codegraph-memory/src/docs.rs:97-103` writes a
   hand-rolled FNV-1a *specifically to avoid this exact trap*, with the comment *"Deliberately
   not `DefaultHasher`: this value is baked into a persisted RocksDB key."* The lesson was
   learned in one file and never applied to the other. Classic solo-project drift.
2. **16-bit disambiguator.** `hash & 0xFFFF` gives 65,536 buckets; birthday collision odds hit
   ~50% around 300 projects. Two projects sharing a directory name *and* a colliding nibble-pair
   share a namespace → silent cross-project contamination.

### Crash recovery — the best part of the repo

Three layers, all worth studying:

1. **WAL point-in-time recovery** — `rocksdb_backend.rs:48`,
   `opts.set_wal_recovery_mode(DBRecoveryMode::PointInTime)`, with a written rationale.
2. **Stale-LOCK recovery** — `open_with_stale_lock_recovery` (`rocksdb_backend.rs:71`). Only
   clears a `LOCK` when (a) opening failed, (b) the error string looks lock-shaped, **and** (c) an
   `fs2` advisory-lock probe on the `LOCK` inode succeeds, proving no live holder. The comment is
   explicit: *"Without that double check we would happily steal a lock from a healthy concurrent
   process."* This is exactly right and Lore should copy the *shape* of it.
3. **Poison-pill quarantine via generation pointer** — `src/mcp/server.rs:257-340`. Because a
   corrupt SST/MANIFEST surfaces as a native `0xC0000005` that no `Result` or `catch_unwind` can
   intercept, detection must span process restarts. Per-PID sentinel files
   (`graph.loading.<pid>`, body carries process start time so a recycled Windows PID cannot
   impersonate a live loader), classified by a liveness probe; only a **dead** owner's sentinel
   counts as poison. Recovery **redirects** (bump `~/.codegraph/graph.generation`, resolve
   `graph.db.N`) rather than renaming, because on Windows the rename loses to lingering handles.
   A best-effort sweep cleans old generations on later startups.

That is a genuinely sophisticated answer to a genuinely nasty problem.

### Secondary-instance reads

`open_as_secondary` + `try_catch_up_with_primary` (`rocksdb_backend.rs:124-161`) support a
read-only instance that reads a DB a live writer owns, without taking the LOCK. There is a real
test for it (`test_secondary_reads_primary_writes_after_catch_up`, including asserting a
secondary write *fails*). **This is the single most reusable primitive in the repo for Lore's
one-writer-many-readers requirement** — and notably, CodeGraph does not actually use it for its
main path.

### Migration story — thin

`crates/codegraph-memory/src/migration.rs` (474 lines) exists and `MemoryStore::new` calls
`migrate_if_needed`. It uses a `_db_version` key, currently `5` ("Jina Code V2 768d vectors")
with a candid comment: *"Migration code expects v1 = JSON, but we now use JSON in v3+ too."*
The **core graph has no migration mechanism at all** — no schema version on `graph.db`, no
`meta:version` key. If the `Node`/`Edge` serde shape changes, `rebuild_from_storage` returns
`GraphError::serialization` and the whole graph fails to load.

---

## 5. Retrieval

### BM25 — hand-rolled, in-RAM, not persisted, and quadratic

`crates/codegraph-server/src/ai_query/text_index.rs`. **Not tantivy.** No external search crate
anywhere in the workspace.

Design: `HashMap<String, Vec<Posting>>` inverted index, `K1 = 1.2`, `B = 0.75`, field weights
`WEIGHT_SYMBOL_NAME = 3.0`, `WEIGHT_DOCSTRING = 2.0`, `WEIGHT_COMMENT = 1.0` (lines 16-25).
IDF is the standard `ln((N - df + 0.5)/(df + 0.5) + 1)`.

The tokenizer (`text_index.rs:333-368`) is the good part: camelCase-aware with correct acronym
handling (`HTMLParser` → `html`, `parser`, not `h`,`t`,`m`,`l`), snake_case, and a
short-token filter with a programming-term allowlist (`id`, `io`, `ok`). Well-tested.
**Lore should copy this tokenizer.**

Four hard problems:

1. **It only indexes symbol *name*, *docstring*, and *comments* — never the body.**
   `add_document(node_id, name, docstring, comments)` is the entire surface. So BM25 cannot find
   a literal string, an error message, a magic constant, or a call inside a function. For an
   agent asking *"where is the string 'InvalidHandshake'"*, this index is blind. That is a
   large fraction of what agents actually need lexical search for.
2. **`add_posting` is a linear scan.** `postings.iter_mut().find(|p| p.node_id == node_id)`
   (`text_index.rs:292`) — O(df) per insertion, so building the index is O(n²) in the frequency
   of common tokens (`get`, `data`, `value`, `id`). The bundled perf test only goes to 1,000
   docs with *unique* tokens (`function{i}`), so it never exercises the pathology it would hit
   at 100k symbols.
3. **Not persisted.** Rebuilt from the in-RAM graph on every startup.
4. **No stemming, no stop-words, no phrase queries.** `Posting.position` is stored and documented
   *"for phrase queries, future use"* — dead weight today.

### Vector search — brute force, despite the README

`crates/codegraph-server/src/ai_query/engine.rs:1074-1101`, `compute_semantic_scores`:

```rust
// Brute-force cosine similarity against all symbol vectors
for (&node_id, symbol_vec) in symbol_vecs.iter() {
    let sim = cosine_similarity(&query_vec, symbol_vec);
    if sim > 0.1 { scores.insert(node_id, sim); }
}
```

`symbol_vectors` is an in-RAM `HashMap<NodeId, Vec<f32>>`. At 100k symbols × 384 dims × f32 that
is ~150 MB of vectors resident, scanned in full on **every query**. HNSW (`instant-distance
0.6.1`) is used **only** in `codegraph-memory` — `storage.rs:35` (`MemoryStore`, tens–hundreds of
memories) and `docs.rs:461` (`DocStore`, doc chunks).

And the HNSW that does exist cannot be updated incrementally: `rebuild_hnsw_index`
(`storage.rs:428-443`) calls `Builder::default().ef_construction(100).build(points.clone(),
points.clone())` — **a full rebuild with two full clones of the point set** on every change,
because `instant-distance` has no insert API. That is exactly why the code path is not used for
code symbols. Honest, but a dead end.

> **For Lore:** pick an ANN index with incremental insert and delete from the start —
> `usearch`, `hnsw_rs`, or an embedded vector store (LanceDB). Retrofitting is what forced
> CodeGraph into brute force.

### Fusion — fixed-weight sum of a max-normalised BM25 and a raw cosine

`engine.rs:25-28` and `engine.rs:1018-1022`:

```rust
const BM25_WEIGHT: f32 = 0.4;
const SEMANTIC_WEIGHT: f32 = 0.6;
...
let bm25_norm = bm25_scores.get(&node_id).copied().unwrap_or(0.0) / max_bm25;
let score = if has_semantic {
    BM25_WEIGHT * bm25_norm + SEMANTIC_WEIGHT * semantic_sim
} else { bm25_norm };
```

Reasonable-but-crude. Max-normalisation is unstable across queries (one outlier compresses
everything else). Not RRF. Two things it *does* get right and Lore should keep:

- **Semantic-only candidates are explicitly merged in**, not just re-ranked — `engine.rs:955-967`,
  with the comment *"Add top semantic candidates that BM25 missed (the key value of semantic
  search)"*. Naive hybrid implementations re-rank a lexical candidate set and silently discard
  the entire benefit; this one doesn't.
- **`match_reason` is propagated to the caller** (`SymbolName` / `Docstring` / `Comment` /
  `Multiple` / `Semantic`), and there is an `embedding_status` string that tells the agent
  *"Embeddings are building in the background… results are from name/text search only"*
  (`engine.rs:1054-1057`). Explainability and honest degradation. **Copy both.**

**A worse duplicate exists.** `crates/codegraph-memory/src/search.rs:281-283`:

```rust
let score = bm25 * config.bm25_weight       // 0.3, BM25 is UNBOUNDED
          + semantic * config.semantic_weight // 0.5, cosine is [0,1]
          + graph * config.graph_weight;      // 0.2, [0,1]
```

**This is a scoring bug.** Raw BM25 is unnormalised and unbounded; cosine and graph proximity are
in [0,1]. Any document with a decent BM25 score dominates the sum absolutely, so the semantic and
graph weights are effectively decorative. Its tokenizer is also much worse (naive lowercase
split, `len() > 2`, no camelCase). It is a forgotten duplicate of the good implementation.

### Chunking policy

- **Code:** the unit is the *symbol*, not a fixed-size window. Embed text is built by
  `build_embed_text` (`engine.rs:132`) with a `full_body_embedding` toggle and an optional
  `split_identifiers` pass (`split_identifier_words`, `engine.rs:90`) that expands `getUserById`
  into words before embedding — a nice trick for name/query vocabulary mismatch.
- **Markdown:** heading-tree leaf chunks — see §8, this is the good one.

---

## 6. `codegraph-csharp` — the crate that matters most for Lore

3,712 lines: `visitor.rs` 1,564, `mapper.rs` 739, `aspx.rs` 327, `extractor.rs` 321,
`parser_impl.rs` 290, `lib.rs` 61, plus `tests/integration_tests.rs` 276 and
`benches/parsing.rs` 134.

### Approach

Hand-written recursive tree-sitter **cursor walk** over `tree-sitter-c-sharp 0.23`. **No `.scm`
query files anywhere.** Pipeline is `extractor.rs` (tree-sitter → `CodeIR`) → `mapper.rs`
(`CodeIR` → graph nodes/edges). `rayon` parallelises across files
(`parser_impl.rs:196-222`).

**The extractor→IR→mapper split is the best structural idea in the parser layer** and Lore should
copy it: parsing is pure and testable, graph mutation is isolated. Note that the *public* trait
undermines it — `CodeParser::parse_file(&self, path, graph: &mut CodeGraph)`
(`codegraph-parser-api/src/lib.rs`) hands the parser a mutable graph, so parsers can't be run
without one, can't be parallelised at the trait level, and can't emit a diff. **Lore's seam
should be `parse(source) -> IR`, full stop**, with a separate applier.

### What it extracts

Confirmed handled (node kinds present in `src/*.rs`): `class_declaration`, `struct_declaration`,
`interface_declaration`, `enum_declaration`, `record_declaration`, `namespace_declaration`,
`file_scoped_namespace_declaration`, `method_declaration`, `constructor_declaration`,
`property_declaration`, `accessor_declaration`, `using_directive`, `invocation_expression`,
`object_creation_expression`, `attribute`/`attribute_list`/`attributes`, `base_list`,
`type_parameter_list`/`type_parameter`, `generic_name`, plus control-flow nodes for complexity
metrics (`if_statement`, `for_statement`, `foreach_statement`, `while_statement`, `do_statement`,
`conditional_expression`, `binary_expression`).

Attributes **are** captured as raw strings via `extract_attributes` (`visitor.rs:887-895`), and
generic type parameters via `extract_type_parameters` (`visitor.rs:844-853`).

### What it does NOT extract — verified by grep across all of `src/`, zero hits each

| Missing node kind | Why it matters for Unity |
|---|---|
| `field_declaration` | **`[SerializeField] private Foo bar;` is invisible.** Fields are how Unity wires everything. |
| `event_declaration` | `UnityEvent`/C# events invisible |
| `delegate_declaration` | callback types invisible |
| `local_function_statement` | local funcs invisible |
| `lambda_expression` | every `() => {}` callback, coroutine lambda, LINQ predicate invisible |
| `record_struct_declaration` | modern value types missed |
| primary constructors | C# 12 |
| `global using` | .NET 6+ implicit usings unresolved |
| preprocessor (`#if UNITY_EDITOR`) | **editor-only code is indistinguishable from runtime code** |
| `partial` modifier merging | partial classes become N unrelated nodes |

**No field extraction is disqualifying for a Unity-first tool.** The single most important
structural fact about a `MonoBehaviour` — its serialised fields and their attributes — is not
captured at all.

There is also an `aspx.rs` (327 lines) handling ASP.NET WebForms. That tells you the author's
domain is enterprise .NET, not Unity. No occurrence of "unity", ".meta", or "Assets/" anywhere in
the workspace.

### Semantic resolution — there is essentially none

Call edges are resolved against a **file-local `node_map` keyed by bare identifier string**
(`mapper.rs:298-317`). Unresolved callees are stashed as an `unresolved_calls` string property on
the caller node, and a second pass in `crates/codegraph-server/src/watcher.rs:497-545`
(`resolve_cross_file_imports`, Phase 2) matches them against a **global `symbol_map` keyed by
bare name**.

That is: no namespace resolution, no `using`-directive scoping, no overload resolution, no type
inference, no inheritance-chain walking across files. In a Unity project, every `MonoBehaviour`
defines `Update()`, `Start()`, `Awake()`. A global bare-name symbol map means those all collide,
and Phase 2 will happily wire an arbitrary winner. **Call-graph output on a Unity codebase would
be actively misleading, which is worse than absent.**

Phase 3 does the same for `unresolved_type_refs` → `References` edges, with the same weakness.

### Is it a "light semantic pass"?

**No. It is a syntactic outline with string-matched edges.** It produces no symbol table with
resolved types. For Lore's C#/Unity-first goal, this crate is a **negative reference**: useful for
the extractor/IR/mapper shape and for the list of tree-sitter node kinds to start from, but its
resolution model must not be copied. Lore needs at minimum: file-scoped + global `using`
resolution, namespace-qualified symbol keys, partial-class merging, field/attribute extraction,
and preprocessor-symbol awareness.

---

## 7. `codegraph-memory` — what Lore's memory design should note

5,893 lines: `docs.rs` 942, `storage.rs` 894, `node.rs` 623, `migration.rs` 474,
`embedding/fastembed_embed.rs` 471, `search.rs` 369, `temporal.rs` 348,
`embedding/static_embed.rs` 295, `embedding/engine.rs` 192.

### Schema

`MemoryNode` (`src/node.rs:199-230`):

| Field | Type | Notes |
|---|---|---|
| `id` | `MemoryId(Uuid)` | v4 |
| `kind` | `MemoryKind` | tagged enum, see below |
| `title` | `String` | required |
| `content` | `String` | required |
| `temporal` | `TemporalMetadata` | bi-temporal |
| `code_links` | `Vec<CodeLink>` | `{node_id: String, node_type, relevance: f32, line_range: Option<(u32,u32)>}` |
| `embedding` | `Option<Vec<f32>>` | |
| `tags` | `Vec<String>` | |
| `source` | `MemorySource` | tagged enum |
| `confidence` | `f32` | 0.0–1.0, clamped |
| `agent_source` | `Option<String>` | free-form ("claude", "cursor", "codex"…), *deliberately* not an enum so new agents need no migration |

`MemoryKind` — five variants, each with **typed structured payload** rather than a free-text blob
(`node.rs:62-109`):

- `ArchitecturalDecision { decision, rationale, alternatives_considered: Option<Vec<String>>, stakeholders: Vec<String> }`
- `DebugContext { problem_description, root_cause: Option, solution, symptoms: Vec, related_errors: Vec }`
- `KnownIssue { description, severity: IssueSeverity, workaround: Option, tracking_id: Option }`
- `Convention { name, description, pattern: Option, anti_pattern: Option }`
- `ProjectContext { topic, description, tags: Vec }`

`IssueSeverity`: `Critical | High | Medium(default) | Low | Info`.
`MemorySource`: `UserProvided{author} | CodeExtracted{file_path} | ConversationDerived{conversation_id} | ExternalDoc{url} | GitHistory{commit_hash}`.
`LinkedNodeType`: `Function | Class | Module | File | Variable | Import | Interface | Trait`.

> **Copy:** typed per-kind payloads. "Decision" carrying `rationale` and `alternatives_considered`
> as *first-class fields* is exactly what makes decision memory queryable rather than a note
> dump. Also copy `agent_source` as a free-form string with the stated no-migration rationale.

### Bi-temporal model

`TemporalMetadata` (`src/temporal.rs:17-40`), explicitly Graphiti-inspired:

| Field | Meaning |
|---|---|
| `valid_at` | when the knowledge became true in the world |
| `invalid_at: Option` | when it stopped being true |
| `created_at` | when it was recorded |
| `superseded_at: Option` | when a newer version replaced this record |
| `commit_hash: Option` | git commit at which it was valid |
| `version_tag: Option` | e.g. `"v1.2.3"` |

Predicates: `is_current()`, `was_valid_at(t)`, `was_current_at(t)`, `valid_duration()`,
`is_at_commit(hash)`. Mutators: `invalidate()`, `invalidate_at(t)`, `supersede()`.

**Two independent time axes (world-truth vs record-lifetime) plus a git anchor is the right
skeleton, and Lore should adopt it.** Anchoring memory to `commit_hash` is especially good.

### What is missing, and what is dead

- **There is no lifecycle enum.** No `active`/`outdated`/`superseded`/`canonical`. Lifecycle is
  implied by nullable timestamps only. No `draft`. No promotion concept at all.
- **There is no bindingness axis.** Nothing distinguishes "we decided this and it is binding" from
  "we observed this". `confidence: f32` is the only strength signal and it conflates certainty
  with authority.
- **No human promotion gate**, no verification status, no owner, no provenance/trust field.
- **`SuggestedAction` / `CodeChangeType` / `MemoryReviewSuggestion` are dead code.**
  `temporal.rs:145-212` defines a well-thought-out invalidation policy
  (`Deleted→Invalidate@1.0`, `SignatureChanged→Review@0.9`, `MajorRefactor→Review@0.8`,
  `Renamed/Moved→Update@0.7`, `MinorEdit→None@0.0`). Grepping the whole workspace, **nothing
  outside `temporal.rs` references any of these types.** The README's "auto-invalidation —
  memories linked to code are flagged when code changes" is **not implemented**. The *policy
  table itself* is still worth stealing; the wiring does not exist.
- `MemorySearch` (`search.rs`) is built once from a snapshot with a manual `rebuild_index()`; the
  scoring bug in §5 applies.

### The doc store — `docs.rs`, the most directly relevant file in the repo

This is Markdown-as-context, which is Lore's core premise. Several ideas are straight keepers.

**Heading-tree chunking** (`docs.rs:145-310`): parse ATX headings into a tree, emit chunks for
**leaf nodes only**, carrying the full `heading_path: Vec<String>` as provenance. Non-leaf nodes
with substantial body text (>10 words) additionally emit a `"{title} (overview)"` chunk so
preamble is not lost. Over-long leaves are paragraph-split with overlap
(`split_paragraphs(content, max, max/6)`), all sharing the heading path. `searchable_text()`
prepends `heading_path.join(" > ")` so the hierarchy participates in retrieval.
**This is the right chunking model for a design vault. Copy it.**

**Prompt-injection flagging** (`docs.rs:59-78`):

```rust
const INJECTION_NEEDLES: &[&str] = &[
    "ignore previous instructions", "ignore all previous", "disregard previous",
    "you are now", "new instructions:", "system:", "[INST]", "<<SYS>>", "<|im_start|>system",
];
```

Matching sets `DocChunk.suspicious: bool`. Crucially it **does not block indexing** — the comment
explains *"false positives on legitimate security docs would be annoying… the flag surfaces in
search results so the host agent can decide."* **Copy this exactly.** Lore's vault is
human-authored but agent-consumed, and content-flag-not-block is the correct posture.

**Stable chunk IDs** (`docs.rs:97-134`): `doc-{fnv1a_of_source_path:016x}-{counter:04}`, using a
hand-rolled FNV-1a with the written rationale about `DefaultHasher` instability. Comment records
the collision bug this fixed (matching the `codegraph_index_markdown` overwrite issue fixed in
0.20.x).

**`DocClaim` — doc→code drift detection** (`docs.rs:342-409`): `extract_identifiers` pulls
backtick-delimited tokens out of chunks as *"the most reliable signal for things the doc claims
should exist in code"*, producing `{identifier, heading_path, source_file}`. This feeds the
`codegraph_verify_design` / `codegraph_design_gaps` MCP tools
(`crates/codegraph-server/src/mcp/server.rs:3526+`), which run **forward** (doc claims → does the
symbol exist?) and **reverse** (code → is it documented?).

**The idea is excellent and Lore should build on it. The implementation is not.** For each claim
it runs a full fuzzy hybrid `symbol_search` and calls it "found" if *any* result returns
(`server.rs:3565`). Against a BM25 index, almost any token returns something — so `found` is
near-meaningless and real gaps will be reported as verified. It is also O(claims × symbols):
one query embedding plus a brute-force cosine sweep over every vector, per identifier. A doc with
200 backticked identifiers against 50k symbols is ~10M cosine ops and 200 embeddings **per tool
call**. Lore should do exact-symbol lookup against a resolved symbol table, not fuzzy search.

Also note `docs.rs:11-18` documents *why* docs did not reuse `MemoryStore`: different schema
(`heading_path`, `source_file`), different search model (pure semantic, no temporal
invalidation), and *"mixing them would pollute memory search results."* That separation
judgement is correct and Lore should preserve it — **vault chunks and decision memories are
different objects with different lifecycles.**

---

## 8. Embeddings

**Fully local. There is no remote or OpenAI-compatible provider anywhere.**

The seam is good (`crates/codegraph-memory/src/embedding/mod.rs:21-26`):

```rust
pub(crate) trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
    fn model_name(&self) -> &str;
}
```

`VectorEngine` holds `Arc<dyn Embedder>`. Two implementations:

1. **`FastembedEmbedding`** — `fastembed 4.9.1` over `ort 2.0.0-rc.9` (ONNX Runtime).
2. **`StaticEmbedding`** — model2vec: a `safetensors` token→vector lookup matrix with a HF
   `tokenizers` tokenizer, mean-pooled. No ONNX. Documented as *"~100× faster indexing"*, and a
   commit message claims *"static ~94% of BGE through real symbol_search"*.

Models (`fastembed_embed.rs:25-90`):

| Model | Dims | Max len | Notes |
|---|---|---|---|
| `BgeSmall` (default) | 384 | 512 | BGE-Small-EN-v1.5 |
| `JinaCodeV2` | 768 | 8192 | code-aware, "6× slower than BGE" |
| `Granite97mMultilingualR2` | 384 | 32768 | IBM ModernBERT; loaded via fastembed's `UserDefinedEmbeddingModel` + direct `hf-hub`, since fastembed ≤4.9 doesn't ship it |

**Two patterns Lore should copy:**

- **`model_id_tag()`** (`fastembed_embed.rs:80-90`) — a stable `"<name>:<version>"` string
  persisted *alongside the vectors*, explicitly *"so we can detect model swaps (even when
  dimensions match) and trigger re-embedding"*, with a documented back-compat default for
  untagged legacy vectors. Dimension equality is not model equality; Bge-Small and Granite are
  both 384-d and mutually incompatible. This is the correct guard.
- **RAM gating.** `embed_memory_pressured` (`engine.rs:84`) uses `sysinfo` to check available
  memory and skips loading the ONNX model rather than OOM-ing. `EmbedMode::Skip` then makes
  every downstream embed call a no-op instead of an error — degradation, not failure.

**Two problems:**

- **`VectorEngine.cache: DashMap<String, Vec<f32>>` is keyed on the full input text and never
  evicted** (`embedding/engine.rs:19-21, 76-86`). Embedding every symbol body means the cache
  retains every body string *and* its vector for the process lifetime. Unbounded growth by
  construction. Lore needs an LRU with a byte budget (the workspace already depends on `lru`).
- Windows ONNX handling is a runtime **download**: `ensure_ort_dll` fetches
  `onnxruntime.dll` v1.20.0 via `ureq` + `zip` because the crate uses `ort-load-dynamic` on
  Windows to dodge a CRT `/MT` vs `/MD` mismatch (`codegraph-memory/Cargo.toml`). Workable, but
  it means first run needs network and a writable cache dir.

> **For Lore:** the `Embedder` trait shape is directly copyable; add a third impl backed by an
> OpenAI-compatible `/v1/embeddings` endpoint with batching, retry, and a dimension assertion
> against the persisted `model_id_tag`.

---

## 9. Incremental indexing and watching

### Hashing

**FNV-1a 64-bit over file content** (`crates/codegraph-server/src/indexer.rs:342`). Fine for
change detection — fast, no crypto needed.

### Persisted state

`crates/codegraph-server/src/index_state.rs` — a `HashMap<PathBuf, u64>` serialised to
`~/.codegraph/projects/<slug>/index_state.json`, or `<workspace>/.codegraph-state/` for ephemeral
test workspaces (nice touch: `is_ephemeral_workspace` keeps test runs out of the global
registry).

**Three defects:**

1. **Non-atomic write.** `save()` does a bare `std::fs::write` (`index_state.rs:121`) — no
   temp+rename, unlike `DaemonHeartbeat::write` which gets it right. A crash mid-write leaves a
   truncated JSON file. `load()` does degrade safely (parse failure → return 0 → full reindex),
   so the failure mode is "silent full reindex", not corruption.
2. **`save()` early-returns when `hashes.is_empty()`** (`index_state.rs:104`). An empty project
   state can never be persisted, and a `clear()` cannot be durably recorded.
3. **The hash map is the only crash-consistency mechanism, and it is not transactionally tied to
   the graph snapshot.** The graph persists on a 15s daemon tick; the hash state persists
   separately. A crash between the two leaves `index_state.json` claiming files are indexed that
   the persisted graph does not contain → **silent under-indexing that survives restart**, since
   unchanged hashes mean those files are skipped forever. This is the correctness hole Lore must
   close: **index state and index content must commit together, or state must be derived from
   content.**

### Watching

`notify 6.1.1`, with three separate debounce constants across three files:

| Path | Constant | Value |
|---|---|---|
| `watcher.rs:22` | `DEFAULT_DEBOUNCE_MS` | 300 ms |
| `mcp/file_watcher.rs:23` | `DEBOUNCE_MS` | 2000 ms |
| `branch_watcher.rs:28` | `BRANCH_DEBOUNCE_MS` | 2000 ms |
| `embed_queue.rs:22` | `DEBOUNCE` | 500 ms |

A dedicated `branch_watcher` that reacts to git branch switches is a good idea worth copying — a
branch switch is a bulk-change event that file-level debounce handles badly.

### The embed queue — best-in-repo, copy wholesale

`crates/codegraph-server/src/embed_queue.rs` (137 lines). An unbounded mpsc feeding a background
task that: blocks for the first path, then coalesces into a `HashSet<PathBuf>` until either the
500 ms window elapses or `MAX_BATCH = 256` is reached; re-embeds each file; prunes orphan
vectors; persists.

The `EmbedMode` tri-state is the sharp idea (`embed_queue.rs:29-36`):

```rust
pub enum EmbedMode {
    Now,      // synchronous — used on SAVE, "the durable event"
    Enqueue,  // debounced background — used on open / change / watch
    Skip,     // no embedding — semantic search lags until next save
}
```

Tying synchronous work to the *durable* user event (save) and deferring everything else is
exactly right for an editor-integrated daemon.

**But it exposes the deepest design flaw.** `embed_queue.rs` line comment:

> `// Re-parsing reassigns node IDs; drop vectors whose node is gone.`

Node IDs are monotonic counters from `next_node_id()` (`codegraph.rs`). **Re-parsing one file
churns the IDs of everything in it.** Consequences cascade: vectors orphan and must be pruned;
the BM25 index (keyed by `NodeId`) is invalidated; cross-file `Calls` edges resolved by Phase 2
must be re-derived; `MemoryNode.code_links` (which store `node_id` as a *string*) silently rot
and point at whatever now holds that ID; `persist_to` needs its orphan-deletion sweep purely
because *"nodes a re-parse removed get fresh ids on re-add"* (`codegraph.rs`, `persist_to`
comment).

> **This is the number-one mistake for Lore to avoid.** Symbol identity must be **content- or
> path-derived and stable across re-parses** — e.g. `hash(file_path + namespace-qualified symbol
> path + disambiguator)`. Every incremental-update problem CodeGraph fights is downstream of
> monotonic counter IDs.

### Indexing limits and exclusions

`IndexConfig::default()` (`indexer.rs:33-42`): `max_file_size_bytes = 1 MiB`, `max_depth = 20`,
**`max_files = 5_000`**.

`default_exclude_dirs()` is a well-curated ~60-entry list including a thoughtful third category —
credential directories (`.aws`, `.ssh`, `.gnupg`, `.kube`, `.docker`) — with the rationale
*"A user who accidentally indexes their home dir should not have those embedded into graph.db."*
`default_exclude_patterns()` is a ~60-glob binary/archive/media filter born of the 4.3 GB runaway.
**Both lists are directly liftable for Lore.**

**Two Unity-specific gaps:**

- Matching is `config.exclude_dirs.iter().any(|e| e == &dir_name)` (`indexer.rs:509`) — **exact,
  case-sensitive string equality**. On Windows, `Build/` does not match `"build"` and `Logs/`
  does not match `"logs"`.
- **No Unity directories are excluded at all**: `Library/`, `Temp/`, `Obj/`, `Builds/`,
  `UserSettings/`, `.vs/`, `Packages/` are all absent. A Unity project's `Library/` alone holds
  tens of thousands of files. Combined with `max_files = 5_000`, a Unity workspace will very
  plausibly **exhaust its file budget inside `Library/` before ever reaching `Assets/`.**

---

## 10. Windows — the weakest claim

**Direct measurement: 3 occurrences of `cfg(windows)` / `target_os = "windows"` in the entire
Rust workspace, all in `crates/codegraph-memory/src/embedding/fastembed_embed.rs`**, all
concerning the ONNX Runtime DLL.

What genuinely exists:

- **`ort-load-dynamic` on Windows** vs `ort-download-binaries` elsewhere, with the CRT
  `/MT` vs `/MD` rationale written down (`codegraph-memory/Cargo.toml`). Real, specific Windows
  knowledge.
- **`libz-sys` pinned to `=1.1.25`** at workspace level: *"v1.1.26 has broken vendored zlib build
  on macOS/Windows."* Real.
- **`fs2` advisory locks** — chosen because they map to `LockFileEx` on Windows and `fcntl` on
  POSIX, the same primitive RocksDB uses (`rocksdb_backend.rs:205-208`).
- **The entire poison-pill generation-pointer design exists because of Windows** — rename/remove
  of a DB directory losing to lingering handles and AV scanners; PID recycling addressed by
  stamping process start time into the sentinel body.
- A Windows-shaped test: `test_stale_lock_recovery_with_live_holder_does_not_deadlock_or_panic`
  documents that *"Windows refuses the LOCK probe outright (sharing violation)"*.

What does **not** exist:

- **No Windows CI.** `.github/workflows/` contains exactly one file, `codegraph-pr.yml`, which is
  an *example workflow for consumers* (`runs-on: ubuntu-latest`, installs the npm package, runs
  `codegraph_pr_context`). **There is no build or test CI in this repository at all.**
- **No path normalisation strategy.** `canonicalize()` is called in `daemon.rs:188`,
  `memory.rs:33/132`, `mcp/engine.rs:97/370` — on Windows this yields `\\?\C:\...` verbatim
  paths, which then flow into `project_slug` hashing and into path comparisons elsewhere.
  Mixing canonicalised and non-canonicalised paths as `HashMap<PathBuf, u64>` keys in
  `IndexState` is a live hazard.
- **No case-insensitivity handling.** Windows paths are case-insensitive; `PathBuf` comparison is
  not. A watcher event reporting `C:\proj\Foo.cs` against an index keyed `C:\proj\foo.cs` misses,
  producing silent staleness.
- No long-path/UNC handling, no `MAX_PATH` awareness.

**Assessment:** Windows is a *supported and battle-tested runtime target* — the crash machinery
proves the author has debugged real Windows failures in the field. It is **not** "first-class" in
the engineering sense: no CI, no path model, no case handling. For Lore, which is Windows-native
by design, this is the area with the least to copy and the most to do properly from the start.

---

## 11. Dependency hygiene

543 total packages in `Cargo.lock` — heavy, but 38 vendored tree-sitter grammars explain most of it.

| Crate | Locked | Assessment |
|---|---|---|
| `rocksdb` | 0.22.0 | one minor behind; fine |
| `tokio` | 1.51.0 | current, `features = ["full"]` (could be trimmed) |
| `tower-lsp` | 0.20.0 | **unmaintained upstream**; `tower-lsp-server` is the live fork |
| `notify` | 6.1.1 | two majors behind (7.x/8.x) |
| `dashmap` | 5.5.3 | one major behind (6.x) |
| `sysinfo` | 0.30.13 | several minors behind; API churns a lot |
| `ort` | **2.0.0-rc.9** | **a release candidate shipping in a production binary** |
| `fastembed` | 4.9.1 | current |
| `instant-distance` | 0.6.1 | works, but **no incremental insert** — see §5 |
| `thiserror` | **1.0.69 *and* 2.0.18** | both in tree |
| `tree-sitter` | **0.20.10 *and* 0.25.10** | see below |
| `libz-sys` | `=1.1.25` | deliberately pinned, rationale documented — good |

**The tree-sitter split is the real finding.** All 38 grammar crates declare `tree-sitter = "0.25"`,
but `tree-sitter-kotlin` transitively pulls **`tree-sitter 0.20.10`**, so two copies of the
tree-sitter C runtime are linked into one binary. It evidently links and ships, but it is ~1 MB
of duplicated native runtime and a latent ODR/ABI hazard for a C library. One stale grammar crate
is imposing this on the whole build.

Release profile is correct for a shipped binary: `opt-level = 3`, `lto = true`,
`codegen-units = 1` (`Cargo.toml:138-141`).

Good hygiene worth noting: heavy zero-usage grammars (COBOL, Fortran, Dart, Zig, R, Perl) are
**feature-gated off by default** with a measured justification — *"COBOL's parser.c alone is
30.7 MB"*, `-25 MB` binary size in the commit message. Making the 38-language claim while
shipping 32 is a marketing choice, but the engineering call is right.

---

## 12. Tests and quality

**Quantity is real:**

| Crate | Test fns |
|---|---|
| `codegraph-server` | 401 |
| `codegraph` | 125 |
| `codegraph-csharp` | 69 |
| `codegraph-memory` | 61 |
| `codegraph-harness` | 38 |
| `codegraph-parser-api` | 31 |
| **workspace total** | **2,168** |

**The golden-file harness is the standout.** `crates/codegraph-harness/` drives the server over
**real JSON-RPC** with ~40 case directories (one per MCP tool), YAML case files, fixtures, a
`bless` mode, normalisation, and profiles (`src/{bless,case,compare,jsonrpc,normalize,profiles,report,runner}.rs`).
Example case (`cases/get_callers/cpp_compute_total.case.yml`) with `setup.fixture`,
`invoke.tool` + `args`, and `expect.match: contains`. **This is exactly the right way to
regression-test an MCP tool surface and Lore should build one on day one.**

**But read what the goldens bless.** That same C++ case expects:

```yaml
callers: []
diagnostic:
  node_found: true
  note: 'No callers found. This may indicate: (1) the function is not called anywhere,
         (2) the language parser doesn''t extract call relationships, or (3) indexes need rebuilding.'
```

The golden files **codify known parser gaps as expected behaviour**. That is honest — and it is
also the clearest possible evidence that "38 languages" means "38 grammars are linked", not
"38 languages produce useful graphs". The `diagnostic` field itself is a good pattern: when a
tool returns nothing, tell the agent *why* rather than returning a bare empty array.

**Error handling:** `thiserror` throughout with structured variants
(`GraphError::{NodeNotFound, EdgeNotFound, Storage, Serialization, InvalidOperation}`), source
chains preserved (`is_lock_error` walks `Error::source()`). `anyhow` only in `codegraph-memory`.
**387 `.unwrap()`/`.expect()` calls in non-test `codegraph-server/src`** — high, though many are
`expect()` on genuine invariants with messages. There is a panic classifier
(`main.rs:177`, `classify_panic`) mapping panics to telemetry categories including
`mutex_poison`, which implies panics in production are an expected event.

**Benchmarks:** `criterion` is wired for `codegraph` (`benches/graph_operations.rs`, 119 lines,
real) and `codegraph-csharp` (`benches/parsing.rs`, 134 lines, real). `codegraph-memory`'s is the
`fn main() {}` stub.

---

## 13. Verdict for Lore

### (a) Patterns to COPY — with references

| # | Pattern | Source |
|---|---|---|
| 1 | **Daemon heartbeat + advisory single-writer.** `{pid, workspace, slug, started_at, heartbeat_at, last_index_at}`, 10 s beat / 30 s stale, atomic write-then-rename, `live_daemon_for()` → consumers skip their own index. | `codegraph-server/src/daemon.rs:29-129` |
| 2 | **`EmbedMode` tri-state + coalescing debounce queue.** `Now` on save (durable event), `Enqueue` on open/change, `Skip` under memory pressure; `HashSet` coalescing, `MAX_BATCH` cap. | `codegraph-server/src/embed_queue.rs` |
| 3 | **Markdown heading-tree leaf chunking** with `heading_path` provenance, `(overview)` preamble chunks, overlap-split of long leaves, hierarchy included in searchable text. | `codegraph-memory/src/docs.rs:145-335` |
| 4 | **Prompt-injection flag, not block.** `suspicious: bool` surfaced in results; agent decides. | `codegraph-memory/src/docs.rs:59-78` |
| 5 | **`model_id_tag` persisted with vectors** to detect model swaps even at equal dimensions, with a back-compat default for untagged legacy vectors. | `codegraph-memory/src/embedding/fastembed_embed.rs:80-90` |
| 6 | **Stale-LOCK recovery gated on an advisory-lock liveness probe** — never steal a lock from a live process. | `codegraph/src/storage/rocksdb_backend.rs:71-87, 198-227` |
| 7 | **RocksDB secondary instance** (`open_as_secondary` + `try_catch_up_with_primary`) for lock-free reader processes. | `codegraph/src/storage/rocksdb_backend.rs:107-161` |
| 8 | **Crash-phase breadcrumbs** — RAII `PhaseGuard` stamping `~/.codegraph/last-phase.<pid>.json`, so native crashes (`0xC0000005` in ONNX) are attributable across restarts; swept only when old **and** the PID is dead. | `codegraph-server/src/crash_phase.rs` |
| 9 | **camelCase/acronym-aware code tokenizer** with programming-term short-token allowlist. | `codegraph-server/src/ai_query/text_index.rs:333-368` |
| 10 | **Hybrid search that merges semantic-only candidates** rather than only re-ranking lexical hits; returns `match_reason` and an honest `embedding_status` while indexes warm. | `codegraph-server/src/ai_query/engine.rs:955-967, 1054-1057` |
| 11 | **`extractor → CodeIR → mapper`** parser split (pure parse, isolated graph mutation). | `codegraph-csharp/src/{extractor,mapper}.rs` |
| 12 | **Golden-file JSON-RPC harness** with `bless`, fixtures, normalisation, per-tool case dirs — and `diagnostic` fields explaining empty results. | `codegraph-harness/` |
| 13 | **Exclusion lists**: ~60 build/cache dirs *including credential dirs* (`.aws`, `.ssh`, `.gnupg`), ~60 binary/media globs. | `codegraph-server/src/indexer.rs:60-210` |
| 14 | **Typed per-kind memory payloads** — `ArchitecturalDecision{decision, rationale, alternatives_considered, stakeholders}` — and free-form `agent_source`. | `codegraph-memory/src/node.rs:62-109, 224-229` |
| 15 | **Bi-temporal metadata anchored to git** (`valid_at`/`invalid_at`/`created_at`/`superseded_at` + `commit_hash` + `version_tag`). | `codegraph-memory/src/temporal.rs:17-40` |
| 16 | **Code-change → action policy table** (`Deleted→Invalidate@1.0`, `SignatureChanged→Review@0.9`, `MinorEdit→None@0.0`). Steal the table; note it is unwired here. | `codegraph-memory/src/temporal.rs:171-212` |
| 17 | **`verify_design` forward/reverse doc↔code drift** as a *tool concept*. | `codegraph-server/src/mcp/server.rs:3526+` |
| 18 | **Transport-agnostic `domain/` modules** separate from MCP/LSP plumbing. | `codegraph-server/src/domain/` |
| 19 | **RAM-gated model loading** via `sysinfo`, degrading to `Skip` rather than OOM. | `codegraph-server/src/ai_query/engine.rs:72-90` |

### (b) Mistakes to AVOID

1. **Monotonic counter node IDs.** *The* root cause. Re-parsing a file churns IDs, orphaning
   vectors, invalidating the text index, rotting memory `code_links`, and forcing snapshot
   orphan-sweeps. **Use stable content/path-derived symbol IDs.**
2. **Whole-graph-in-RAM + full-snapshot persist.** O(graph) writes every 15 s, a hard memory
   ceiling, and torn-DB corruption on kill. The elaborate poison-pill recovery is treating this
   symptom. **Make RocksDB (or SQLite/redb) the actual store with incremental, atomic per-file
   transactions.**
3. **One global `RwLock` over the entire graph.** Any write stalls every reader.
4. **`DefaultHasher` in a persisted key** (`project_slug`) — toolchain upgrades silently orphan
   indexes. And **16-bit disambiguators** collide by ~300 projects.
5. **Index state not committed atomically with index content.** Separate `index_state.json`
   (non-atomic `fs::write`) + separate graph snapshot = permanent silent under-indexing after a
   crash between them.
6. **Lexical index over names/docstrings/comments only, never bodies.** Agents need literal
   string and constant search. Also: O(n²) posting insertion, not persisted.
7. **Brute-force cosine over all vectors, held entirely in RAM.** Choose an incremental ANN index
   up front.
8. **Unbounded embedding cache** keyed on full input text, never evicted.
9. **Two divergent BM25 implementations**, the second with a real scoring bug (unbounded BM25
   summed with [0,1] cosine). One retrieval core.
10. **Bare-name symbol resolution across files.** In Unity, every `Update()`/`Start()` collides —
    a confidently wrong call graph is worse than none.
11. **Shipping a large surface (38 languages / 42 tools) whose long tail returns blessed-empty
    results.** Depth over breadth: for Lore, C# done properly beats 38 done shallowly.
12. **Declared-but-empty benchmarks** (`fn main() {}`) and **dead policy types**
    (`SuggestedAction`, `CodeChangeType` — README-advertised, zero call sites).
13. **No build/test CI in the repo**, and no Windows CI, while claiming Windows-first.
14. **Stderr telemetry forwarded to PostHog by a JS wrapper** with only an env-var opt-out. For a
    local-first tool, make it opt-**in**.
15. **Case-sensitive exact-match directory exclusion**, and no Unity dirs (`Library/`, `Temp/`,
    `Obj/`) with a 5,000-file cap — a Unity project can exhaust its budget before reaching `Assets/`.

### (c) Code reusable at crate level under Apache-2.0

Apache-2.0 is compatible with Lore under Apache-2.0 or MIT-or-Apache dual licensing (attribution
+ NOTICE required; keep the SPDX headers on any lifted file).

**Genuinely liftable as-is (small, self-contained, well-tested):**

- `crates/codegraph-server/src/crash_phase.rs` (285 lines) — near-zero coupling; only depends on
  `sysinfo` + `tracing`. Best value-per-line in the repo.
- `crates/codegraph-server/src/embed_queue.rs` (137 lines) — swap the `QueryEngine` dep for a trait.
- `crates/codegraph-server/src/daemon.rs:29-154` — the `DaemonHeartbeat` half, independent of the
  LSP half below it.
- `tokenize()` from `crates/codegraph-server/src/ai_query/text_index.rs:333-368` plus its tests.
- Markdown chunker from `crates/codegraph-memory/src/docs.rs:80-335` (`build_heading_tree`,
  `parse_heading_line`, `collect_leaf_chunks`, `split_paragraphs`) + `INJECTION_NEEDLES`.
- `is_lock_error` / `try_clear_stale_lock` from `crates/codegraph/src/storage/rocksdb_backend.rs:171-227`.
- The exclusion lists from `crates/codegraph-server/src/indexer.rs` (data, not code — trivially
  adapted, add Unity dirs and case-insensitive matching).

**Worth reading, not lifting:** `codegraph-harness` (build your own to the same shape);
`codegraph-csharp` (take the node-kind list and the extractor/mapper split, write the resolution
yourself); the poison-pill machinery (a symptom fix Lore shouldn't need).

**Do not lift:** the `codegraph` core graph crate (in-RAM design is the thing to avoid),
`codegraph-memory/src/search.rs` (scoring bug), `project_slug` (two bugs).

### (d) Overall credibility of the project's claims

| Claim | Verdict |
|---|---|
| Single Rust binary | **True.** `codegraph-server`, LTO, one bin target. |
| RocksDB-backed persistent graph | **Misleading.** RocksDB is a snapshot file; the graph is a RAM structure. No column families, no queries against the DB. |
| BM25 + HNSW embeddings | **Half false.** BM25 is real (name/doc/comment only). Code-symbol vector search is **brute-force cosine**; HNSW is confined to the small memory/doc stores and is full-rebuild-only. |
| tree-sitter for 38 languages | **Technically true, practically inflated.** 38 crates exist; 6 are feature-gated off by default; the harness blesses empty results for non-flagship languages. |
| 42 MCP tools | **True — verified.** 42 distinct `codegraph_*` tool names in `mcp/tools.rs`. |
| LSP surfaces | **True.** `tower-lsp` (on an unmaintained version). |
| PR / impact analysis | **True.** `domain/impact.rs`, `git_mining/`, and a working example CI workflow. |
| Project memory | **Partly true.** Storage/search/bi-temporal exist. **Auto-invalidation on code change is not implemented** — the types are dead code. |
| Windows x64 first-class | **Overstated.** Real field-hardened Windows crash handling; **no Windows CI, no path/case model.** |

**7/10.** Real, operated, honestly documented software whose README describes an
architecture one refactor ahead of the code. For Lore's purposes it is a **high-value negative
example on storage and identity**, and a **high-value positive example on process lifecycle,
crash forensics, doc chunking, and MCP testing**.

---

# PART 2 — agent-memory-mcp: memory/decision data model

`github.com/ipiton/agent-memory-mcp`, **MIT**, Go, ~30.7k lines of non-test Go. Storage is
**SQLite** (`modernc.org/sqlite`, pure-Go) — **not** Postgres/pgvector. Embeddings are stored as
`BLOB` in-row and searched by exhaustive cosine in Go; there is no vector extension.

This model is **substantially more mature than CodeGraph's** on exactly the axes Lore cares
about: lifecycle, provenance, promotion gating, and stewardship.

## 1. Memory types

Two **orthogonal** classification axes plus a third for retrieval priority.

**Axis 1 — cognitive `Type`** (`internal/memory/memory.go:23-31`), stored in column `type`:

```go
TypeEpisodic   Type = "episodic"   // Events and actions
TypeSemantic   Type = "semantic"   // Facts and knowledge
TypeProcedural Type = "procedural" // Patterns and skills
TypeWorking    Type = "working"    // Short-term working memory
```

**Axis 2 — `EngineeringType`** (`internal/memory/engineering.go:75-85`), stored in the metadata
JSON:

```go
EngineeringTypeDecision      = "decision"
EngineeringTypeIncident      = "incident"
EngineeringTypeRunbook       = "runbook"
EngineeringTypePostmortem    = "postmortem"
EngineeringTypeMigrationNote = "migration-note"
EngineeringTypeCaveat        = "caveat"
EngineeringTypeProcedure     = "procedure"
EngineeringTypeDeadEnd       = "dead_end"
```

`dead_end` is a genuinely original idea: a first-class record for *"we tried this and it does not
work"*, with its own hygiene module (`internal/memory/dead_ends_hygiene.go`) and a
`store_dead_end` MCP tool. **Lore should have this.** Negative knowledge is exactly what agents
re-derive expensively and repeatedly.

**Axis 3 — `SedimentLayer`** (`internal/memory/sediment.go:24-45`), column `sediment_layer TEXT
NOT NULL DEFAULT 'surface'`, described in-source as *"orthogonal to Type, governs retrieval
priority"*:

```go
LayerSurface   = "surface"    // session-scoped, evictable (default)
LayerEpisodic  = "episodic"
LayerSemantic  = "semantic"
LayerCharacter = "character"  // always-surfaced
```

Progression `surface → episodic → semantic → character` is driven by a `sediment_cycle` tool with
transition rules in `internal/memory/sediment.go` / `sediment_cycle.go`, and a
`BackfillSedimentLayer` migration. This is a **memory-pressure/retention axis kept deliberately
separate from lifecycle and from type**.

There is also `RecordKind` (`engineering.go:68-73`): `session_summary`, `session_checkpoint`,
`knowledge_item`, `review_queue_item`.

## 2. Lifecycle states and transitions

`internal/memory/engineering.go:89-96`:

```go
type LifecycleStatus string
const (
    LifecycleDraft      LifecycleStatus = "draft"
    LifecycleActive     LifecycleStatus = "active"
    LifecycleOutdated   LifecycleStatus = "outdated"
    LifecycleSuperseded LifecycleStatus = "superseded"
    LifecycleCanonical  LifecycleStatus = "canonical"
)
```

**Lifecycle is *derived*, not stored as a column** — resolved from metadata by a table-driven
priority chain, `lifecycleSources` (`engineering.go:205-234`), first match wins:

1. `canonical` bool flag **or** `knowledge_layer == "canonical"` → `LifecycleCanonical`
2. explicit `lifecycle_status` metadata → that value
3. `archived` bool flag → `LifecycleSuperseded`
4. `status` metadata mapped to a lifecycle → that value
5. *(fallback, not metadata-derived)* `Type == working` → `Draft`, else `Active`

The source comment is instructive: *"Adding a 7th source means appending a rule here instead of
threading another branch through `LifecycleStatusOf`."* There is a dedicated test
(`TestLifecycleStatusOfPriorityMatrix`) that *"pins the exact priority ordering"*.

**Transitions are explicit tool calls**, not automatic: `promote_to_canonical`, `mark_outdated`,
`demote_sediment`/`promote_sediment`, `sweep_archive`, `merge_duplicates`, `verify_entry`,
`resolve_review_item`.

> **For Lore:** deriving lifecycle from a *priority chain over metadata* with a pinned-order test
> is a good pattern — it makes the precedence auditable and extension cheap. But note that
> CodeGraph stores none of this and agent-memory-mcp stores it in a JSON blob; Lore should make
> lifecycle a **real indexed column** and keep the priority chain only for ingesting external
> metadata.

## 3. The promotion gate — the single best idea in this repo

`internal/memory/engineering.go:32-66` defines provenance as a first-class trust axis:

```go
MetadataProvenance = "provenance"   // T77 memory-poisoning defense

ProvenanceConversational = "conversational" // captured from a session; UNTRUSTED for auto-promotion
ProvenanceVerified       = "verified"       // vetted by a human or verification step
ProvenanceExternal       = "external"       // ingested from a trusted external source (docs, RAG)

func ProvenanceOf(m *Memory) string      // defaults to conversational when unset — safe default
func ProvenanceIsTrusted(p string) bool  // only verified | external
```

And the gate itself (`internal/memory/write.go:347-358`):

```go
func (ms *Store) PromoteToCanonical(ctx, id, owner string, verified bool) (*PromoteToCanonicalResult, error) {
    ms.writeMu.Lock(); defer ms.writeMu.Unlock()
    mem, err := ms.Get(id); if err != nil { return nil, err }
    if !verified && !ProvenanceIsTrusted(ProvenanceOf(mem)) {
        return nil, ErrPromotionRequiresVerification
    }
    ...
}
```

On success it stamps: `provenance=verified`, `owner`, `status=confirmed` (if draft/empty),
`knowledge_layer=canonical`, `canonical=true`, `canonical_promoted_at`, `last_verified_at`;
floors `Importance` at **0.95**; and lifts `SedimentLayer` to `character`.

Two design notes recorded in the comments are worth quoting:

- *"Canonical promotion is gated on it: auto-pipelines may not canonicalize a
  conversational-origin record without a human/verify step."* — **explicit defence against an
  agent laundering its own hallucination into canon.**
- *(T89 H2/M3)* *"holds `writeMu` across the whole read-modify-write (the `Get` used to sit
  outside any lock, so a concurrent writer's change could be overwritten), and lifts the sediment
  layer along with the canonical flag — the two axes used to drift apart, leaving entries that
  were canonical on one axis and surface-level on the other."*

That second note is a direct warning for Lore: **when you have multiple orthogonal axes, every
transition must move all of them, or they desynchronise.**

## 4. Storage schema

**`memories`** (`internal/memory/memory.go:257-275`):

```sql
CREATE TABLE IF NOT EXISTS memories (
    id             TEXT PRIMARY KEY,
    content        TEXT NOT NULL,
    type           TEXT NOT NULL,
    title          TEXT,
    tags           TEXT,                      -- serialized list
    context        TEXT,                      -- task slug / session
    importance     REAL DEFAULT 0.5,          -- 0.0..1.0
    metadata       TEXT,                      -- JSON blob: lifecycle, provenance, owner, ...
    embedding      BLOB,
    created_at     DATETIME NOT NULL,
    updated_at     DATETIME NOT NULL,
    accessed_at    DATETIME NOT NULL,
    access_count   INTEGER DEFAULT 0,
    sediment_layer TEXT NOT NULL DEFAULT 'surface'
);
CREATE INDEX idx_memories_type       ON memories(type);
CREATE INDEX idx_memories_context    ON memories(context);
CREATE INDEX idx_memories_importance ON memories(importance);
CREATE INDEX idx_memories_created_at ON memories(created_at);
```

Plus `embedding_model` and the temporal columns added by `ensureMemorySchema` migration. The Go
struct (`memory.go:33-61`) carries temporal fields not in the base DDL:

```go
ValidFrom    *time.Time  // when this knowledge became true
ValidUntil   *time.Time  // when it stopped being true
SupersededBy string      // ID of the entry that replaced this one
Replaces     string      // ID of the entry this one replaced
ObservedAt   *time.Time  // when first observed (may differ from created_at)
```

**`memory_triples`** — knowledge-graph layer (`memory.go:822-845`):

```sql
CREATE TABLE IF NOT EXISTS memory_triples (
    id         TEXT PRIMARY KEY,
    subj       TEXT NOT NULL,
    rel        TEXT NOT NULL,
    obj        TEXT NOT NULL,
    memory_id  TEXT NOT NULL,
    link_type  TEXT NOT NULL DEFAULT 'extracted',
    weight     REAL NOT NULL DEFAULT 1.0,
    created_at DATETIME NOT NULL,
    FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
);
-- indexes on subj, obj, memory_id, link_type
```

**`steward_inbox`** — the human review queue (`internal/steward/inbox.go:57-80`):

```sql
CREATE TABLE IF NOT EXISTS steward_inbox (
    id TEXT PRIMARY KEY, source_run_id TEXT,
    kind TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending',
    title TEXT NOT NULL, evidence TEXT, confidence REAL, urgency TEXT,
    recommended_action TEXT, target_ids TEXT, created_at DATETIME NOT NULL,
    resolved_at DATETIME, resolved_by TEXT, resolution TEXT, resolution_note TEXT
);
```

Also present: `steward_audit`, `steward_policy`, `steward_runs`, and a separate RAG vector store
(`chunks`, `index_metadata`, `indexed_files` in `internal/vectorstore/vectorstore.go`).

**Cache note worth stealing** (`memory.go:120-127`): metadata is held in the RAM cache with a
written cost/benefit — *"~300 bytes/memory; for 100k memories that is ~30 MB, an acceptable trade"*
— after a *"~24× hot path regression"* from round-tripping through SQL on every steward scan.

## 5. Metadata field vocabulary

Exact constants (`internal/memory/engineering.go:10-35`):

| Key | Semantics |
|---|---|
| `entity`, `service` | subject scoping |
| `severity`, `status` | severity; free status mapped into lifecycle |
| `lifecycle_status` | explicit lifecycle override (priority 2) |
| `knowledge_layer` | `canonical` etc. (priority 1) |
| `owner` | accountable human |
| `last_verified_at` | drives `FreshnessScore` |
| `review_required`, `review_reason` | flags into the review queue |
| `record_kind` | `session_summary` / `session_checkpoint` / `knowledge_item` / `review_queue_item` |
| `session_mode` | `coding`/`incident`/`migration`/`research`/`cleanup` |
| `derived_from`, `source_session_id`, `session_origin`, `session_boundary` | provenance chain |
| `action_kind`, `action_handling` | procedural |
| `verified_by`, `verification_method`, `verification_status` | verification triple |
| `provenance` | `conversational` / `verified` / `external` |

Plus stamped on promotion: `canonical`, `canonical_promoted_at`.

`trust.Metadata` (`internal/trust/trust.go`) is the shared retrieval-time view:

```go
type Metadata struct {
    KnowledgeLayer string; SourceType string; Confidence float64
    LastVerifiedAt time.Time; Owner string; FreshnessScore float64
}
```

## 6. Retrieval and ranking — exact formulas

**Freshness is a step function over verification age**, not a continuous decay
(`internal/scoring/scoring.go:26-43`):

```go
func FreshnessScore(lastVerifiedAt, now time.Time) float64 {
    if lastVerifiedAt.IsZero() { return 0.20 }   // never verified
    switch age := now.Sub(lastVerifiedAt); {
    case age <= 7*24h:   return 1.00
    case age <= 30*24h:  return 0.80
    case age <= 90*24h:  return 0.60
    case age <= 180*24h: return 0.35
    default:             return 0.15
    }
}
```

**Memory recall** (`internal/memory/read.go:163-260`):

```go
baseW = 0.45; importanceW = 0.35; confidenceW = 0.20; freshnessW = 0.03
minScore = 0.05
layerCharacterBoost = +0.15   // always-surfaced, exempt from minScore cutoff
layerEpisodicBoost  = -0.05

score := cosine(queryEmbedding, m.Embedding)     // or textMatchScore fallback
weightedScore := score*(baseW + m.Importance*importanceW + trust.Confidence*confidenceW)
               + trust.FreshnessScore*freshnessW
weightedScore *= recallDecayMultiplier(m)         // T68: e^(-λ·ageDays), λ = ln2/halfLife
```

Note the shape: importance and confidence **multiply** relevance (they modulate, they don't
add), while freshness adds a small constant and age decay is a **separate multiplier**. The
comment distinguishes the two time signals explicitly: `trust.FreshnessScore` is *source-verification
recency*; the decay multiplier is *record age*. Half-life is configurable
(`SetRecallHalfLife`, `read.go:112`), and `0` disables decay.

**RAG document ranking** (`internal/rag/ranking.go:249-278`):

```go
if candidate.semanticScore < 0.1 && candidate.keywordScore <= 0 { continue }  // prefilter
score := candidate.semanticScore*0.60
       + keywordComponent*0.30
       + candidate.recencyScore
       + candidate.sourceBoost          // 0.08 runbook/postmortem, 0.07 secondary
       + confidenceComponent            // max(0, (confidence-0.50)*0.05)
if keywordComponent > 0 && candidate.semanticScore < 0.1 { score += 0.05 }   // exact-match rescue
```

Two details worth stealing: **`confidenceComponent` only rewards above-average confidence**
(`(c-0.50)*0.05`, floored at 0) rather than letting low confidence drag a good match down; and the
**exact-match rescue bonus** for a strong keyword hit the embedder missed. `sourceBoost` is a
per-`EngineeringType` table — runbooks and postmortems outrank generic notes.

## 7. Session capture

MCP tools (from `internal/server/tools_schemas*.go`): `close_session`, `analyze_session`,
`review_session_changes`, `accept_session_changes`, `end_task`, `summarize_project_context`.

`SessionSummary` (`engineering.go:107-118`): `{ID, Mode: SessionMode, Context, Service, Summary,
StartedAt, EndedAt, Tags, Metadata}`.

`SessionDelta` (`engineering.go:120+`) is *"the normalized bridge between raw session capture and
consolidation decisions"*:

```go
Summary           *SessionSummary
ExtractedEntities []EngineeringType
TouchedServices   []string
TouchedPaths      []string
SuspectedChanges  []string
InferredTopics    []string
Risks             []string
```

**The three-stage flow is the point: capture (`close_session`) → propose (`analyze_session` →
`SessionDelta`) → human accept (`review_session_changes` / `accept_session_changes`).** Nothing
enters durable memory from a session without passing a review step. Records are written with
`record_kind=session_summary` and `provenance=conversational`, so by construction they cannot be
auto-promoted to canonical.

Session start surfaces: `recall_canonical_knowledge`, `list_canonical_knowledge`,
`summarize_project_context`, `project_bank_view`, `steward_inbox`.

## 8. Staleness, conflict, and drift — the steward

`internal/steward/scanner.go` runs periodic scans producing typed inbox items
(`internal/steward/inbox.go:16-26`):

```go
InboxDuplicateCandidate     = "duplicate_candidate"
InboxContradictionCandidate = "contradiction_candidate"
InboxStaleEntry             = "stale_entry"
InboxOutdatedProcedural     = "outdated_procedural"
InboxUnverifiedRunbook      = "unverified_runbook"
InboxSourceMismatch         = "source_mismatch"
InboxMissingSourceLink      = "missing_source_link"
InboxSupersededCandidate    = "superseded_candidate"
InboxPromotionCandidate     = "promotion_candidate"
InboxDriftDetected          = "drift_detected"
```

Scanners: `scanDuplicates`, `scanConflicts`, `scanStale`, `scanExpiredWorking`,
`scanCanonicalCandidates`, `scanCanonicalHealth`, `scanSemanticConflicts`.

**`scanSemanticConflicts` (`scanner.go:509-604`) is the clever one.** It finds pairs that are
*semantically similar but likely contradictory*, judging by **divergent lifecycle status,
temporal supersession markers, or differing terminal state** — not by trying to reason about
content. Confidence comes from `contradictionConfidence(sim)`.

Equally instructive is the **false-positive suppression** work recorded in the comments (T72/T82/T83):
two *terminal* episodic records for the same task are *"the same class — not a contradiction"*,
and a shared `completionPrefixes` list (`engineering.go:290-300`) is the *"single source of truth
for the T71 idempotent writer, the T72 contradiction-suppression guard, and T82/T83 lifecycle
pairing."* The maintainer clearly shipped a naive contradiction detector, drowned in false
positives, and built structured suppression. **Lore will hit this exact wall; the lesson is that
similarity alone is not contradiction — you need a structural disagreement signal.**

`scanCanonicalHealth` produces `CanonicalIssue`s — canon that has gone stale, lost its owner, or
lost verification. **Canon needs a health check, not just a promotion path.**

Every finding lands in `steward_inbox` with `evidence []string`, `confidence`, `urgency`
(high/medium/low), `recommended_action`, `target_ids` — and is resolved by a **human** via
`steward_inbox_resolve`, recording `resolved_by`, `resolution`, `resolution_note`.

## 9. Relations between memories

Three mechanisms:

1. **Direct supersession chain** — `SupersededBy` / `Replaces` (bidirectional ID pointers) plus
   `ValidFrom`/`ValidUntil`.
2. **`memory_triples`** — `(subj, rel, obj)` with `link_type` (default `extracted`) and `weight`,
   FK-cascaded to the owning memory. `rel` is open-vocabulary. Feeds `recall_multihop`
   (`internal/memory/multihop.go`, 391 lines).
3. **`derived_from` / `source_session_id` metadata** — provenance chain back to origin.

## 10. Critique — what it gets right, what Lore still needs

**Gets right (adopt):**

1. **Provenance as a distinct axis from confidence, defaulting to untrusted**, with canonical
   promotion *gated* on it. This is the memory-poisoning defence Lore needs the moment an agent
   can write to its own memory.
2. **Orthogonal axes**: cognitive type ⟂ engineering type ⟂ lifecycle ⟂ sediment/retention. Each
   answers a different question. Collapsing them is the common design mistake.
3. **`dead_end` as a first-class record type.** Negative results are the most expensive knowledge
   to re-derive.
4. **A human review inbox as core schema**, not an afterthought — with `evidence`, `confidence`,
   `urgency`, `recommended_action`, and full resolution audit.
5. **Structural contradiction detection** (lifecycle divergence + temporal markers) rather than
   semantic similarity alone, plus explicit false-positive suppression rules.
6. **Canonical health scanning** — canon rots, and something must notice.
7. **Two distinct time signals**: `last_verified_at` (source freshness) and record age decay,
   scored separately.
8. **Ranking that modulates rather than adds** (importance/confidence multiply relevance) and only
   rewards *above-average* confidence.
9. **Three-stage session capture** — capture → propose delta → human accept.

**Missing for Lore's Markdown-vault-as-canon model:**

1. **No document provenance or anchoring.** Memories link to `service`/`entity` strings and
   `TouchedPaths`, but nothing anchors a memory to *a heading in a Markdown file at a content
   hash*. Lore's canon lives in the vault; a decision memory must cite `vault/decisions/foo.md ›
   ## Rationale @ <content-hash>` so that editing the vault can invalidate or flag the memory.
   CodeGraph's `DocChunk{source_file, heading_path}` is closer here — **combine the two.**
2. **Canon is a *state* of a memory, not a *document*.** `promote_to_canonical` flips a flag on a
   database row. In Lore, canon is a human-authored Markdown file; the database is a derived
   index. That inverts the promotion gate: Lore's gate is not "may this row become canonical?"
   but **"should this memory be *proposed as a patch* to the vault?"** — a PR-shaped flow, ending
   in a human editing a file. The steward inbox is the right *mechanism*; the resolution action
   should be "open a diff against the vault", not "set `canonical=true`".
3. **No bindingness axis.** This model conflates *authority* with *lifecycle* and *importance*.
   `canonical` + `importance≥0.95` is doing three jobs. Lore needs bindingness as a genuinely
   separate dimension — roughly `binding` / `recommended` / `informational` / `superseded-context`
   — because a decision can be **active and non-binding** (a preference), or **outdated yet still
   binding** (a constraint nobody has revisited). Neither is expressible here.
4. **Lifecycle lives in a JSON metadata blob**, derived on read by a priority chain. Convenient
   for a schemaless Go service; wrong for Lore. Make lifecycle and bindingness **indexed columns**
   with real constraints; keep a priority-chain resolver only for *importing* foreign metadata.
5. **No conflict detection between memory and canon.** The steward compares memories to *other
   memories*. Lore's most valuable signal is **memory ⟂ vault disagreement** — a recorded decision
   that contradicts the current vault text. CodeGraph's `verify_design` forward/reverse
   (`DocClaim` → symbol existence) is the right *shape*; run it as a steward scanner with exact
   symbol resolution, and add the inverse (vault claim vs. recorded decision).
6. **Vector search is exhaustive cosine in Go over BLOBs** — same scaling dead end as CodeGraph,
   reached by a different route. Both projects independently converged on brute force. **That is
   the strongest signal in this whole report that Lore should choose a real incremental ANN index
   before writing anything else.**

---

## Synthesis: the five decisions this research should settle for Lore

1. **Stable, content-derived symbol IDs.** Both the orphaned-vector pruning and the snapshot
   orphan-sweep in CodeGraph are downstream of monotonic counters. Non-negotiable.
2. **Real incremental storage + real incremental ANN.** Two independent projects both ended up
   loading everything into RAM and scanning linearly. Choose SQLite/redb + `usearch`/`hnsw_rs`
   (or LanceDB) up front; make index state and index content commit in one transaction.
3. **One resident owner process.** Copy CodeGraph's heartbeat handshake, but go where its
   telemetry comments say it is heading — clients hold no graph state.
4. **Four separate axes in the memory schema**: lifecycle (draft/active/outdated/superseded),
   **bindingness** (binding/recommended/informational), provenance (human/verified/agent/external,
   defaulting to untrusted), and retention. Never let one column do two jobs — and make every
   transition move all axes together (the T89 lesson).
5. **The vault is canon; the database is an index; promotion is a diff.** Take agent-memory-mcp's
   steward inbox mechanism wholesale, but change the terminal action from "flip `canonical=true`"
   to "propose a patch against a Markdown file for a human to accept". Anchor every memory to
   `(vault_path, heading_path, content_hash)` so vault edits mechanically flag dependent memories.
