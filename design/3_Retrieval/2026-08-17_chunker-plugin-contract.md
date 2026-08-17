---
design_status: decided
last_reviewed: 2026-08-17
decision_refs:
  - D-0023
  - D-0004
  - D-0005
  - D-0015
---

# Chunker plugin contract — declarative core, WASM grammars

Outcome of the M2 contract session (2026-08-17), planning against the framing
notes in [[2026-08-16_pluggable-chunkers-brief]]. Wrysk chose the mechanism in
that session ("declarative + WASM grammars — 100% for it"), the contract was
implemented and dogfooded the same day, and Wrysk promoted the decision:
**this document is the canonical source of [[../0_Canon/DECISIONS#D-0023 — Chunker plugins: declarative contract, WASM grammars|D-0023]]**,
which binds the summary there; prose here that goes beyond the entry is
detail, not additional requirement.

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
   memory-isolated — see the spike's sandboxing caveat), plus a mapping that fills the same role vocabulary as the
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

Executed with D-0023's promotion (2026-08-17): 3.1's file-class table drops the
UXML/USS and Unity-YAML rows in favor of a "plugin-routed" row; 5.1's M2
line replaces "UXML/USS first-class; bounded experimental Unity YAML" with
"chunker plugin contract"; 5.1's Go/C/C++ backlog item re-points at plugin
authorship.

## Spike results (2026-08-17): FEASIBLE, with caveats

Worktree spike (branch `worktree-agent-ab7cba9785182e68a`, unmerged), Opus
agent, findings **parent-verified by re-running the demonstrating binary**:
tree-sitter 0.26.12's `wasm` feature (wasmtime 36.0.13 via its C API) loads
`.wasm` grammars at runtime on Windows/MSVC. XML grammar (ABI 14, prebuilt
release asset) parses sample UXML with `has_error false` and every byte span
slicing the source exactly; CSS grammar (ABI 15) parses USS including
`-unity-*` properties. Wasm parse ≈ 1.63× native — noise under the
embedding-bound budget. Runtime accepts grammar ABI 13..=15.

**Distribution is better than hoped.** Most grammars ship `.wasm` release
assets (xml, css, c-sharp, yaml, toml, json, rust, python, typescript —
verified; **hlsl does not**), and where they don't, `tree-sitter build
--wasm` (CLI 0.26) needs no Emscripten/Docker — it auto-downloads wasi-sdk
once (~510 MB cached) and builds in under a second. A plugin author's whole
toolchain is `npm i tree-sitter-cli`.

Caveats and consequences for Phase 1:

- **MSVC toolset prerequisite — CLEARED 2026-08-17.** Toolsets below 14.33
  lack C11 `<stdalign.h>` and cannot compile the feature (the spike
  initially shimmed around 14.32). Wrysk updated the dev box to 14.44 the
  same day; the spike then built and ran unmodified with the shim deleted,
  and GitHub-hosted CI runners already carry a current toolset. Expect ~40
  benign LNK4217 warnings.
- **Cost is real: +42 s clean build, +10.2 MiB binary, +101 crates**
  (Cranelift). This settles the former open question: **wasm plugin support
  goes behind a Cargo feature of core** so grammar-bundled-only builds
  don't pay it. (The spike crate currently defaults the feature on and sits
  in the workspace members — flip before any merge.)
- **Sandboxed means memory-isolated, NOT resource-bounded.** Verified:
  tree-sitter never arms wasmtime fuel or epoch deadlines, so both trap
  during its own stdlib instantiation and are unusable; a hostile grammar's
  external scanner that never returns hangs the thread (reasoned from call
  sites, not demonstrated). The contract text must not promise timeouts;
  if bounding a runaway grammar ever matters, that is a killable worker
  process, not the wasm engine. Declarative-data plugins keep this a
  malice-only concern, not an accident-prone one.
- **`Spec::language: fn() -> Language` cannot express a wasm grammar** —
  a wasm-backed `Language` comes from a fallible call against a live
  `WasmStore`. The walker itself is grammar-agnostic; only that field and
  `chunk_code`'s throwaway `Parser::new()` change.
- **Parser construction becomes stateful per thread.** One shared `Engine`;
  one `WasmStore` + `Parser` per thread (a store must never be shared, and
  `set_wasm_store` moves it); `Language` clones freely across stores.
  Store creation ~7–10 ms + 16–60 ms per-grammar load argues for
  thread-local pooling, not per-file construction.
- **Loader validation is cheap and clean:** bad artifacts fail with
  `WasmErrorKind::Parse`, no panic; grammars must be side modules with a
  `dylink.0` section (i.e. produced by `tree-sitter build --wasm`).
  Manifest should carry the expected ABI; gate on `Language::abi_version()`
  + `is_wasm()` at load.

## Open questions

> [!open] Declarative name extraction: default to the grammar's `name`
> field (covers most grammars, zero config) with an optional
> `name_field`/`name_kinds` override — or adopt tags-style `.scm` queries
> (`@definition.*`/`@name` captures, the ecosystem convention many grammars
> already ship)? Field-based reuses the walker as-is; queries are more
> expressive and a plausible later convergence target for `Spec` itself.
> The first UXML fixture should settle it.

> [!open] HLSL/ShaderLab for Unity: `tree-sitter-hlsl` publishes no `.wasm`
> release asset, so the Unity plugin would build its own (cheap, per the
> spike) — and `.shader` ShaderLab has no obvious grammar at all; likely a
> `windows` strategy candidate. Scope question for the Unity plugin, not
> the contract.

> [!open] Whether `windows`-strategy caps may exceed core's global bounds
> (`MAX_FILE_BYTES` etc.) or only tighten them. Leaning: tighten-only.

Performance is explicitly *not* an open question: the 2026-08-16 perf
session measured drain (embedding) at ~99% of index wall time; a wasm-speed
parse path is invisible in total index time.

## Decision entry

Promoted by Wrysk as **D-0023** (2026-08-17) — see
[[../0_Canon/DECISIONS]]. The draft that previously sat here moved into the
ledger, updated to the implemented reality (enabled-scope contest
settlement, fingerprint-only versioning, the first-consumer consequences).

## Implementation plan

- **Phase 0 — spike. DONE (2026-08-17): FEASIBLE**, results above. Spike
  code stays on its unmerged worktree branch as Phase 1 reference (it
  carries the MSVC shim and a default-on wasm feature that must not merge
  as-is).
- **Phase 1 — core seam. DONE (2026-08-17)**, shipped in this branch's
  commits: registry + manifest parsing, both strategies through the
  existing walker/windower, per-thread wasm parser pooling, ABI gate,
  fingerprint-in-stamp invalidation, per-project opt-in, `status` +
  push-lease advertisement, fallback counters, `lore plugin list/add`,
  `wasm-grammars` Cargo feature (default-on), toy fixture plugin.
  Built by two implementation packages plus an independent adversarial
  test pass; suite 479 → 607.
- **Phase 2 — Unity plugin. AUTHORED (2026-08-17)** at
  `wryskware/lore-unity` (private): XML/CSS wasm grammars, UXML/USS
  mappings derived from dumps of all 54 real Lexomancy UI files (691
  chunks, zero parse errors, zero name fallbacks), measured YAML windows
  cap, `.shader` windows entry. Authoring it fed five schema fixes back
  into Phase 1 before integration — which is what dogfooding a contract
  is for. Live Lexomancy dogfood is the acceptance step.
- **Phase 3 — adoption bookkeeping.** Promote the decision entry (Wrysk),
  amend 3.1/5.1 (held until promotion — both are decided canon), start
  the built-in feature-gating from the migration issue (#25).

## What the first consumer taught the contract (recorded 2026-08-17)

Resolved during implementation:

- **Conflict scope**: contests are settled against the plugins a project
  *enabled*, per this doc's wording — install-scope resolution was
  implemented first and rejected in review because a machine-wide install
  could re-chunk a project that never asked for anything.
- **Whitespace-preserving grammars** (XML's `CharData`) need whitespace
  nodes as attachments for comment attachment to work at all, which
  silently disabled the blank-line guard; fixed — a pure-whitespace
  attachment node is treated as the gap it reifies.
- **`symbol` defaults from the grammar filename stem**, not
  `language_tag`: the right tag names the *format* (`uxml`), the entry
  point names the *grammar* (`xml`).

Open items, none blocking:

- No manifest field for grammar provenance (upstream repo/release/ABI);
  it lives in the plugin's README, but `lore plugin list` is where a user
  would want it.
- Deeply nested markup yields repetitive symbol paths
  (`ui:UXML.ui:VisualElement.ui:VisualElement…`) in the embedding header;
  no mechanism says "nests but contributes no path segment" (`path_only`
  is the inverse). Real for markup generally.
- Only the literal `md` extension is walled; `markdown`/`mdx` are
  claimable. Not laundering — `VaultMeta` has no code path from a plugin
  — just an authority-free route for Markdown-family files. Pinned by
  test.
- Promoting an extension to built-in later retroactively voids any
  claiming chunker entry *including its unrelated extensions* — loud in
  diagnostics, but an upgrade re-chunks that corpus.
- `lore plugin add` validation loads the manifest but does not void
  built-in claims; the daemon voids them at load with a diagnostic. The
  wall holds (three layers deep at routing), the asymmetry is cosmetic.
- Remote mode: a pushed `.lore.toml` is inert content — the RECEIVER's
  on-disk copy governs enablement, consistent with "the receiving
  daemon's plugin set governs" but worth knowing when a remote project
  seems to ignore its committed config.
- tree-sitter-css 0.25 flags Unity 6's media range syntax
  (`@media (width < 800px)`) as an ERROR node; spans stay exact and
  inner rules chunk correctly.
- Size caps are standing in for a path rule in the YAML windows entry;
  the deferred `path_glob` remains the honest tool if generated blobs
  ever come in small.
