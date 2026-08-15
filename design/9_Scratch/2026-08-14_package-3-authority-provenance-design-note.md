---
design_status: exploration
last_reviewed: 2026-08-14
---

# Design note — Package 3: authority & provenance (for Wrysk approval)

Response to `2026-08-14_package-3-authority-provenance-brief.md` (S1#2, S1#5,
S1#7+S1#3, S1#8). Diagnosis re-confirmed against current `main`; line
references are current. Nothing here is implemented; decision checkpoints are
at the end. Amending any decided document (Part 4) additionally requires
explicit authorization per canon rules.

## Diagnosis (confirmed)

1. **Authority is self-certified and path-blind** (S1#2). Every Markdown file
   gets vault metadata unconditionally (`chunk/markdown.rs:34`), and
   `authority_weight` (`daemon/search.rs:273`) trusts it: `decided` in
   frontmatter ⇒ 1.15× with no ledger check; an unclassified chunk whose text
   mentions any `D-NNNN` ⇒ 1.05× (`search.rs:275`,
   `chunk/common.rs:233`). Path is never consulted, so `9_Scratch` material —
   which Canon README §Authority-order and 2.1's status-ranking callout place
   with `deprecated` at the bottom — ranks by whatever it declares or quotes.
   The review corpus itself is the exploit: these session notes quote
   D-numbers densely and currently outrank their station.
2. **No provenance model** (S1#5). `projects` is (id, root, name); `files`
   and `chunks` require a repo-project FK (`store/schema.rs:40,47`). A
   session document (D-0008) has nowhere to carry source kind, source
   timestamp, or a capped authority; `recall` cannot filter by corpus without
   path conventions leaking through the store API.
3. **The registry lives only inside the rebuildable DB** (S1#7). Deleting a
   corrupt `lore.db` loses the list of roots; "derived, rebuildable index"
   (D-0006) is only true if its inputs survive elsewhere. Related (S1#3):
   the wire round-trips project *display names*, which are not unique
   (`schema.rs`: only `root` is UNIQUE), so `expand(project, chunk_id)` can
   resolve the wrong project or 404.
4. **Decided 4.1 promises `path_glob`; the wire implements `path_prefix`**
   (S1#8). `4_Interfaces/4.1_MCP_Surface.md:15` says "path glob";
   `lore-core/src/lib.rs:143` and `store/query.rs:90` implement a literal
   (char-count and Windows-case correct) prefix.

## Part 1 — Declared vs effective authority (S1#2)

### Mechanism (common to all policy options)

- `design_status` and today's `authority_tier` remain **declared** — parsed,
  stored, visible, filterable, never edited by Lore.
- New **`effective_tier`** per chunk, computed at index time by a pure policy
  function over (declared status, project-relative path, frontmatter refs,
  the project's active-ledger set). Ranking multiplier and the
  `min_authority` filter read *effective*; `status` filters keep reading
  declared.
- **Ledger recognition** by path convention: `**/0_Canon/DECISIONS.md`.
  Indexer parses `## D-NNNN` headings, `Status:`, `Supersedes:` → the
  project's active-decision set, persisted in store meta. The ledger file
  itself is pinned to the top tier (it *is* the canon; it should never lose
  to a doc quoting it).
- **Recompute is cheap**: effective tier derives entirely from persisted
  columns + vault JSON — a store-side pass, no re-chunk, no re-embed, chunk
  IDs untouched. Triggered when a ledger file changes or the policy version
  bumps; migration V2 adds the column and startup backfills by recompute.
- **Wire (additive)**: `SearchResult.design_status` stays declared; add
  `effective_authority` (label) and `authority_note` (present only when
  demoted, e.g. `"decided declared but cites no active decision"`,
  `"9_Scratch path cap"`). MCP schema text updated: `decided` means
  *validated* decided.

### 1a — What must `decided` cite to be honored?

- **Option A — syntactic:** frontmatter `decision_refs` contains ≥1
  `D-NNNN`, existence unchecked. Per-file, no cross-file state. A typo'd or
  invented D-number still promotes.
- **Option B — ledger-validated (recommended):** ≥1 cited ref resolves to an
  **active** (accepted, not superseded) entry in the same project's ledger.
  Matches Canon README verbatim ("must cite at least one active decision
  ID"). Incremental cost over A is small because the mechanism parses the
  ledger anyway (pinning + recompute trigger).
- Failed validation demotes to tier 1 (neutral) and sets `authority_note`;
  it does not drop to deprecated — an invalid declaration is a mistake to
  surface, not content to bury.

### 1b — Path floor/ceiling map (proposed)

Matched on any path segment, hardcoded in v1 (config knob only if a second
vault needs different names — Lexomancy shares the convention):

| Path / condition | Rule | Basis |
|---|---|---|
| `0_Canon/DECISIONS.md` | pinned tier 3 | it is the canon |
| any `9_Scratch` segment | **ceiling 0** (with deprecated) | README order #6; 2.1 ranking callout |
| any `7_Research` segment | **ceiling 1** (evidence, not decisions) | README promotion rules |
| declared `deprecated` | tier 0 | existing behavior |

Open sub-question on `7_Research`: D-0002 names research summaries as
canonical sources, so a case exists for ceiling 2 (leaning) instead of 1.
Recommendation stays 1 — "canonical source of a decision's rationale" is what
the ledger link is for (see 1c-B), not a general ranking boost.

### 1c — Fate of the unclassified-cites-D bonus

- **Option A — remove (recommended):** citations stay visible metadata
  (`decision_refs` on results) but carry no weight. This is the direct
  laundering vector; scratch and session notes quote D-numbers constantly.
- **Option B — replace with canonical-source elevation:** only files that
  active ledger entries *name* (via `Canonical sources` links) get a floor
  of tier 2. This is the truer reading of "ledger-cited" in 2.1/3.1 —
  authority flows *from* the ledger *to* named documents, not from any doc
  quoting a number. Cost: resolve the ledger's wiki-links to paths at index
  time. Composable with A (A removes the bad bonus; B adds the legitimate
  one).
- **Option C — keep, restricted** to validated refs outside demoted paths.
  Still launderable by any ordinary repo doc; not recommended.
- Note: 3.1 (leaning) says "`decided`/ledger-cited" rank together; under A
  alone that phrase should be edited to match (3.1 is not decided, so this
  is an ordinary leaning-doc edit, flagged here for transparency).
- **Residual, documented:** `leaning` declarations remain self-certified
  (gentle 1.05×). Path ceilings cover scratch; elsewhere this is accepted
  workflow, not laundering worth machinery.

### 1d — Surfacing violations

- **Option A (recommended):** all three surfaces — `authority_note` on
  affected search results; `status` reports a per-project violation count
  with the CLI listing offending files (same pattern as watch/embedding
  degradation: visible, never silent); tracing log line at index time.
- **Option B:** log only. Cheapest; contradicts the project's own
  "degradation must be visible" ethos (D-0007 reasoning).

## Part 2 — Provenance fields before M3 hardens the schema (S1#5)

Fields and seams now; the session writer/watcher stays M3.

- `projects` becomes the **source registry**: add
  `kind TEXT NOT NULL DEFAULT 'repo'` (`repo` | `session` | later `issue`)
  and the stable `key` from Part 3. At M3 a session corpus is a source row
  with `kind='session'` rooted in the data dir, registered internally —
  HTTP `POST /v1/projects` keeps refusing anything but repo roots.
- `files` gains `source_ts INTEGER NULL` — the source-declared timestamp
  (session write time). NULL for repo files in v1; the recency term (3.1
  ranking #3) reads it at M3.
- Store seam: `SearchFilter` gains `source_kinds: Option<Vec<SourceKind>>`;
  `recall` becomes "search filtered to sessions" with no path convention
  leaking. Wire: additive optional `SearchRequest.sources` filter now, while
  there is one client generation (absent = all kinds).
- Ranking contract: session sources get an **effective-tier ceiling below
  vault material** via the Part 1 policy function (the cap 3.1 requires);
  exact cap value and recency weighting are M3 tuning, but the fields make
  them expressible without another migration.
- Declared-vs-effective authority is Part 1; stable source key is Part 3.
  Net v1 behavior change: none (everything defaults to `repo`).

## Part 3 — Registry manifest outside engine state + stable keys (S1#7 + S1#3)

- **`<data-dir>/projects.toml` manifest**, written atomically (temp+rename,
  as the handshake does) on every register/remove:
  `[[project]] key / name / root / kind`. The manifest is authoritative;
  the DB `projects` table is derived. Startup reconciles manifest → DB
  (insert missing rows, drop rows the manifest no longer lists), then the
  normal rescan rebuilds everything else. Deleting `lore.db` now loses only
  derived state.
- **Stable opaque key** assigned at registration, never recomputed or
  changed by rename: slug of the display name, with a short random suffix
  on collision (`lore`, `shared-a3f2`). Engine row IDs (`i64`) stay
  internal/non-authoritative.
- **Wire**: `SearchResult` gains `project_key`; `ExpandRequest` gains
  optional `project_key` taking precedence over `project`. Display-name
  resolution stays for humans and the CLI, and registration now **enforces
  unique names** (reject with a "pass --name" hint) so name resolution is
  deterministic — this closes both halves of S1#3's cheap-now.

## Part 4 — 4.1 `path_glob` vs implemented `path_prefix` (S1#8)

- **Option A — amend 4.1 to say prefix** (requires explicit authorization to
  edit a decided doc). Zero code. Cost: agents cannot express
  `Assets/**/Tests/*.cs` — a real pattern for the Unity flagship — until a
  later `path_glob` lands (backlog issue). "Everything under this directory"
  — the dominant case — already works.
- **Option B — implement real `path_glob`**: `globset` crate; store layer
  extracts the glob's literal prefix for SQL pushdown, then glob-matches in
  Rust before ranking in both arms, honoring the same Windows
  ASCII-case-folding policy as `daemon::paths`. New wire field alongside
  `path_prefix`. A bounded medium package (dep + both-arm plumbing +
  case-policy tests), sequenced after package 2.
- No recommendation strong enough to preempt: A is honest-and-cheap now, B
  keeps the promise the decided doc already made. Either way the semantics
  are never silently renamed.

## Sequencing with package 2

`pkg2-ranking-collapse` (unmerged) rewrites `fuse`/collapse in
`daemon/search.rs` and extends `types.rs`. Package 3's implementation touches
`authority_weight` (an input to the same scoring path), `types.rs`, and the
schema. Land package 2 first; implement package 3 against the updated `main`.
The designs are orthogonal — effective tier replaces the *input* to the
authority multiplier; collapse/acquisition changes neither read nor write it.

## Issue candidates (Wrysk to pick)

- S1#4 — discovery version negotiation (scalar `api_version`, hard-coded
  `/v1` base URL defeat coexistence).
- S1#6 — engine-neutral store trait (M4 seam; interface-level only).
- Embed worker parked through a query-side health demotion (residual flagged
  in the embed-fix commit).
- Loopback-without-auth before M3 adds a write endpoint (`session_log`
  changes the threat model).
- BOM handling in code/text chunkers (markdown handles it; others don't).
- ATX trailing-`#` trimming mangles headings like `# Learning C#`.
- `path_glob` implementation (only if Part 4 Option A is chosen).

## Decision checkpoint — resolved 2026-08-14

Wrysk answered 1a–1d directly in-thread and delegated the rest ("do whatever
makes sense"); remaining picks follow the note's recommendations.

1. **1a** `decided` validation: **Option B, ledger-validated** (Wrysk).
2. **1b** path map: **approved as proposed; `7_Research` ceiling = neutral**
   (Wrysk).
3. **1c** citation bonus: **Option A, removed** — citations stay metadata,
   no weight; no canonical-source elevation for now (Wrysk).
4. **1d** violations: **Option A — results + status + log** (Wrysk).
5. **Part 2 + Part 3**: **approved as specced** (delegated).
6. **Part 4**: **amend 4.1 to prefix** (Wrysk selected in-thread; treated as
   the required authorization), `path_glob` filed as a backlog issue.
7. Issues: **all seven candidates filed** (delegated; closing an unwanted
   issue is cheap, losing a deferred item is not).

If implementation follows approval: worktree, 235-test suite green,
clippy/fmt clean, `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
