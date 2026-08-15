---
design_status: exploration
last_reviewed: 2026-08-14
---

# Design note — Package 2: ranking & collapse (for Wrysk approval)

Response to `2026-08-14_package-2-ranking-collapse-brief.md` (S3#1, S3#4,
S4#4). Diagnosis confirmed against current `main`; line references below are
current. Nothing here is implemented yet — the two checkpoint calls are at the
end.

## Diagnosis (confirmed)

1. **Collapse folds distinct content** (`daemon/search.rs:214-240`,
   `collapse_anchor` at `:249`). The post-fusion dedup key is
   `(project, path, anchor-with-#w-stripped)` for *every* candidate, not just
   window chunks. Two C# overloads (`Parse(string)`/`Parse(Stream)`) share
   symbol path `Parser.Parse` with different bodies → distinct IDs, no `#d`
   suffix (the `#d` pass in `chunk/common.rs:251` fires only on identical
   anchor *and* text) → equal collapse keys → one suppresses the other. Same
   for two sibling `## Notes` sections.
2. **A second instance of the same bug lives one tier down, and pure string
   gating cannot fix it.** If *both* overloads are oversized (>4 KB), each is
   windowed independently and both families emit `Parser.Parse#w0`,
   `Parser.Parse#w1`, …. The strings carry no family identity, so "collapse
   only chunks that have a `#w` suffix" still folds the two families into one
   result. Family membership is only knowable at chunk time.
3. **Fixed candidate depth** (`LEXICAL_CANDIDATES`/`VECTOR_CANDIDATES` = 50,
   used at `search.rs:127,134`). A chunk at rank 51 in both arms scores
   2/111 ≈ 0.01802 under RRF, beating a rank-1 singleton (1/61 ≈ 0.01639),
   yet is never fetched. Separately, collapse after fusion can underfill the
   page with no refill (ask 20, get 1).
4. **Tests certify both defects** — all ranking tests call `fuse` with
   materialized lists; `section_windows_collapse_but_distinct_headings_do_not`
   (`search.rs:522`) asserts the buggy whole-section fold as expected
   behavior.

## Fix 1 — window collapse

**Option A — string-gated collapse (search.rs only).** Compute a collapse key
only for candidates whose anchor contains a `#w<n>` discriminator; every
non-window candidate is a distinct result. Fixes the two reported scenarios.
Residual holes: (a) windowed overload families still fold (point 2 above);
(b) a user-authored Markdown heading literally named `#w0` is still
misclassified as bookkeeping. Zero schema cost.

**Option B — explicit window family (recommended).** Make membership chunk
metadata instead of string inference:

- Add an optional field to `ChunkKind::Code`/`ChunkKind::Section`, e.g.
  `window: Option<WindowFamily { family: u32, index: u32 }>` where `family`
  is a per-file ordinal assigned by the `Emitter` each time it windows a
  span (`chunk/common.rs:199-219` is the only producer).
- Collapse gates on `window.is_some()` and keys on
  `(project, path, family)`. String inference (`strip_discriminators` /
  `is_discriminator` in ranking) goes away entirely; a heading named `#w0`
  becomes ordinary content.
- **Cost is smaller than the brief feared.** `kind` is persisted as serde
  JSON in a TEXT column (`store/mod.rs:404,738`) — a `#[serde(default)]`
  field needs **no SQLite migration**; old rows deserialize as `None`. The
  `#w{i}` anchor suffix is kept, so **chunk IDs do not change**, upsert keys
  on `(project_id, chunk_id)` preserve rowids, and **embeddings survive
  untouched** (no re-embed).
- **CHUNK_FORMAT_VERSION 3→4 (the sub-call).** Without a bump, existing
  windowed chunks have `window = None` and stop collapsing — duplicates
  *appear* in results (annoying, not hiding content — errs in the safe
  direction) until each file is next edited. With a bump, one CPU-only full
  re-chunk converges everything; since IDs are stable, no re-embedding
  follows. Recommend bumping: the cost is one indexing pass, not an
  embedding spend like 2→3.

## Fix 2 — candidate acquisition

Store facts that set the costs (80k-chunk corpus): the vector arm is a
brute-force O(n) scan whose cost is independent of requested depth (bounded
top-k heap, ~few ms; `store/mod.rs:536-542`); deeper `k` only hydrates more
rows. The lexical arm is FTS5 with bm25 ordering — SQLite scores all matching
postings regardless of LIMIT, so depth is nearly free there too. **Depth per
call is cheap; extra rounds cost roughly one more vector scan each (~ms).**

**Option 1 — fixed depth + refill-on-underfill.** Loop in `execute`: fetch at
depth D, fuse+collapse; if the page is short and some arm returned exactly D
(possibly more available), double D and rerun. Fixes underfill (scenario B).
**Does not fix scenario A** — a rank-51 cross-arm winner is still silently
absent whenever the page fills; per-arm depth stays a documented
approximation. S4's required test #6 ("the mathematically winning shared hit
is returned") cannot pass under this option.

**Option 2 — adaptive fetch-until-provably-done (recommended).** Same loop,
but the stop condition is a proof: stop when the page is full **and** no
candidate outside it can possibly reach the page's minimum score, i.e. for
every outside candidate c,
`authority_max(c) × (rrf_seen(c) + missing_arms(c) / (RRF_K + D + 1))` is
below the current cutoff (fully-unseen candidates are the special case
`rrf_seen = 0`, `authority = 1.15`); or every arm is exhausted (returned
< D). Fuse/collapse recompute from scratch each round, so partially-seen
candidates whose other-arm rank was beyond D are handled by the next round.
A hard cap (`MAX_CANDIDATES = 1000`, logged when hit) keeps depth bounded
and is the one remaining documented approximation. Underfill refill is
subsumed by the same loop.

> **Post-implementation correction (2026-08-14, same day).** This paragraph
> originally estimated "one extra round to D≈200 usually decides it — total
> ~2 vector scans per query." That modeled only the fully-unseen bound and
> was wrong. The binding term is the partially-seen candidate just below the
> cutoff: proving it out needs the per-arm ceiling `1/(K+D+1)` to fall under
> the gap between adjacent ranks, which near rank 20 is ~`1/(K+20)²` — a
> real proof needs D≈6400 (measured during implementation: a dense singleton
> corpus at limit 20 first proves final at depth 6400). Consequence: on
> hybrid queries whose arms are both still open at 1000, the cap binds
> routinely and the loop runs the full ladder (50→200→800→1000, ~4 vector
> scans, each O(n) and few-ms at 80k). What the loop still genuinely
> guarantees: cross-arm agreement anywhere in the first 1000 wins its page
> (the reported rank-51 case), and collapse never underfills while eligible
> hits exist within 1000. Lexical-only queries still settle in one round
> (the closed vector arm zeroes the unseen bound for lexical-seen rows).
> Open option for Wrysk at diff review: since the cap binds anyway on rich
> hybrid corpora, a single fixed fetch at 1000 per arm would deliver the
> same guarantee with less code and fewer scans; the ladder only pays off
> when an early round proves finality (wide gaps, small limits) or arms
> exhaust early (small filtered scopes — common in practice). Direction
> unchanged either way.

## Fix 3 — tests (per S4#4 and Top-10 #4/#6)

- Rewrite `section_windows_collapse_but_distinct_headings_do_not`: the
  unsuffixed whole section is now a distinct result; explicit siblings of one
  generated family still collapse.
- New `fuse`-level: two same-anchor C# overloads and two repeated-heading
  sections, distinct IDs/spans — all survive; two *windowed* families with
  equal anchors collapse per-family, not together (Option B only).
- New through `search::execute` against a real in-memory store: rank-51
  cross-arm agreement wins (Option 2 only); window-heavy corpus where
  collapse forces refill and the page still fills while eligible hits exist.
- Test authoring is a separate pass from implementation per global rules.

## Decision checkpoint

1. **Candidate acquisition:** Option 2 (adaptive, recommended) or Option 1
   (fixed + refill, leaves S3#4-A standing and drops required test #6)?
2. **Window membership:** Option B with version bump (recommended), Option B
   without bump (converges lazily, temporary duplicate windows in results),
   or Option A (string-gated, leaves windowed-overload fold + `#w0` heading
   smell)?
