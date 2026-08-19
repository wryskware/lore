---
design_status: exploration
last_reviewed: 2026-08-18
---

# Distilled knowledge cards — v0 experiment

An exploration, not a decision. Nothing here binds until Wrysk says so.

Question it answers: *does a layer of agent-generated, intent-level prose
summaries ("cards"), indexed alongside the source it summarizes, improve
retrieval on the queries where raw-chunk search is weakest — without drowning
the source it points at?*

## Concept

A cheap-tier background model scans a repository and writes one Markdown card
per code area: what the area is for, how its pieces relate, which invariants
it maintains — phrased at the level a person asks questions, not at the level
identifiers are spelled. The cards are plain files in the tree, so the
existing walker/chunker/embedder pipeline indexes them with **zero daemon
changes**. Search then bridges intent-shaped queries to cards, and cards carry
anchors back to the exact sources they summarize.

Motivation from the bench record: the lexical floor scores 1.00 on
`lexical`/`symbol`/`docs` kinds but 0.23 on `semantic` (2026-08-17,
lore-bench), and e2e pass-B verdicts repeatedly show topically-relevant hits
that were not sufficient to answer intent-level questions. Cards target
exactly that gap.

Design lineage (all previously discussed, none decided):

- Cards are repo-resident Markdown, consistent with the D-0006 posture that
  durable knowledge lives in files and the DB stays a derived index.
- Cards are **routers, not answers**: every claim carries a source reference,
  so a wrong claim costs a wasted hop rather than a wrong belief. This is the
  working mitigation for agent-generated-content laundering
  ([[../7_Research/01_landscape]]: provenance defaults untrusted).
- Anchors are `(path, content hash)`, not chunk ids — chunk ids churn with
  content and chunker versions; path+hash makes staleness mechanically
  detectable later (a dangling anchor marks the card stale).

Deliberately **out of scope for v0** (daemon-side, only worth building if the
experiment shows a margin):

- A `distilled` authority class / path cap, and a search-response **lane**
  separating card hits from source hits (with a `distilled: off | lane`
  request knob). v0 cards are unclassified exploration material interleaved
  in one ranking — the *harshest* placement for them, which biases the
  experiment against the feature, not toward it.
- Watcher-driven incremental regeneration on source-hash churn.
- Reverse lookup ("which cards cite this file").
- Typed graph edges beyond card→source anchors.

## Card format (v0)

One card per area, `distilled/<area-slug>.md` at the corpus root:

```markdown
---
distilled: v0
generator: bench/distill/distill.py
model: qwen3.8:latest (ollama)
generated: 2026-08-18
area: crates/lore/src/daemon
sources:
  - path: crates/lore/src/daemon/index.rs
    sha256: 6b86b273ff34
  - path: crates/lore/src/daemon/queue.rs
    sha256: d4735e3a265e
---

# Daemon indexing pipeline

Intent-level prose, 150–400 words. Claims name their source file in
backticks inline. No identifier dumps, no copied code.
```

- `sources` is the authoritative anchor list (path + first 12 hex of the
  file's sha256 at generation time); inline backtick paths are for the
  reading agent, frontmatter is for tooling.
- Target length keeps a card to 1–2 chunks so it survives chunking coherent.
- Granularity: one card per directory of indexable source; directories with
  fewer than 3 files merge into their parent's card; oversized groups split.
  This is a guess — granularity is one of the things the bench can compare
  later if v0 shows any signal.

## Experiment protocol

Third tree beside the existing slots, so nothing frozen is touched:

- `bench-e2e\lore-bench-d` = detached worktree at the same pin (`977364a`)
  plus the identical slot-a deltas (empty `.loreignore`, scrub deletion,
  `.lore.toml` with `name = "lore-bench-d"` and the same `[authority]`
  table), plus `distilled/` as untracked files.
- `corpora.json` gains a `lore-bench-d` entry; `queries/lore-bench-d.json`
  is the lore-bench key verbatim (same ids, same relevant paths — the tree
  layout is identical, cards only add files).
- One `run.ps1 -Models qwen3-4b -Corpora lore-bench,lore-bench-d` session:
  same binary, same daemon instance, same embedder (D-0014 incumbent), both
  arms drained together. Control and treatment differ *only* in the cards.

Scored two ways, bracketing the effect:

1. **Raw** (pessimistic): cards count as noise. A card outranking its own
   source pushes the key path down and the metrics say so. This measures the
   colonization risk directly.
2. **Card-credit** (optimistic): a post-processor rewrites the recorded
   searches, replacing each `distilled/` path with that card's frontmatter
   source paths in listed order (then the scorer's existing first-occurrence
   dedup applies). This models an agent that follows the card's anchors in
   one hop. Emitted as a synthetic run dir so `score.ps1` is unchanged.

Read the per-kind breakdown before the aggregate: the hypothesis predicts a
gain concentrated in `semantic` (and possibly `design`) kinds, little or no
movement on `lexical`/`symbol`, and the raw-vs-credit gap shows how much of
the gain depends on the (not yet built) lane/anchor UX. Also record how often
cards appear in top-10 at all — a layer nobody retrieves is dead weight
regardless of quality.

Precision (judged-pool) metrics are deferred: the pool has no labels for card
paths, and judging them is only worth the labels if recall shows a margin.

## Generator

`bench/distill/distill.py` — standalone, no daemon involvement. Walks the
corpus (respecting the same obvious exclusions the walker enforces), groups
files into areas, prompts a local model through Ollama's OpenAI-compatible
endpoint (`qwen3.8:latest`; the RunPod 27B arm was down on 2026-08-18 and a
local cheap model is the more honest simulation of a background distiller
anyway), and writes cards. Deterministic file grouping; one model call per
card; failures skip the card and are listed at the end, so a partial run is
usable and resumable.
