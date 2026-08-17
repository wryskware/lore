---
design_status: leaning
last_reviewed: 2026-08-17
decision_refs:
  - D-0004
  - D-0005
  - D-0015
---

# Chunker plugin contract — declarative core, WASM grammars

Outcome of the M2 contract session (2026-08-17), planning against the framing
notes in [[2026-08-16_pluggable-chunkers-brief]]. Wrysk endorsed the mechanism
direction in that session ("declarative + WASM grammars — 100% for it") and
the built-in disposition below; **no ledger entry exists yet**, so this
document is `leaning`: the direction is set, the material doubt is the
wasmtime feasibility spike running against it. A proposed decision entry is
drafted at the bottom, unpromoted per canon rules.

Related: [[3.1_Chunking_and_Ranking]] (current chunking, `leaning`),
[[../1_Architecture/1.2_Ingestion]] (D-0015, decided),
[[../5_Implementation/5.1_Milestones]] (M2 line needs amendment on adoption).

## The shape

> [!working] A plugin is data, not code.
> A chunker plugin is a directory containing a manifest plus assets — no
> executable plugin logic in v1. Core interprets the manifest through the two
> chunking engines it already has. Everything the framing brief listed as
> non-negotiable therefore holds *structurally* rather than by policy:
> plugins cannot emit unstable spans, mint chunk IDs, hand-roll embedding
> headers, or set authority metadata, because plugins never emit anything —
> core does all of it.

A plugin declares, per claimed file extension, one of two **strategies**:

1. **`grammar`** — a tree-sitter grammar compiled to `.wasm`, loaded at
   runtime through the tree-sitter crate's `wasm` feature (wasmtime,
   sandboxed), plus a mapping that fills the same role vocabulary as the
   internal `Spec` (`crates/lore/src/chunk/code/mod.rs`): `path_only`,
   `containers`, `symbols`, `wrappers`, `bodies`, `attachments`,
   `trailing_scope` as TOML string arrays, and declarative name extraction
   (see open question below). The existing language-agnostic walker runs
   unchanged over the loaded grammar.
2. **`windows`** — route the extension to the existing line-window chunker
   with declared caps (max file bytes, window/overlap sizes within
   core-enforced bounds). This is 5.1's "bounded experimental Unity YAML"
   expressed as configuration.

The first real plugin — Wrysk's Unity plugin, authored later in its own repo,
deliberately outside the public release cutoff — needs nothing else:
UXML → XML grammar, USS → CSS grammar, `.unity`/`.prefab`/`.asset` →
capped windows.

> [!candidate] A code escape hatch (WASM chunker logic or subprocess) for
> cases the declarative surface cannot express (the Markdown heading-tree
> fold is the known example). Deliberately deferred until a real case
> demands it; option 1 of the framing brief composes with it if that day
> comes. Do not build speculative hooks for it now.

## Manifest sketch

`lore-plugin.toml` at the plugin root:

```toml
[plugin]
name = "unity"                # registry identity; collision-checked

[[chunker]]
extensions = ["uxml"]
strategy = "grammar"
grammar = "xml.wasm"          # path relative to plugin root
language_tag = "xml"          # value for Chunk::language / header
containers = ["element"]
symbols = ["..."]             # spike + fixtures fill these in
# name extraction: see open question

[[chunker]]
extensions = ["unity", "prefab", "asset"]
strategy = "windows"
max_file_bytes = 262144
```

Field names are illustrative; the implementation package fixes them.

## Identity, versioning, invalidation

- **Chunk identity is unchanged** — `(path, span-anchor, content-hash)`,
  derived by core. Plugins have no way to destabilize it.
- **Plugin fingerprint instead of a declared version.** The plugin's
  effective version is a content hash over its manifest bytes plus every
  referenced asset (`.wasm` files). For files routed to a plugin, the
  indexer folds `CHUNK_FORMAT_VERSION` **and that plugin's fingerprint**
  into the per-file content hash. Any edit to the plugin re-chunks exactly
  the files it owns; core bumps and other plugins' bumps stay independent.
  This answers the brief's per-plugin invalidation question with no version
  bookkeeping for authors to forget.
- Re-chunking after a grammar change re-embeds only chunks whose text
  actually moved, same as today — content-addressed IDs absorb the rest.

## Routing and precedence

- Built-in chunkers win every extension conflict, unconditionally. `.md` in
  particular is never claimable — frontmatter/authority extraction
  (`design_status`, `decision_refs`, D-NNNN scanning) stays core-side, per
  the brief's laundering concern.
- Two enabled plugins claiming the same extension is a loud registration
  error, not a precedence rule.
- A file whose plugin is missing/disabled falls back to the existing unknown-
  text path — but **visibly**: `status` reports per-project counts of files
  that fell back because a named plugin was absent, so the quality cliff the
  brief flagged has a signal.

## Installation, opt-in, remote mode

- Plugins install into the daemon's data dir (`plugins/<name>/`); minimal
  CLI surface (`lore plugin list`, `lore plugin add <path>`) — no registry,
  no fetching, in v1. Distribution is "clone the plugin repo and add it."
- **Per-project opt-in**: `.lore.toml` names the plugins it wants
  (`[plugins] enable = ["unity"]`). Naming an uninstalled plugin is not an
  error — files fall back and `status` says so. Rationale: a chunker sees
  every byte that survived the credential hard-excludes; even with
  sandboxed-data-only plugins, which formats get structural indexing should
  be a repo-visible choice.
- **Remote mode (D-0015):** chunking is daemon-side, permanently, so the
  *receiving* daemon's plugin set governs. The receiver advertises its
  enabled plugin names + fingerprints in the push handshake/status surface;
  the pusher does not negotiate, it only sees. Mismatch is therefore
  observable, never silent.

## Built-ins: marked for migration

Per Wrysk (2026-08-17): the in-tree chunkers **stay built-in for now** but
are actively marked for migration —

- Near term: feature-gate the language grammars in Cargo so builds can drop
  them (`tree-sitter-c-sharp` et al. become optional features), pulling the
  heavy grammars out of the mandatory MSVC build. This also rehearses the
  boundary the plugin crates will need.
- Later: language chunkers move into their own plugin crates riding this
  contract (which also disposes of the Go/C/C++ backlog item in 5.1 as
  "three plugins nobody ships in core").
- **Markdown stays native permanently** — the authority coupling is the
  reason the privileged set exists.
- Tracked as a GitHub issue on wryskware/lore (created alongside this doc).

## Amendments owed if adopted

On promotion of the decision below: 3.1's file-class table drops the
UXML/USS and Unity-YAML rows in favor of a "plugin-routed" row; 5.1's M2
line replaces "UXML/USS first-class; bounded experimental Unity YAML" with
"chunker plugin contract"; 5.1's Go/C/C++ backlog item re-points at plugin
authorship.

## Open questions

> [!open] Declarative name extraction: default to the grammar's `name`
> field (covers most grammars, zero config) with an optional
> `name_field`/`name_kinds` override — or adopt tags-style `.scm` queries
> (`@definition.*`/`@name` captures, the ecosystem convention many grammars
> already ship)? Field-based reuses the walker as-is; queries are more
> expressive and a plausible later convergence target for `Spec` itself.
> Spike evidence and the first UXML fixture should settle it.

> [!open] Whether the wasm feature's build cost (wasmtime is a heavy
> dependency) belongs behind a Cargo feature of core itself, making
> plugin support opt-in at build time. Depends on the spike's measured
> build-time/binary-size deltas.

> [!open] Whether `windows`-strategy caps may exceed core's global bounds
> (`MAX_FILE_BYTES` etc.) or only tighten them. Leaning: tighten-only.

Performance is explicitly *not* an open question: the 2026-08-16 perf
session measured drain (embedding) at ~99% of index wall time; a wasm-speed
parse path is invisible in total index time.

## Proposed decision entry (draft — unpromoted)

Drafted for Wrysk to promote or edit; not an accepted entry.

```markdown
## D-00XX — Chunker plugins: declarative contract, WASM grammars

- **Date:** (on promotion)
- **Status:** Proposed
- **Scope:** Chunking extensibility (amends 3.1 file-class table and the 5.1 M2 line; touches D-0004, D-0005, D-0015)
- **Decided by:** Wrysk (M2 contract session, 2026-08-17)
- **Decision:** File-type support beyond the built-in set arrives via **declarative chunker plugins**: a manifest plus assets, no plugin code. Strategies: tree-sitter grammars compiled to WASM (loaded sandboxed at runtime, mapped onto the internal Spec walker) and configured line-windowing. Core alone derives chunk IDs, builds embedding headers, and extracts authority metadata; built-ins win extension conflicts and `.md` is never claimable. Per-plugin invalidation via content fingerprint (manifest + assets) folded into the per-file hash. Plugins install daemon-side (receiver-side in remote mode, advertised in the push surface); projects opt in via `.lore.toml`; absent-plugin fallback is surfaced in `status`, never silent. Unity support (UXML/USS/serialized YAML) ships as the first out-of-tree plugin, not as core features. In-tree language chunkers are marked for migration (feature-gate, then plugin crates); Markdown stays native.
- **Rationale:** Data-only plugins make the brief's constraints structural instead of policed; the measured embedding-bound perf budget absorbs wasm parse cost; WASM grammars are single-artifact and sandboxed where native DLLs are neither.
- **Supersedes:** None (amends the 3.1/5.1 prose listed in Scope).
- **Canonical sources:** [[../3_Retrieval/2026-08-17_chunker-plugin-contract]]; [[../3_Retrieval/2026-08-16_pluggable-chunkers-brief]]
```

## Implementation plan

- **Phase 0 — spike (running, 2026-08-17).** tree-sitter 0.26 `wasm`
  feature on MSVC: load an XML grammar from `.wasm`, parse sample UXML,
  span exactness, build-time/size deltas, grammar-sourcing friction.
  Outcome feeds the open questions above; an infeasible verdict reopens
  the mechanism fork (native grammar DLLs are the named fallback).
- **Phase 1 — core seam (lore repo).** Plugin registry + manifest parsing;
  routing in `chunk_file` after built-ins, before fallback; fingerprint in
  the indexer file hash; `status`/push-surface advertisement; fallback
  counters; `lore plugin list/add`. Fixture-driven tests with a toy plugin
  checked into the test tree. Test authoring is its own pass per working
  rules.
- **Phase 2 — Unity plugin (new repo, e.g. `wryskware/lore-unity`).**
  Wrysk authors this later, post-cutoff, as the contract's first real
  consumer: XML/CSS wasm grammars, UXML/USS mappings, windowed serialized
  YAML, fixtures from a real Unity project; dogfood against Lexomancy.
  The contract is done only when this needs zero core changes.
- **Phase 3 — adoption bookkeeping.** Promote the decision entry (Wrysk),
  amend 3.1/5.1, start the built-in feature-gating from the migration
  issue.
