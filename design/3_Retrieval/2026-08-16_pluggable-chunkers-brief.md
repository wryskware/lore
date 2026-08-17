---
design_status: exploration
last_reviewed: 2026-08-16
decision_refs:
  - D-0004
  - D-0015
---

# Pluggable chunkers — framing notes for the M2 contract session

Input notes for a planner thread, captured from the 2026-08-16 orchestration
conversation. **Nothing here is decided.** Wrysk's proposal, in his words: the
base install does not need to know about every file type under the sun and
never could; authoring an independently maintained chunking plugin should be
easy. He intends to write a Unity plugin himself, at a later date and
deliberately outside the public release cutoff.

Related: [[3.1_Chunking_and_Ranking]] (current chunking design, `leaning`),
[[../1_Architecture/1.2_Ingestion]] (D-0015, decided).

## Why this came up

The M2 milestone in [[../5_Implementation/5.1_Milestones]] lists "UXML/USS
first-class" and "bounded experimental indexing of serialized Unity YAML" as
core features, and 3.1's file-class table names them in-tree. Wrysk's read is
that those are the wrong shape: Unity support is a *plugin question*, not a
core feature, and the same is true of the Go/C/C++ additions the milestone
backlog lists. What belongs in M2 is the **contract**, not any particular
file type.

If a plugin contract is adopted, 3.1's file-class table and 5.1's M2 line
both need amendment — the UXML/USS/YAML rows describe in-tree work that would
move out of tree.

## Constraints the contract has to respect

These are properties of the system as it stands today, not opinions.

- **Chunk identity is content-addressed** — `(path, span-anchor, content
  hash)`, so re-parsing an unchanged file yields identical IDs and the stored
  vectors and FTS rows survive (3.1, "Chunk identity"). A plugin that emits
  unstable spans re-churns embeddings on every pass. That is the CodeGraph
  counter-churn anti-pattern, and it is expensive: embedding is the
  bottleneck, not chunking.
- **Cache invalidation rides `CHUNK_FORMAT_VERSION`.** The constant (in
  `chunk/mod.rs`) rides in the indexer file hash, so bumping it forces a
  re-chunk of everything. A plugin's own version must participate in this,
  and ideally *per-plugin* — a plugin author's revision should not invalidate
  chunks produced by core or by unrelated plugins.
- **Frontmatter/authority extraction must not be plugin-overridable.** The
  Markdown chunker records `design_status`, `decision_refs`, and D-NNNN
  references as chunk metadata, and that metadata feeds authority weighting
  and ledger validation. A plugin able to declare `design_status: decided` on
  its own output would launder authority into canon. Whatever the seam is,
  this stays on core's side of it.
- **Chunking is daemon-side, permanently.** D-0015 is explicit that clients
  send content and never chunks. Plugins therefore install on whichever
  daemon owns the index — in remote mode that is the *server's* plugin set,
  not the pusher's. Two daemons with mismatched plugin sets is a state the
  contract has to have an answer for.
- **The embedding-text header is core's construction.** Chunk text is
  prefixed with language, relative path, and symbol kind/name or
  `heading_path` before embedding (3.1). Likely division: the plugin returns
  spans plus structured metadata, core builds the header. Worth confirming —
  a plugin that hand-rolls its own header shape degrades retrieval quality
  invisibly.

## The performance budget is generous

Measured in the 2026-08-16 indexing-perf session: walk + chunk + store is
roughly free (~40k chunks in under 15s), and drain — sending text to the
embedding server — is 99% of index wall time. The honest ceiling is
llama.cpp's, not lore's.

This means a plugin mechanism can afford to be *much* slower per file than
the in-process tree-sitter path without moving total index time. That widens
the viable mechanism set considerably and should be stated explicitly to the
planner, because the instinct will be to optimize a path that has slack.

## Mechanism options, roughly cheapest-first

Not a recommendation set — the axes are safety, distribution friction, and
Windows build cost (the C++ grammar was already flagged in 5.1 for MSVC build
time and binary size).

1. **Declarative / config-only.** A plugin is a tree-sitter grammar plus a
   mapping table from node types to chunk kinds, with no executable code at
   all. Most chunkers genuinely are just "this grammar, and these node types
   are symbols." Would plausibly cover UXML and USS via XML/CSS grammars
   without anyone writing a line of Rust. Cannot express the harder cases
   (the Markdown heading-tree fold, the bounded-window YAML experiment).
2. **WASM.** tree-sitter grammars already compile to WASM; sandboxed, no ABI
   pain, cross-platform distribution is a single artifact. Runtime cost is
   real but the budget above absorbs it.
3. **Subprocess over stdio.** Simplest to author in any language, highest
   per-file overhead, trivially sandboxable at the OS level.
4. **Native dynamic library.** Fastest and worst: Rust has no stable ABI,
   crashes take the daemon with them, and Windows DLL distribution is its own
   tax.

Option 1 composes with the others — a declarative fast path plus a code
escape hatch is a plausible shape.

## Open questions for the session

- Does the contract cover only *chunking*, or also file-type detection,
  ignore defaults, and language-specific embedding-header hints?
- Per-plugin cache versioning: is it a plugin-declared version string folded
  into the file hash alongside `CHUNK_FORMAT_VERSION`?
- Distribution and trust: where do plugins come from, and is running one an
  explicit per-project opt-in? A chunker sees every byte of the repo,
  including whatever survived the credential hard-excludes.
- Remote mode: how does a pusher discover the receiving daemon's plugin set,
  and what happens when a file type the pusher expects to be chunked is
  unknown to the server? Silent fallback to line windows is a quality cliff
  with no signal.
- Do the in-tree chunkers (C#, Rust, Python, JS/TS, Markdown) get ported onto
  the plugin contract, or does the contract sit beside a privileged built-in
  set? Dogfooding argues for the former; the Markdown authority coupling
  argues against.
- Does this supersede the Go/C/C++ chunking backlog item in 5.1 by turning it
  into three plugins nobody has to ship in core?
