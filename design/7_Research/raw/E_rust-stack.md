# Brief E — Rust Stack Brief for the Lore Daemon

**Decision date:** 2026-08-14  
**Scope:** Windows-first, cross-platform Rust daemon for multi-repo code/document indexing, MCP proxying, hybrid retrieval, AST-aware chunking, graph and Markdown-backed decision memory.

## Executive recommendation

Build Lore as a Tokio 1.53 / axum 0.8 service with a small, versioned JSON API over loopback HTTP. Use the official `rmcp` 3.0.0 SDK at the MCP boundary, `rusqlite` 0.40.1 for metadata and generation state, Tantivy 0.26.1 for BM25, and arroy 0.6.4 as the first vector-index prototype. Keep vectors and metadata logically separate even if they share an on-disk directory. Use `notify` 8.2 plus `notify-debouncer-full` 0.7, gix for read-mostly Git discovery, `reqwest` 0.13.4 for the OpenAI-compatible embedding endpoint, and `ort` only behind an optional feature.

The most important architectural rule is to make one daemon the only writer. Thin MCP clients should be stateless protocol adapters. The daemon owns the SQLite transaction, Tantivy commit, vector-index update, graph update, and Markdown write; a generation record makes the resulting snapshot observable and recoverable. This avoids trying to make SQLite, Tantivy, and an ANN index participate in one distributed transaction.

## What the comparable projects show

The local reference manifests were read directly:

* [`repos/codesearch/Cargo.toml`](../repos/codesearch/Cargo.toml) uses Tokio 1.40, Tree-sitter 0.26.8 with many mixed-version grammars, `notify` 6.1 plus `notify-debouncer-full` 0.3, Tantivy 0.22, axum 0.7, Tower 0.5, arroy 0.5, `heed` 0.20, RMCP 1.8 with both server and client features, `reqwest` 0.13, and `ort` 2.0.0-rc.10. It is directionally strong, but several pins are now behind current releases; its explicit RMCP feature selection and Windows-target awareness are useful patterns.
* [`repos/CodeGraph/Cargo.toml`](../repos/CodeGraph/Cargo.toml) uses a workspace with one parser crate per language, Tokio 1, `notify` 6, RocksDB 0.22, `fs2` 0.4, `tower-lsp` 0.20, and Tree-sitter 0.25 with per-grammar versions. It explicitly pins `libz-sys = 1.1.25` because 1.1.26 broke vendored zlib builds on macOS/Windows. Its separation of language adapters is a good model; its `tower-lsp` and watcher versions are old, and its `instant-distance`/RocksDB memory layer should not be copied without a Windows build and durability test.
* [`repos/opencode-codebase-index/native/Cargo.toml`](../repos/opencode-codebase-index/native/Cargo.toml) uses Tree-sitter 0.26.8, bundled rusqlite 0.31, and usearch 2.23.0. Its target-specific usearch configuration disables default features on Windows and enables `fp16lib` because of a documented SIMD/MSVC issue. That is exactly the kind of target-specific feature split Lore should retain, though the usearch pin and rusqlite release are now stale.

The comparison supports upgrading the foundations, not indiscriminately upgrading every grammar or native dependency. Grammar crates have their own release cadence and ABI compatibility must be tested as a set.

## Stack by subsystem

The table gives the requested recommendation, main alternative, and Windows caveat. Versions are the latest stable versions found for the 2026-08-14 decision date unless marked prerelease or optional.

| Area | Recommended crate/version | Why | Main alternative | Windows-specific caveat |
|---|---|---|---|---|
| Async/runtime | `tokio` 1.53.1; `tokio-util` 0.7.19; `tower` 0.5.3 | Mature IO/runtime, cancellation, task tracking, and middleware composition | `async-std`/smol, or Tokio plus `JoinSet` only | Avoid blocking filesystem, Git, SQLite, or ONNX work on core workers; use bounded `spawn_blocking` pools |
| MCP | `rmcp` 3.0.0 | Official SDK; both server/client sides; stdio and current Streamable HTTP | Hand-written JSON-RPC, or `mcp-protocol`-style community crates | Child-process stdio needs strict stdout discipline; service installation must not assume an interactive console |
| Daemon IPC | axum 0.8.9 over `127.0.0.1` | Simple, debuggable, cross-platform, easy to version and instrument | `interprocess` 2.4.2 named pipes/Unix sockets; `tonic` 0.14.6 gRPC | Loopback is reachable by local processes; use an unguessable bearer secret and restrictive bind address |
| BM25/full text | Tantivy 0.26.1 | Mature Rust Lucene-style index with incremental commits and fast readers | SQLite FTS5 through rusqlite | One writer/index process; stale Tantivy lock files can remain after a crash and need safe recovery |
| ANN/vector index | arroy 0.6.4 for first prototype | Persistent LMDB-backed, filtering, incremental item updates, atomic index IDs | usearch 2.26.0 | LMDB/mmap and Windows file-sharing behavior must be exercised under service restarts and antivirus scanning |
| Metadata | rusqlite 0.40.1 with `bundled`; `rusqlite_migration` 2.6.0 | Direct SQLite control, predictable synchronous transactions, easy static deployment | sqlx 0.9.0 with SQLite | Bundled SQLite needs a C build toolchain; WAL sidecars must travel with the DB and are not for network shares |
| Parsing | `tree-sitter` 0.26.11 plus pinned grammar crates | Current runtime, incremental parsing, query APIs, AST-aware chunking | Tree-sitter CLI/generated C parser wrappers or language-specific parsers | Keep grammar/runtime ABI-compatible; compile all flagship grammars on MSVC in CI |
| Watching | `notify` 8.2.0 + `notify-debouncer-full` 0.7.0 | Current cross-platform backends plus rename/file-ID coalescing | polling watcher; `notify-debouncer-mini` | `ReadDirectoryChangesW` can overflow; rescan on overflow and distinguish editor saves from Lore writes |
| Git | `gix` 0.86.0 | Pure Rust, good repository/HEAD/status primitives, no libgit2 ABI | git2 0.21.0 | Validate linked worktrees, long paths, `.git` files, sparse checkouts, and file-change status cost on NTFS |
| Embeddings | reqwest 0.13.4 + serde types | Small, transparent client for any OpenAI-compatible `/v1/embeddings` endpoint | async-openai 0.18.2 | Prefer rustls or native roots deliberately; local llama-server may accept a dummy bearer token and has model/pooling constraints |
| Local ONNX fallback | `ort` 2.0.0-rc.12 behind feature; fastembed 5.17.4 as convenience layer | Fast CPU/accelerator fallback without making it the default deployment | shell out to fastembed/ONNX helper or use a separate embedding process | ONNX DLL/EP packaging, DirectML/CUDA/TensorRT selection, and AV/SmartScreen make this a release risk |
| Config/CLI/log/errors | `config` 0.15.25 or minimal TOML+serde; clap 4.6.4; tracing 0.1.44 + subscriber 0.3.23 + appender 0.2.5; thiserror 2.0.19/anyhow 1.0.104 | Current layered config, idiomatic CLI, structured daemon logs, typed library errors | Figment 0.10.19; `env_logger`; `eyre` | Log to a file, not stdout in MCP stdio mode; rotate without holding a file open across upgrade |
| Windows lifecycle | windows-service 0.8.1; `ipc-lock` 0.1.2 or named-lock 0.4-ish | SCM integration, stop/shutdown handling, cross-process single-instance guard | Background user process + Task Scheduler; `windows-services` 0.26.1 is a newer alternative API | Service and user-session processes have different profiles, ACLs, environment, and desktop access |
| Testing/release | insta 1.48.0, proptest 1.11.0, criterion 0.8.2, tempfile 3.27.0, cargo-dist 0.32.0 | Snapshot, property, benchmark, temp-fixture, and release automation coverage | cargo-wix for a focused MSI; cargo-binstall for binary installation | Test MSVC artifacts, service install/upgrade, long paths, Defender interference, and signed binaries |

## 1. Async runtime and service structure

Use `tokio = { version = "1.53", features = ["rt-multi-thread", "macros", "net", "io-util", "sync", "time", "signal", "process"] }` rather than `full` in production crates unless a feature is genuinely needed. Tokio 1.53.1 is current in the decision window and remains the ecosystem default for the MCP, HTTP, file-watch, and process integrations. [`tokio-util` 0.7.19](https://docs.rs/tokio-util/latest/tokio_util/) provides `CancellationToken` and `TaskTracker`: the token signals shutdown; the tracker closes admission and waits until tracked tasks have exited. [`tower` 0.5.3](https://docs.rs/crate/tower/0.5.3) and `tower-http` 0.7.0 provide reusable layers.

Structure the daemon as supervisors, not a single `select!` containing every task:

1. create a root cancellation token and task tracker;
2. start the IPC listener, MCP bridge, watcher, scheduler, index workers, and maintenance tasks under child tokens;
3. on SCM stop, Ctrl-C, parent-pipe close, or upgrade request, stop accepting new work, cancel children, drain in-flight generation work, flush durable Markdown/SQLite state, and wait with a bounded deadline;
4. if the deadline expires, record an interrupted generation and exit so recovery can rescan.

Use `tower::ServiceBuilder` for tracing, request IDs, body limits, concurrency limits, and timeouts. Axum intentionally uses Tower rather than a separate middleware system, so this stack also leaves the door open to mounting RMCP’s Tower service. [`TaskTracker` documentation](https://docs.rs/tokio-util/latest/tokio_util/task/struct.TaskTracker.html) specifically describes this cancellation-plus-wait pattern.

## 2. MCP protocol

Use the official [`rmcp` Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) at version 3.0.0, with the relevant `server`, `client`, `transport-io`, `transport-streamable-http-server`, `transport-streamable-http-client-reqwest`, and `macros` features. It supports both `ServerHandler` and client service APIs, stdio, child-process stdio, Streamable HTTP server, Streamable HTTP client, and in-process worker transports. That matches Lore’s two roles: the daemon serves MCP, while each thin client proxies MCP to the daemon.

RMCP is now materially more capable than the 1.8 pin in codesearch, but it is also a moving protocol boundary. RMCP 3.0 targets MCP protocol revision 2026-07-28 and includes breaking Rust API changes, stateless Streamable HTTP behavior for the new revision, discovery/lifecycle changes, and a stated MSRV of Rust 1.88. The [release notes](https://github.com/modelcontextprotocol/rust-sdk/releases) and [transport documentation](https://github.com/modelcontextprotocol/rust-sdk/blob/main/README.md) show the supported transports and the removal of the old standalone HTTP+SSE transport. Pin the exact RMCP minor/patch in the workspace, maintain conformance fixtures, and isolate RMCP types behind a small Lore MCP adapter so a future RMCP upgrade does not infect the storage core.

If RMCP is temporarily blocked by an API or conformance gap, the fallback should be a small hand-written JSON-RPC 2.0 adapter for only the methods Lore needs, not another broad community SDK. Treat that as a compatibility shim with golden wire tests, then return to RMCP. Do not build the daemon’s internal API around MCP session semantics.

On Windows, an MCP stdio server must never write logs or diagnostics to stdout; stdout is the JSON-RPC channel. Use stderr for emergency diagnostics during development and a rolling file appender in the daemon. When RMCP launches child processes, use absolute executable paths or a controlled PATH, and test service-mode environment differences.

## 3. Daemon-to-client IPC

Recommend versioned localhost HTTP with axum 0.8.9 and reqwest 0.13.4. Bind only to `127.0.0.1` (and `::1` if intentionally supported), expose a small JSON API for queries, status, generation submission, and health, and put a protocol version in every request. The client can reconnect and discover daemon restarts without needing a pipe-specific reconnection state machine. Axum’s compatibility with Tower and hyper is a strong fit for the rest of the service. [`axum` 0.8.9](https://docs.rs/axum/latest/axum/) documents its Tower integration.

The main alternatives are:

* [`interprocess` 2.4.2](https://docs.rs/interprocess/latest/interprocess/), whose local sockets map to Windows named pipes and Unix-domain sockets and whose async support is Tokio-specific today. This is attractive if localhost exposure is unacceptable, but it adds platform-specific connection/security behavior and more bespoke client reconnect testing.
* Tokio’s native Unix sockets and Windows named-pipe APIs. Tokio documents both [`UnixStream`](https://docs.rs/tokio/latest/tokio/net/struct.UnixStream.html) and [`tokio::net::windows::named_pipe`](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/index.html), but a cross-platform abstraction is Lore’s responsibility.
* [`tonic` 0.14.6](https://docs.rs/tonic/latest/tonic/) for gRPC. It is capable and interoperable, but it brings protobuf/codegen, HTTP/2, schema evolution, and a larger operational surface for an exclusively local daemon API. In May 2026 the gRPC project announced that Tonic is in maintenance mode while a future `grpc` crate is developed; that makes Tonic a poor place to anchor a new long-lived internal protocol unless Lore specifically needs gRPC tooling.

For localhost HTTP, use an authentication secret stored with user-only ACLs, reject non-loopback `Host`/origin combinations as appropriate, cap request bodies, and include daemon build/API versions in `/health`. A named pipe is the harder-security but potentially cleaner future transport; keep the internal request/response structs transport-neutral so it can be added later.

## 4. Full-text search

Use [`tantivy` 0.26.1](https://docs.rs/tantivy/latest/tantivy/) for BM25. Tantivy supports incremental indexing, immutable segments, delete-plus-reinsert updates, explicit commits, and snapshot-like readers. Its writer lock permits only one `IndexWriter` per index; the [lock documentation](https://docs.rs/tantivy/latest/tantivy/directory/static.INDEX_WRITER_LOCK.html) notes that a crash can leave a stale lock file, which is safe to remove only after confirming no writer is alive. Keep a single writer task in Lore and expose searchers/readers to query tasks.

The metadata store should hold the stable chunk ID, path, generation, language, and deletion state. Tantivy does not enforce a primary key, so Lore must use a stored/indexed stable chunk key and delete by term before reinserting. Commit in bounded batches, use `IndexReader` reload-on-commit, and keep an index manifest containing the last committed Lore generation. That lets recovery compare Tantivy’s generation with SQLite’s generation instead of guessing.

On Windows, the operational concern is not a special Tantivy algorithm but mandatory file-sharing and stale-lock behavior: do not run two daemon writers against the same index directory, avoid copying a live index, and make upgrade/recovery code tolerate files that are still memory-mapped. If a copy or backup is needed, quiesce the writer and take a consistent snapshot.

SQLite FTS5 is the alternative when the corpus is modest and minimizing moving parts outweighs Tantivy’s throughput. It makes metadata and full-text state transactional in one database, but it is less attractive for large code corpora, custom ranking, or independent index compaction. Prototype both on representative repositories before committing to FTS5 as an escape hatch.

## 5. Vector index

The candidates are not interchangeable:

| Candidate | Incremental add/delete | Persistence/filtering | Memory and maintenance assessment |
|---|---|---|---|
| **arroy 0.6.4** | Writer supports adding and removing items; tree maintenance is separate from lookup | LMDB-backed, atomic index IDs, query filters, mmap/shared readers | Low-memory, durable, and a good fit for a single daemon with bounded update batches; LMDB operational semantics are the main Windows risk |
| **usearch 2.26.0** | Add/remove and filtered search; strong single-index ergonomics | Save/load/view; view uses memory mapping; filtering predicates | Very fast and active, but native FFI/SIMD features and target-specific builds require care. The opencode reference’s Windows `default-features = false, fp16lib` split is worth preserving. |
| **hnsw_rs 0.3.4** | Insert/search/filter; persistence/mmap facilities, but no equally convenient transactional delete model | `hnswio` dump/reload and filters | Pure Rust and easy to inspect, but smaller ecosystem and more application-managed deletion/tombstone work |
| **sqlite-vec 0.1.9** | SQL table/extension model is appealing; extension capabilities should be tested for delete/update semantics | Lives next to metadata; stable 0.1.9, but 0.1.10-alpha.4 was the latest prerelease found | Attractive for one-file deployment, but a C extension and early API make it a secondary prototype, not the default commitment |
| **LanceDB Rust 0.31.0** | Rich table/versioning/filtering story and embedded operation | Columnar Lance storage; strong for large analytical/vector datasets | High dependency/build footprint and a broader database model than Lore needs; use if scale or Arrow interoperability becomes decisive |

Recommend **arroy 0.6.4 first**, because Lore explicitly needs persistence, incremental updates, filtering, and a daemon-owned durable index. Arroy’s documentation describes LMDB-backed atomic updates and a writer that removes items and builds the search tree; its release page also calls out incremental updates, filters, and small memory usage. Keep usearch as the performance baseline. Run a benchmark matrix with 10k/100k/1M chunks, 384/768/1536 dimensions, 1%, 10%, and 50% churn, metadata filters, restart recovery, and concurrent queries.

Store the canonical vector-to-chunk mapping in SQLite. The ANN index should contain stable numeric IDs and a manifest with model ID, dimension, distance metric, normalization, and build generation. Deletes should be a two-phase operation: mark the SQLite row unavailable, update the ANN index, then publish the new generation. For any engine that cannot guarantee physical deletion, use tombstones plus periodic rebuild and document data-erasure implications.

## 6. Metadata store, WAL, migrations, and generation atomicity

Use [`rusqlite` 0.40.1](https://docs.rs/crate/rusqlite/latest) with `bundled` for a controlled embedded SQLite build. The upstream README recommends `bundled` for applications that control their database and reports SQLite 3.53.2 in the current 0.40.1 line. Use one write connection/task and a small read pool or read connections; SQLite has one writer even in WAL mode.

Use `rusqlite_migration` 2.6.0 for a small embedded application: it tracks migration state in SQLite’s `user_version` and keeps the migration mechanism simple. [`refinery` 0.9.2](https://docs.rs/refinery/latest/refinery/) is the alternative if Lore wants checked, named SQL migration files and a migration runner shared with other database backends. [`sqlx` 0.9.0](https://docs.rs/sqlx/latest/sqlx/sqlite/) is the alternative when compile-time checked SQL and an async pool are more valuable than direct SQLite control. SQLx 0.9 widened its `libsqlite3-sys` version range, but it still warns that only one compatible `libsqlite3-sys` version may appear in a dependency tree; mixing it with rusqlite and extensions needs an explicit build test.

Enable WAL on local disk, set a busy timeout, and explicitly choose synchronous behavior appropriate to the durability target. SQLite’s [WAL documentation](https://sqlite.org/wal.html) is important here: readers and the single writer can overlap, but a long-lived reader can starve checkpoints and let `-wal` grow. The WAL and `-shm` files are part of the live database state; a backup/copy must include a consistent checkpoint or use SQLite’s backup mechanism. WAL is not suitable for a database on a network filesystem because the wal-index uses shared memory.

For index-generation atomicity, use a coordinator transaction:

1. begin an `IMMEDIATE` SQLite transaction and create a new generation row with status `building`;
2. write Markdown decisions atomically via temp file + flush + rename, then record content hashes;
3. build/update graph, Tantivy, and vector state under the same generation ID, each with its own durable manifest;
4. commit SQLite metadata only after all derived artifacts report durable completion;
5. flip one `active_generation` record in the final SQLite transaction and mark the previous generation retired.

There is no cross-store atomic commit. The design therefore needs an idempotent recovery state machine: `building` generations are either resumed or discarded; the active generation is never partially published. On Windows, rename-over-existing and open-handle sharing rules make temp-file replacement a first-class tested operation.

## 7. Tree-sitter and grammar management

Use [`tree-sitter` 0.26.11](https://docs.rs/tree-sitter/latest/tree_sitter/) for the runtime. The current runtime supports ABI version 15; its documentation states that the library is generally backward-compatible with older grammar ABIs but not forward-compatible with newer ones. The flagship [`tree-sitter-c-sharp` grammar is 0.23.5](https://github.com/tree-sitter/tree-sitter-c-sharp/releases), released in April 2026.

Do not assume the runtime version and grammar crate version should match numerically. The local manifests correctly demonstrate mixed grammar versions, but the CodeGraph workspace’s `tree-sitter = 0.25` and the opencode/codesearch `0.26.8` lines must be treated as separate compatibility baselines. Use a workspace table that records, for every grammar: crate version, grammar repository/tag, generated ABI, language name, query set, and whether it is flagship or optional. Compile and run a parse/query smoke test for every enabled grammar in the Windows CI matrix.

Prefer grammar crates that expose a stable `LANGUAGE` constant and use the runtime’s `Parser::set_language`. For grammars without a maintained Rust crate, vendor the grammar source and generate it with a pinned `tree-sitter-cli`, or put it behind a plugin crate. Keep `.scm` queries in source control next to the grammar adapter, snapshot the extracted symbols, and fail loudly when a query matches zero nodes after a grammar upgrade.

The C# parser should have fixtures for namespaces, file-scoped namespaces, records, primary constructors, pattern matching, top-level statements, attributes, interpolated strings, preprocessor directives, and generated code. Tree-sitter is syntactic; semantic resolution for C# still needs graph heuristics or a separate compiler/LSP path.

Windows caveat: grammar crates frequently compile generated C/C++ code through `cc`. Pin the MSVC toolchain and test clean builds, incremental builds, and all optional grammar feature combinations; this catches ABI and build-script failures earlier than an end-user install.

## 8. File watching

Use [`notify` 8.2.0](https://docs.rs/notify/latest/notify/) and [`notify-debouncer-full` 0.7.0](https://docs.rs/notify-debouncer-full/latest/notify_debouncer_full/). The full debouncer coalesces rename pairs, tracks file IDs where possible, removes duplicate create events, and re-exports notify. The old `notify` 6 plus debouncer 0.3 choices in codesearch and CodeGraph should be upgraded together, not independently.

On Windows the backend is `ReadDirectoryChangesW`. Microsoft documents that the API can return `ERROR_NOTIFY_ENUM_DIR` when its buffer overflows. On that signal, treat the watch as lossy: rescan the affected root, reconcile by path/file ID/content hash, and resume watching. Do not claim exact event history. Network-mounted paths and WSL watching Windows paths have known problems in notify; prefer a watcher process running on the same OS/filesystem as the paths.

The critical event policy is to separate editor writes from Lore writes:

* write Lore-owned derived files under a dedicated excluded directory, or tag them with an in-memory write token and suppress matching path/hash events;
* debounce by path and wait for file-size/mtime stability before reading;
* hash content when the event is ambiguous, because editors commonly use temp-write-plus-rename;
* treat rename-from/to as one logical change and rescan a directory when only one half is observed;
* schedule work through a bounded queue so a save storm cannot exhaust RAM or starve MCP queries.

## 9. Git integration and worktrees

Prefer [`gix` 0.86.0](https://docs.rs/gix/latest/gix/) for read-mostly discovery, HEAD/ref watching, object IDs, and status inspection, but pin it exactly after a focused prototype. Gitoxide is active and pure Rust; its `gix::status` module exposes worktree/index/tree status primitives. The project’s own status page is candid that some higher-level workflows remain incomplete, so Lore should use it for the subset it needs rather than promising full Git porcelain parity.

Keep [`git2` 0.21.0](https://docs.rs/crate/git2/latest) as the fallback when a required operation is missing or when compatibility with libgit2 behavior is more valuable. Git2-rs 0.21 binds libgit2 1.9 and exposes worktrees, but the `Repository`/`Worktree` objects have `!Send`/`!Sync` constraints in places and bring a C build/ABI dependency. That makes it a poor fit for arbitrary long-lived Tokio tasks without a dedicated blocking worker.

The worktree model should track the physical worktree path, common Git directory, current HEAD, branch/detached state, index path, and a generation of observed Git metadata. Watch `.git/HEAD`, the relevant `refs`, `packed-refs`, `index`, and worktree administrative files, but treat a Git event as a trigger to re-query status, not as a complete status answer. Test linked worktrees where `.git` is a file, bare repositories, detached HEADs, sparse checkouts, submodules, ignored files, and repositories with long Windows paths.

## 10. Embedding client

Use plain [`reqwest` 0.13.4](https://docs.rs/reqwest/latest/reqwest/) plus small local request/response structs for `/v1/embeddings`. This keeps Lore compatible with OpenAI, llama.cpp, vLLM, and other OpenAI-compatible servers without importing a large, opinionated OpenAI surface. Reqwest 0.13 uses rustls by default according to its documentation; choose `default-features = false, features = ["json", "rustls"]` for a self-contained TLS choice, or deliberately use native roots if corporate Windows trust stores are required.

[`async-openai` 0.18.2](https://docs.rs/crate/async-openai/0.18.2) is the main alternative and is useful if Lore later needs many OpenAI APIs, typed streaming, or provider-specific models. For an embeddings-only, OpenAI-compatible endpoint it couples Lore to an unofficial client’s generated types and release cadence for little benefit.

Use a bounded embedding queue and a semaphore for in-flight requests. Batch by both item count and estimated UTF-8/token size; preserve input order; record model, dimension, normalization, and request hash. Retry only network errors, 408, 429, and 5xx with exponential backoff plus jitter, and honor `Retry-After`; do not retry invalid dimensions or 4xx schema errors. Use an idempotency key or deterministic content hash so an interrupted batch can resume. `llama-server` documents `/v1/embeddings` as the compatible route, requires a pooling-capable embedding model, and may accept a dummy bearer token; validate `/v1/models` and dimension at startup.

Backpressure is part of the scheduler: embedding work is GPU/remote-bound and should not compete unboundedly with parsing or SQLite writes. Centralize concurrency limits so the RTX 5090 endpoint, CPU parser pool, and disk writer each have separately measurable budgets.

## 11. Optional ONNX local fallback

Keep ONNX out of the default binary. [`ort` 2.0.0-rc.12](https://docs.rs/crate/ort/latest) wraps ONNX Runtime 1.24 and supports execution providers, but it is still a release-candidate API and requires native runtime/DLL selection. [`fastembed` 5.17.4](https://docs.rs/crate/fastembed/latest) is the saner convenience layer if Lore wants a built-in CPU model: it packages model management and uses ORT underneath, with a DirectML feature for Windows GPU use.

Recommended product shape: remote OpenAI-compatible embeddings are the default; an `onnx` feature adds a separate provider implementation; a separate helper process is the fallback if DLL/EP failures destabilize the daemon. Shelling out to a helper is operationally heavier but isolates native crashes, model downloads, and GPU initialization from the indexing supervisor. Test cold start, model cache ACLs, cancellation during inference, upgrade migration, DirectML availability, CPU-only Windows, and an RTX machine before making it built in.

## 12. Configuration, CLI, logging, and errors

Use `config` 0.15.25 when Lore needs layered file/env/runtime overrides, or use a small explicit loader built on `toml` + `serde` if the configuration surface is intentionally small. [`config` 0.15.25](https://docs.rs/crate/config/latest) supports environment and file sources and live re-reading. Figment 0.10.19 remains a good alternative with a clean provider model, but its release cadence is much slower; use it only if its profile/metadata semantics materially reduce code.

Use [`clap` 4.6.4](https://docs.rs/clap/latest/clap/) derive for `lore daemon`, `lore status`, `lore index`, `lore install-service`, and `lore uninstall-service`. Keep service-control commands separate from daemon runtime flags, and ensure every command supports a machine-readable output mode for agents.

Use `tracing` 0.1.44, `tracing-subscriber` 0.3.23, `tracing-appender` 0.2.5, and `EnvFilter`. Configure one file layer with rolling rotation and one stderr layer only for foreground mode. Add fields for project ID, repository/worktree, generation, request ID, task kind, queue wait, and byte/item counts. Do not log embeddings, authorization secrets, or full source content by default. `tracing-appender` 0.2.5 adds current rotation fixes and pruning behavior; still test Windows rename/replace while a log file is open.

Use `thiserror` 2.0.19 for public library/domain errors and `anyhow` 1.0.104 at binary/application boundaries. This preserves typed matching in parser/storage crates while allowing the daemon entry point to add context and print a useful report. Keep error messages stable enough for operational diagnosis, but do not make raw error strings part of the MCP API.

## 13. Windows daemon lifecycle

Offer two modes: foreground `lore daemon --foreground` for development and a Windows service for end users. Use [`windows-service` 0.8.1](https://docs.rs/windows-service/latest/windows_service/) for the established low-level SCM integration. The newer [`windows-services` 0.26.1](https://docs.rs/windows-services/latest/windows_services/) is worth evaluating for a more ergonomic API, but do not mix the two until the service callback, stop event, failure actions, and shutdown deadline are proven.

Implement service stop as cancellation, not process termination. Report `SERVICE_START_PENDING`/`RUNNING`/`STOP_PENDING` correctly, respond quickly to SCM control requests, and let the Tokio supervisor perform the actual drain. Configure SCM failure actions for restart, but include exponential startup backoff and a persisted crash/recovery record to avoid a tight restart loop.

For a single instance, use a kernel named mutex through [`ipc-lock` 0.1.2](https://docs.rs/crate/ipc-lock/0.1.2/source/README.md) or `named-lock` if its API better matches the service/user split. A lock file alone is adequate on Unix but is not the same primitive on Windows. The Windows `CreateMutex` documentation warns that a predictable mutex name can be pre-created by another process to deny startup; use a user/service-scoped name and an ACL strategy appropriate to the installation context. Keep a PID/endpoint record for diagnostics, but do not use a stale PID file as the authority.

For autostart, prefer explicit service installation for machine-wide daemon mode and Task Scheduler or Startup for per-user mode. Avoid silently installing persistence. Upgrades should be side-by-side or stop-copy-start: drain, close DB/index handles, replace binaries, migrate schema on next start, and verify a health endpoint before considering the upgrade complete. Sign Windows binaries and installers; native ONNX DLLs and unsigned release artifacts increase SmartScreen friction.

## 14. Testing, release, MSRV, and edition

Use Rust 2024 edition. It has been stable since Rust 1.85, as documented in the [Rust 1.85 announcement](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html). Set an explicit `rust-version` based on the oldest crate in the selected stack, not on the newest compiler installed locally. RMCP 3.0 requires Rust 1.88, while current Tokio/SQLite/criterion ecosystems may move faster; a practical Lore MSRV is 1.88 or newer, validated in CI. If Windows service or ONNX dependencies force a newer toolchain, document that as an intentional product decision.

Testing split:

* `insta` 1.48.0 snapshots MCP wire messages, parser extraction, query plans, generation manifests, and CLI output. Review snapshots rather than blindly accepting them.
* `proptest` 1.11.0 generates paths, rename sequences, chunk boundaries, malformed UTF-8-adjacent inputs, graph edges, and interrupted generation state transitions.
* `criterion` 0.8.2 benchmarks parsing, chunking, BM25, ANN recall/latency, embedding batching, and generation commit throughput. Criterion’s current compatibility policy targets recent Rust releases, so pin it in the benchmark crate if MSRV matters.
* `tempfile` 3.27.0 supports isolated database/index/watch fixtures. Add Windows-specific tests that keep readers open, kill the process between generation phases, reopen stale locks, and exercise long paths.
* Run a real integration matrix with multiple thin clients, simultaneous queries and saves, watcher overflow/rescan, worktree switches, daemon restart, service stop, and upgrade.

Use [`cargo-dist` 0.32.0](https://axodotdev.github.io/cargo-dist/changelog/) for GitHub Actions, archives, PowerShell installers, and optional MSI integration. Use cargo-wix directly if Lore needs a highly controlled service MSI, or cargo-binstall metadata for developer installs. Release at least `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, and macOS targets only if the product promise requires them. Produce SBOM/audit metadata, sign Windows binaries, and test installed paths rather than only unpacked CI artifacts.

## Consolidated recommended Cargo workspace sketch

This is a dependency table, not code. Pin exact versions in `Cargo.lock`, use workspace dependencies, and enable features per crate rather than making every member depend on `full`.

| Workspace crate/subsystem | Recommended dependencies |
|---|---|
| `lore-core` | `serde 1`, `serde_json 1`, `thiserror 2.0.19`, `uuid 1`, `blake3 1` or `sha2 0.10`, `camino 1` if UTF-8 paths are desirable |
| `lore-runtime` | `tokio 1.53.1`, `tokio-util 0.7.19` (`rt`, `sync`), `tower 0.5.3`, `tracing 0.1.44` |
| `lore-api` | `axum 0.8.9`, `tower-http 0.7.0`, `reqwest 0.13.4`, `http 1`, `serde` |
| `lore-mcp` | `rmcp 3.0.0` with `server`, `client`, `transport-io`, `transport-streamable-http-server`, `transport-streamable-http-client-reqwest`, `macros`; `schemars` version compatible with RMCP 3 |
| `lore-ipc` | Initially axum/reqwest over loopback; optional `interprocess 2.4.2` transport adapter later |
| `lore-meta` | `rusqlite 0.40.1` with `bundled`, `rusqlite_migration 2.6.0`; `parking_lot 0.12` only where needed |
| `lore-search` | `tantivy 0.26.1` |
| `lore-vectors` | `arroy 0.6.4`, `heed` version selected by arroy; optional benchmark feature for `usearch 2.26.0` with Windows target feature split |
| `lore-parse` | `tree-sitter 0.26.11`, `tree-sitter-c-sharp 0.23.5`, plus separately pinned flagship grammars; `rayon 1.11` for bounded CPU parsing |
| `lore-watch` | `notify 8.2.0`, `notify-debouncer-full 0.7.0`, `ignore 0.4`/`globset 0.4` for filtering |
| `lore-git` | `gix 0.86.0` (exact pin after worktree prototype); optional `git2 0.21.0` compatibility backend |
| `lore-embeddings` | `reqwest 0.13.4`, `serde`, `serde_json`, `tokio-retry`-style local policy or a small internal backoff implementation; optional `async-openai 0.18.2` adapter |
| `lore-onnx` | Optional `ort 2.0.0-rc.12` or `fastembed 5.17.4`; keep behind feature or helper-process boundary |
| `lore-config` | `config 0.15.25` + `serde` + `toml 1`, or minimal explicit TOML loader |
| `lore-cli` | `clap 4.6.4`, `anyhow 1.0.104`, `tracing-subscriber 0.3.23`, `tracing-appender 0.2.5` |
| `lore-service` | `windows-service 0.8.1` under `cfg(windows)`, `ipc-lock 0.1.2`; platform-specific autostart modules |
| `lore-test-support` | `insta 1.48.0`, `proptest 1.11.0`, `tempfile 3.27.0`, `criterion 0.8.2` in benchmark-only members |
| Release tooling | `cargo-dist 0.32.0` as CI tool; cargo-wix only if MSI customization exceeds cargo-dist |

## Prototype before committing: highest-risk choices

1. **ANN durability and churn:** benchmark arroy versus usearch under real repository churn, deletes, filtering, restart, antivirus scanning, and Windows service upgrades. The API-level feature comparison is favorable to arroy, but operational behavior is the largest unvalidated native/storage choice.
2. **RMCP 3.0 compatibility:** build both daemon-server and thin-client paths against the 2026-07-28 protocol, run conformance/golden wire tests, and verify the RMCP 3 MSRV and breaking lifecycle changes. Keep an adapter boundary in case the protocol or SDK moves again.
3. **Generation atomicity across SQLite, Markdown, Tantivy, vector index, and graph:** inject crashes at every phase and prove recovery never advertises a partially published generation. This is a system design risk that no crate can solve.
4. **Tree-sitter 0.26 plus many grammar ABIs:** compile and query the complete C#-first grammar set on Windows and Linux, snapshot representative extraction, and test upgrades. Runtime 0.26.11 does not make forward-incompatible grammars safe automatically.
5. **Windows lifecycle/native packaging:** validate SCM stop/restart, named-instance security, long paths, open-handle replacement, rolling logs, ONNX DLL/DirectML behavior, signing, and cargo-dist/cargo-wix artifacts on clean Windows machines before promising frictionless installation.
