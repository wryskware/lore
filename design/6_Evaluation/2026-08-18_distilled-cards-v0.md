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

## Results — run 20260818-212430 (qwen3-4b, 25 queries, n=1 run)

16 cards generated (local qwen3.8 via Ollama, ~25 min wall); every card
survived chunking as exactly one chunk (+16 chunks on a 1,636-chunk corpus).
Cards were retrieved 23 times across the 25 queries' recorded top-20 and
appeared in the top-10 of 14 queries — the layer is not dead weight.

| arm | hit@5 | hit@10 | MRR@10 | nDCG@10 | semantic hit@10 |
|---|---|---|---|---|---|
| control (lore-bench) | 0.92 | 0.92 | 0.761 | 0.771 | 0.8 |
| cards, raw (pessimistic) | 0.88 | 0.92 | 0.740 | 0.755 | 0.8 |
| cards, card-credit (optimistic) | 0.92 | **0.96** | 0.741 | 0.765 | **0.9** |

Per-query key ranks: **22/25 queries unchanged in all three arms.** The
entire aggregate movement is three queries:

- **LB-01** (semantic: "what stops two copies of the background service
  from both writing to the same index at once") — the control arm's hardest
  miss: key absent from top-10. With cards, two cards land in the top-10 and
  anchor-following surfaces the key at rank 6. Miss → hit is the one real
  win, and it is exactly the query shape the concept targeted.
- **LB-13** (lexical, FTS5 tokenizer) — rank 1 → 2 raw (one card displaced
  the source) → 3 under card-credit (naive expansion inserts *all* of a
  card's anchors above the source). The one real cost.
- **LB-07** (semantic) — 5 → 6 raw (crosses the hit@5 boundary; the whole
  hit@5 dip), back to 5 under credit.
- **LB-03** stays missed in every arm: the cards covering the walker did not
  describe the exclusion behavior in matching vocabulary — card coverage is
  only as good as the distiller's read of the area.

Reading, with the n=25 / single-corpus / single-run caveat stated plainly
(every delta above is one or two queries):

1. **Displacement is real but tiny** — two queries slipped one rank each.
   The colonization fear is not borne out at this card density (16 cards /
   1,636 chunks), even with cards interleaved in the main ranking with no
   authority cap.
2. **The gain shows up exactly where predicted** and only via
   anchor-following: raw ranking alone gains nothing (cards can only match;
   the key path still has to be reached through them). The value of the
   layer is contingent on the anchor hop being cheap and visible.
3. Both observations together are the empirical case for the **lane
   design**: keep cards out of the main ranking (removes LB-07/LB-13-style
   displacement entirely), present them beside it with their anchors
   (keeps the LB-01-style win). LB-13's extra drop under card-credit is an
   artifact of flat expansion that a lane presentation would not have.

What would make the evidence decision-grade rather than suggestive: repeat
on terrarium-bench and lexomancy (different languages, bigger corpus),
a stronger distiller model for coverage (LB-03), and a judged-pool pass to
price the noise cards add below rank-10. None of that is worth doing before
deciding whether the lane surface is wanted at all.

## v1 direction — judgment-carved areas (Wrysk, 2026-08-20)

Wrysk's leaning after v0: directory grouping is not durable across
repositories. The perfect-world shape is a strong agent scanning the repo and
highlighting the lanes worth distilling, then fanning out to compile each
card — some strategy where *judgment*, not folder structure, decides what a
card is about.

v1 implements the minimal honest version of that as a two-phase pipeline
(`bench/distill/distill2.py`):

- **Map pass** — one call to the strongest available model over a repo
  digest (file tree + the head of every indexable file). It returns a JSON
  plan: areas with a slug, a title, an *intent* line ("the questions this
  area answers"), and an explicit file list. Areas may cross directories,
  skip boilerplate, and overlap where a file genuinely serves two stories.
  The plan is validated mechanically (paths must exist; hallucinated ones
  are dropped loudly) and cached beside the bench, so it is reviewable and
  the compile pass is resumable against it.
- **Compile pass** — per area, **whole files** (large caps, truncation
  marked) plus the plan's intent line, so the compiler writes toward the
  questions the planner said the area answers rather than guessing from
  content alone. Card format unchanged except frontmatter records the
  strategy and both models.

Growth path, deliberately not v1: compile agents with tools (request more
files, search the index), planner critique/iteration, cross-card links. The
laundering exclusions (tests, fixtures, scratch, research raw) are strategy-
independent and stay.

## Results — round 2, run 20260820-145702 (lane binary, v0 vs v1 cards)

Three arms in one session on the branch build (lane shipped): control,
`lore-bench-d` (v0 folder cards), `lore-bench-d2` (v1 judgment cards, 20
areas, qwen3.8 for both phases — same model as v0, so deltas are the
strategy). Token cost, measured: map 19.7k→5.1k, compile ~142k→~16k.

**The lane guarantee held live: all three arms' page metrics are identical**
(hit@5 0.92, hit@10 0.92, MRR 0.76, nDCG 0.77). Cards in the corpus change
nothing about the ranked page. The comparison is therefore lane *routing*:
did the ≤3-card lane contain a card whose anchors hold a key path?

| routing hits | design | lexical | semantic | symbol | total | rescues |
|---|---|---|---|---|---|---|
| v0 folder cards | 0/6 | 5/5 | 7/10 | 4/4 | 16/25 | 1 (LB-01) |
| v1 judgment cards | **4/6** | 5/5 | 7/10 | 2/4 | 18/25 | 0 |

Read (n=25 caveats apply as ever):

- **v1 fixed design routing outright** (0→4): v0's single merged "design"
  card never surfaced for design queries; concept-carved cards do.
- **v1 lost the LB-01 rescue — and not to coverage.** `daemon-lifecycle.md`
  anchors `ownership.rs` and describes the lock correctly, but three wrong
  cards outrank it in the lane for that query. A 7-file area's prose dilutes
  any single story; v0's accidentally-narrow daemon cards matched tighter.
  Card *retrievability* trades against area breadth — the planner's epics
  (7-9 files) are too big for their own findability.
- v1's symbol-routing drop (4→2) is the same effect in reverse and costless:
  the page scores 1.0 on symbol queries; lane routing there is decorative.
- LB-03 remains unrouted by both strategies (two carvings, same miss) —
  walker-exclusion phrasing looks like a genuinely hard case.
- Per-card quality is visibly better in v1 (whole files + intent line: the
  authority card explains laundering, path ceilings, bare-ID supersession),
  but several v1 cards hit the 800-token completion cap; raise it for the
  compile pass next round.

Next levers, in order of expected value: cap planner areas at ~4 files
(split the epics), open each card with its intent sentence (it embeds), and
re-test whether the LB-01-class rescue returns without losing the design
gains. A stronger distiller remains untested.

## Results — round 3, run 20260820-191125 (lexomancy: the headroom test)

The corpus where retrieval is actually weak, per Wrysk's framing that a null
here should close the question. 40 v1 cards (tree-mode plan: 1,538 indexed
files -> 40 areas / 194 anchors; qwen3.8 both phases; plan 74k->15.7k tok,
compile 520k->40k tok, all local), drained through the **production vLLM
FP8 embedding stack** via the new `run-vllm.ps1` (the model-matrix `run.ps1`
predates the 2026-08-17 vLLM switch), 17,974 chunks in 2.2 min.

Page: hit@10 0.80 (24/30; keys predate corpus drift off the pin, so treat
absolute numbers as internal to this run). The number that matters:

**The lane rescued 3 of the page's 6 misses — all semantic-kind.**

| | page hit | lane routing | rescues |
|---|---|---|---|
| semantic (13) | 7 | 4 | **3** |
| lexical (6) | 6 | 4 | 0 |
| symbol (5) | 5 | 2 | 0 |
| docs (4) | 4 | 3 | 0 |
| multi-file (2) | 2 | 1 | 0 |

All three rescues verified by hand and mechanically sound: the topically
right card ranks 1-2 in the lane and its frontmatter anchors the key file —
`hand-system` -> `HandLayoutManager.cs` for "what makes the tiles you're
holding spread into a curve and lift up" (LX-04), `player-actions` ->
`PlayerActionSO.cs` (LX-08), `overworld-level-gen` -> `HexRoomGenerator.cs`
(LX-12). Exactly the mechanism as designed: intent-phrased queries that raw
chunks miss, matched by card prose, routed through anchors. Three misses
remain unrescued (LX-02/05/10) — coverage, not ranking.

Scoreboard across rounds, rescues per page-miss:

| corpus | page misses | rescued |
|---|---|---|
| lore-bench v0 (r1) | 2 | 1 |
| lore-bench v1 (r2) | 2 | 0 |
| lexomancy v1 (r3) | 6 | **3** |

Reading: on a strong corpus the lane is decoration; on the weak, flagship
corpus it converts half the failures. The value concentrates exactly where
retrieval needs help, which is the only place a second lane was ever going
to pay. Still open before this is a feature and not an experiment: the
freshness/regeneration loop and plan evolution (unchanged from the parking
assessment), and the three unrescued misses set the coverage ceiling for
this carving. The cards are live in the Lexomancy root, so the dogfood
daemon indexes them and the lane can be felt in real use immediately.

## Generator

`bench/distill/distill.py` — standalone, no daemon involvement. Walks the
corpus (respecting the same obvious exclusions the walker enforces), groups
files into areas, prompts a local model through Ollama's OpenAI-compatible
endpoint (`qwen3.8:latest`; the RunPod 27B arm was down on 2026-08-18 and a
local cheap model is the more honest simulation of a background distiller
anyway), and writes cards. Deterministic file grouping; one model call per
card; failures skip the card and are listed at the end, so a partial run is
usable and resumable.
