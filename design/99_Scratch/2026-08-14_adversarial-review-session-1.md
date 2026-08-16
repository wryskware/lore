---
design_status: exploration
last_reviewed: 2026-08-14
---

# Adversarial Review — Session 1: Architecture and Design Conformance

## Findings

### 1. Two daemons can pass admission concurrently and become simultaneous index owners

- **Severity:** critical
- **Confidence:** high
- **Binding constraint:** D-0003 — exactly one authoritative owner of index state
- **Locations:** `crates/lore/src/daemon/handshake.rs:101`, `crates/lore/src/daemon/mod.rs:106`, `crates/lore/src/daemon/mod.rs:116`, `crates/lore/src/store/mod.rs:235`
- **Failure scenario:** Start daemon A and daemon B against the same empty (or stale) data directory at the same time. A reads no live handshake in `preflight`; B reads the same state before A publishes. Both return `Ok(())`, both open the same SQLite file in WAL mode, both bind listeners, and both atomically replace `daemon.json` with their own record. The last writer becomes discoverable, but the loser keeps running its watcher, indexer, embed worker, and heartbeat. SQLite serializes individual writes; it does not restore the one-owner invariant. The same check-then-publish race exists when two processes simultaneously take over one stale record.
- **Why this is architectural:** `daemon.json` is a discovery/liveness record, not mutual exclusion. The code calls it a mutual-exclusion token, but there is no atomic claim and no lifetime-held OS primitive. D-0003 calls a second writer disqualifying by construction.
- **Cheap-now fix:** Acquire a Windows-safe, process-lifetime lock before `preflight` and hold it until shutdown. A named mutex or an exclusively locked file handle is crash-releasing; the heartbeat remains useful for discovery and diagnosis but should not be the admission primitive. Add a barrier-based two-starter test.

### 2. Vault authority is self-certified by frontmatter and path-blind, enabling authority laundering

- **Severity:** major
- **Confidence:** high
- **Binding constraint:** D-0001 authority order and promotion gate; D-0004 schema awareness
- **Locations:** `crates/lore/src/chunk/markdown.rs:119`, `crates/lore/src/chunk/markdown.rs:162`, `crates/lore/src/chunk/common.rs:228`, `crates/lore/src/daemon/search.rs:266`, `crates/lore-mcp/src/server.rs:26`
- **Failure scenario A:** Add any Markdown file containing `design_status: decided`, with no active decision reference and no ledger authorization. The chunker accepts the word, the store assigns the highest authority tier, search emits `design_status: decided`, and the MCP schema tells the model that `decided` is settled canon. A file edit has therefore crossed the promotion gate without a ledger entry.
- **Failure scenario B:** Search for architecture terms that occur in `design/99_Scratch/2026-08-14_adversarial-review-briefs.md`. The file has no frontmatter but cites many D-numbers. Every Markdown chunk receives `VaultMeta`, body references are extracted, and `authority_weight` promotes an unclassified chunk with any D-number to the leaning multiplier. The path is never consulted even though D-0001 places all `99_Scratch` material below normal exploration and the memory design explicitly groups `99_Scratch` with deprecated material.
- **Why this is architectural:** The retrieval layer treats a declaration or citation as proof of authority. Canon says authority comes from the active ledger and its authorized documents; merely quoting a decision is not inheritance.
- **Cheap-now fix:** Compute an explicit, validated authority classification at index time. At minimum, special-case the ledger, require `decided` documents to cite an active D-entry, distinguish declared status from validated authority, and force `99_Scratch` to the bottom tier regardless of citations. Surface invalid declarations instead of silently promoting them.

### 3. Duplicate project names make search results impossible to expand reliably

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/store/schema.rs:40`, `crates/lore/src/store/mod.rs:262`, `crates/lore/src/daemon/mod.rs:64`, `crates/lore/src/daemon/search.rs:319`, `crates/lore/src/daemon/http.rs:293`
- **Failure scenario:** Register `C:\\repos\\a\\shared` and `D:\\work\\shared` without explicit names. Both are accepted with display name `shared` because only `root` is unique. A search hit from the second project returns `project: "shared"`. The instructed `expand(project, chunk_id)` call resolves the first matching name and looks for the second project's chunk there, returning 404. If both projects contain the same relative path/anchor/text, their content-addressed chunk IDs also coincide and `expand` can silently cite the first project instead.
- **Why this is architectural:** The wire uses a non-unique display label as the round-trip identity while a SQLite-assigned integer ID is available but omitted from `SearchResult`.
- **Cheap-now fix:** Enforce unique project names at registration or return a stable opaque project key in search results and require that key for expand. Keep the display name presentation-only. Do this before session paths and issue-source identities depend on the same resolver.

### 4. The v1 discovery contract cannot support the promised `/v1` + `/v2` coexistence

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore-core/src/discovery.rs:32`, `crates/lore-core/src/discovery.rs:46`, `crates/lore/src/daemon/http.rs:4`, `crates/lore/src/cli.rs:201`, `crates/lore-mcp/src/daemon.rs:63`
- **Failure scenario:** Ship a breaking v2 daemon while retaining `/v1` for an older MCP proxy, as the HTTP module says versioned routes permit. The handshake has one scalar `api_version`, so if it advertises 2 the old proxy rejects the daemon before trying the still-supported `/v1`. The new proxy accepts version 2 but `Handshake::base_url` is hard-coded to `/v1`, so changing `API_VERSION` alone still routes it to v1. A scalar equality check and a hard-coded path defeat coexistence.
- **Cheap-now fix:** Have discovery advertise supported versions/endpoints (or an unversioned discovery endpoint) and let a client select a common version. Make the selected route data-driven. If named pipes remain an earned M4 option, advertise transport endpoints rather than only a TCP port.

### 5. The persisted model cannot represent the M3 session ledger's source semantics or ranking contract

- **Severity:** major (future blocker)
- **Confidence:** high
- **Binding constraints:** D-0006 and D-0008
- **Locations:** `crates/lore/src/store/schema.rs:47`, `crates/lore/src/store/schema.rs:55`, `crates/lore/src/types.rs:53`, `crates/lore/src/daemon/http.rs:209`, `crates/lore/src/daemon/mod.rs:176`, `crates/lore/src/daemon/search.rs:266`
- **Failure scenario:** Implement `session_log` by writing `%LOCALAPPDATA%\\lore\\...\\sessions\\<thread>.md` and attempt to index it "like everything else." Every file/chunk currently requires a repo project foreign key, while HTTP refuses to register the data directory and startup watches/rescans every registered project root. Even if M3 inserts a synthetic project directly, a session chunk has no source/corpus discriminator or session timestamp; as unclassified Markdown it receives neutral authority (or leaning if it mentions a D-number) and the ranking pipeline cannot apply recency or cap it below vault material. `recall` also cannot filter sessions without path/name conventions leaking through the store API.
- **Cheap-now fix:** Add engine-neutral document provenance now: stable source/project key, source kind (`repo`, `session`, later `issue`), source timestamp, and declared-versus-effective authority. Put source-kind filtering and ranking inputs in the store seam before v1 data and wire types harden. The session writer/watcher itself can wait for M3.

### 6. `SearchStore` is a module boundary, not a replaceable engine seam

- **Severity:** major (M4 debt)
- **Confidence:** high
- **Locations:** `crates/lore/src/store/mod.rs:5`, `crates/lore/src/store/mod.rs:70`, `crates/lore/src/store/mod.rs:220`, `crates/lore/src/daemon/store_handle.rs:32`, `crates/lore/src/daemon/search.rs:28`, `crates/lore/src/daemon/expand.rs:19`
- **Failure scenario:** Implement a Tantivy+arroy engine at M4. There is no trait or engine-neutral handle to implement: `StoreHandle` contains concrete `Store`, search and expand accept concrete `Store`, and the public `StoreError` directly contains `rusqlite::Error` and `rusqlite_migration::Error` despite the module documentation claiming no SQLite type crosses the public surface. Project identity is an `i64`, lexical sanitization is hidden inside the SQLite implementation, and `replace_file_chunks` promises one-transaction behavior that a split lexical/vector engine cannot literally reproduce. The replacement touches daemon signatures, error plumbing, lifecycle construction, and semantic policy rather than being another implementation of one seam.
- **Cheap-now fix:** Define a small engine-neutral trait around the operations actually used by daemon/search/embed, give it an engine-neutral error, move query normalization/fusion policy above it, and document logical atomicity rather than SQLite transaction mechanics. Do not build the alternative engine now.

### 7. The project registry is durable configuration stored only inside the allegedly replaceable index database

- **Severity:** minor (operational and M4 debt)
- **Confidence:** high
- **Locations:** `crates/lore/src/store/schema.rs:40`, `crates/lore/src/config.rs:31`, `crates/lore/src/daemon/mod.rs:176`
- **Failure scenario:** Delete/rebuild a corrupt `lore.db`, or replace it with a new engine store. Source files and vault documents still exist, but Lore has lost the only list of roots it needs to rebuild the index; startup sees zero projects and cannot reconstruct anything until the user manually repeats every `lore add`. The DB is rebuildable only if external knowledge of its inputs survives.
- **Cheap-now fix:** Persist the registry as daemon configuration/manifest outside the derived engine state, with stable project keys. Let each engine rebuild from that manifest.

### 8. The decided MCP design promises a path glob, while v1 implements a literal prefix

- **Severity:** minor
- **Confidence:** high
- **Locations:** `design/4_Interfaces/4.1_MCP_Surface.md:13`, `crates/lore-core/src/lib.rs:113`, `crates/lore-mcp/src/server.rs:61`, `crates/lore/src/store/query.rs:69`
- **Failure scenario:** A client implemented from the decided interface document sends a glob such as `Assets/**/Tests/*.cs`. v1 normalizes it as `path_prefix` and compares it literally with `substr`, so it returns no matches. The agent-facing JSON schema accurately says prefix, but the binding interface document and wire contract disagree.
- **Cheap-now fix:** Either correct the decided interface document with Wrysk's authorization or implement a true `path_glob` field. Do not silently rename one semantic into the other.

## Smells and "could be nicer" observations

- `Store::open` and every mutator are public through `pub mod store`; even after the daemon race is fixed, an external crate or future offline CLI can attach to WAL and become a second writer. Prefer crate-private mutation/opening plus a guarded daemon constructor, or enforce the same ownership lock in the store itself.
- CLI and MCP duplicate HTTP client, discovery/version checks, response decoding, and transport error classification. They already differ in whether heartbeat freshness is cached or reread; M4 transport work will duplicate again. An engine-free client in `lore-core` (or a sibling client crate) would make transport a real seam.
- Wire project IDs are SQLite-shaped `i64` values, while comments describe them as stable script handles. A rebuild or engine migration can renumber them. Use a persisted opaque ID independent of backend row IDs.
- `resolve_project` intentionally lets a numeric display name shadow the same numeric ID. That is predictable but makes the supposedly stable ID unaddressable after an unrelated project is named with those digits.
- `RegisterProjectRequest` documents an absolute root, but the daemon accepts relative paths and resolves them against the daemon's working directory, not the client's. Reject relative roots at the HTTP boundary.
- All durable paths are forced through `Utf8PathBuf`. This is pleasant internally but excludes legal Windows paths containing ill-formed UTF-16; decide explicitly whether that is an accepted Windows-native limitation.
- Before M3 adds `session_log`, revisit loopback-without-auth. Local read access may be acceptable, but a write endpoint that can poison recall or fill the data directory changes the threat model.
- Additive response fields are friendly to old Serde clients because unknown fields are ignored, but new clients reading old responses need defaults/optional fields. Establish a compatibility rule before M3 extends status and provenance.

## Pay now vs. pay at M3/M4

| Rank | Debt | Pay | Cheap-now action |
|---:|---|---|---|
| 1 | Daemon admission is check-then-publish | **Now** | Hold a crash-releasing OS ownership lock and add a simultaneous-start test. |
| 2 | Authority is derived from unvalidated declarations/citations | **Now** | Separate declared status from effective authority; validate ledger refs and demote `99_Scratch` by path. |
| 3 | Project display name doubles as wire identity | **Now** | Introduce a stable opaque project key and make names unique/presentation-only. |
| 4 | No source kind/timestamp/effective-authority model | **Now, before M3 schema** | Add provenance fields and source filters; leave session capture for M3. |
| 5 | Discovery advertises one version and one HTTP port | **Now, while v1 has one client generation** | Negotiate supported endpoints/versions; make transport data-driven. |
| 6 | Concrete SQLite types define the store boundary | **Now at interface level** | Add the engine-neutral trait/error and policy boundary; do not implement Tantivy/arroy yet. |
| 7 | Project registry lives in engine state | **Now or early M3** | Move roots/stable IDs to a small daemon manifest and rebuild engine state from it. |
| 8 | Session retention, compaction, and Stop-hook behavior | **M3** | These remain genuinely open product policy; avoid freezing them in M1 schema. |
| 9 | Tantivy+arroy, managed llama-server, named-pipe implementation | **M4, only if earned** | Preserve seams now; defer code and dependencies until measurement demands them. |

