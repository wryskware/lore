---
design_status: exploration
last_reviewed: 2026-08-14
---

# Deferred backlog

Items deliberately deferred out of the M1 review fix waves. Tracked here
instead of GitHub issues (solo repo). Source: session-1 review + package
briefs; details in the cited docs.

- **Discovery version negotiation (S1#4).** Scalar `api_version` +
  hard-coded `/v1` in `Handshake::base_url` defeat /v1+/v2 coexistence.
  Advertise supported versions/endpoints; make the route data-driven.
- **Engine-neutral store trait (S1#6).** `StoreHandle` holds concrete
  `Store`; `StoreError` leaks rusqlite types. Define the trait + neutral
  error around what daemon/search/embed actually use; policy above the seam.
  Interface-level only — no second engine until M4 earns it.
- ~~Embed worker parked via query-side health demotion~~ — fixed 2026-08-17
  (`0215a81`). The entry was accurate and the hazard was still live: issue
  #5 had wired `Health::request_probe` to the query *timeout* branch, which
  does not demote, while the branch that does demote (a hard failure, e.g. a
  400) had no wake path at all. A worker parked in its select watches the
  indexer pulse, the probe request, the idle tick and cancellation — a health
  write wakes none of them — so the endpoint stayed reported unreachable and
  every search stayed lexical-only for up to the 60s fallback tick. Both
  query outcomes now converge on the same wake-up. Residual: `Embedder::
  refresh()` demotes without a probe request on the same pattern; it has no
  production callers today, so it was left alone.
- **Loopback-without-auth, before M3 `session_log`.** A write endpoint
  changes the threat model; decide auth posture before it ships.
- ~~BOM handling in code/text chunkers~~ — fixed 2026-08-17 (`da8bbbc`).
  Stripped once in `common::trim_span`, the single function every chunker's
  spans pass through, rather than per chunker. Measured aside: tree-sitter
  symbol spans already started after the mark, so the real leak was the
  window/text path — unknown extensions, `.txt`, and code that fails to parse
  and degrades to windows.
- ~~ATX trailing-`#` trimming~~ — fixed 2026-08-17 (`ae4ed6c`). `scan_headings`
  now implements the CommonMark §4.2 rule (a trailing `#` run closes a heading
  only when preceded by a space/tab, or when it is the whole content), so
  `# Learning C#` keeps its `#` while `## Wrap-up ###` still trims.
- **`path_glob` search filter — deferred until a need arises (low
  priority).** 4.1 was amended (S1#8) to document the implemented literal
  prefix; a real glob (`Assets/**/Tests/*.cs`, globset, SQL prefix pushdown,
  Windows case-folding parity in both arms) was wanted for Unity workflows.
  Wrysk's call, 2026-08-16: don't build it now. The original cross-project
  noise motivation is largely gone under mandatory project scoping (D-0016),
  agents doing natural-language retrieval rarely reach for a path filter, and
  issues #22/#2 name a more useful filtering axis — partitioning by document
  type (design/docs vs code vs issues) as DB-level tags. Revisit if a real
  workflow demands paths specifically.

## Dogfood findings (2026-08-15, first daemon session)

- **Transient "authority declaration not honored" warnings on first scan.**
  During a full scan that (re)indexes files, per-file authority is evaluated
  before the ledger row lands, so validly-cited `decided` docs get a WARN
  that the same pass's recompute immediately reverses. End state is correct;
  the log misleads. Defer or suppress the warning until after ledger parse.
- **Zombie daemon.** The 2026-08-14 daemon (pre-fix-wave binary) was found
  alive but wedged: process running ~24 h, handshake stale, HTTP unresponsive.
  Cause unknown; possibly an already-fixed hang. Watch for recurrence on the
  current binary before investigating.
- ~~Ledger parser retired partially-superseded decisions~~ — fixed same day
  (bare-ID-list rule, Wrysk's call; see D-0010 and `authority.rs`).
- **The walker does not follow junctions/symlinks.** A workspace assembled
  from directory junctions (Lexomancy-bench) indexed only its 2 regular
  files; all three junctioned trees were skipped silently. Real-world
  layouts do this. Needs `follow_links` plus loop protection, and a think
  about what the watcher can honestly promise across junction targets on
  Windows (ReadDirectoryChangesW does not see through them).
  *Update 2026-08-17:* the **silently** half is fixed — a walk now reports
  the links it declined, a pass counts them (`links_skipped`) and warns with
  the paths. Traversal itself is still open and is now argued in
  [[../1_Architecture/2026-08-17_link-traversal-decision-brief]], which
  proposes `!`-re-include opt-in plus periodic rescan (rather than secondary
  watches) for coverage. Awaiting a decision.
- **No `lore remove`.** A mistakenly registered project cannot be
  unregistered from the CLI; the row lingers in status output
  (Lexomancy-bench, 2 files, is the standing example).

## Residuals from the package-3 merge (2026-08-15)

- **Unreadable-ledger degradation is untested.** `refresh_authority` keeps
  the stored active set when a ledger read fails (so a transient IO error
  cannot mass-demote the vault), but no test exercises it — making a file
  reliably unreadable on Windows needs platform locking machinery.
- **Pre-V2 duplicate display names are left in place.** Uniqueness is
  enforced for new registrations only; `resolve_project("shared")` returns
  the first match while project keys reach both. Acceptable migration
  behavior; a `lore rename` affordance would let a user clean up.
- **`registry::bootstrap` key backfill is per-row, not atomic.** Safe as
  written (only allocates unheld keys); worth folding into
  `apply_project_set` if bootstrap ever grows.
- **Authority multiplier dominance is only proven at fixture scale.** The
  ordering (0.7 scratch cap below neutral, etc.) is tested at the ranks the
  fixtures produce, not against arbitrary RRF gaps on large corpora.

## Product questions parked (2026-08-17)

- **Should an LLM be coupled to lore, and on which path?** Prompted by
  Augment Code's context engine, so the premise was checked before parking it.
  What their docs actually say: the Context Engine returns *ranked chunks* —
  "results with file paths, line numbers, and snippets" — with no
  answer-generation step; the prose answers come from the agent calling it
  (Auggie CLI, or whatever agent the MCP is plugged into), not from the engine.
  Their one shipped LLM coupling is on the **write** path: Context Lineage
  detects new commits, has an LLM summarize each diff, and embeds the summary
  alongside code chunks so "why was this renamed" retrieves the commit itself.
  So lore is not missing a capability their engine has — lore's caller is
  already an LLM, and that is the same place Augment's synthesis happens.
  Three distinct proposals hide inside "couple an LLM", worth deciding
  separately:
  1. **Index-time enrichment** — LLM summaries of commits, files or modules,
     embedded as retrievable chunks. Closest to what Augment demonstrably
     ships; costs indexing time and GPU, no query-path latency.
  2. **Query-time rewriting / reranking** — HyDE-style query expansion or a
     cross-encoder/LLM rerank over the fused candidate list. Still returns
     chunks; measurable directly by [[../6_Evaluation/2026-08-17_relevance-bench-proposal]].
  3. **Query-time synthesis** — lore returns prose. The only real payoff is
     saving the *caller's* context window, since the caller can already
     synthesize. Weigh against the extra dependency and GPU contention with
     embeddings.
  Canon is silent: D-0003 constrains embedding *providers* to local-only and
  says nothing about generation; D-0004 scopes v0.1 as "a grep/CCE
  replacement, not a memory system". Nothing here is decided.
  Sources: `docs.augmentcode.com/context-services/context-connectors/how-it-works`,
  `augmentcode.com/blog/announcing-context-lineage`,
  `docs.augmentcode.com/context-services/mcp/overview`.

## Bench integrity (2026-08-17)

- **The `lexical` control arm was empty in every recorded run.** All four
  2026-08-15/16 lexical runs recorded zero results for all 80 queries, so the
  0.00 floor in the bench summaries was a broken arm, not a BM25 measurement.
  It reproduces correctly on the current binary (lore-bench hit@10 0.92,
  terrarium 0.80, lexomancy 0.63), so whatever caused it is fixed or was
  transient — the cause was never diagnosed and no artifact from those runs
  records an error. D-0012 does not rest on it (its evidence is the C#-semantic
  gap *between embedders*), but no stated margin over the floor was ever real.
  On lore-bench today BM25 alone matches or beats every recorded embedding arm.
- **Lexomancy has drifted off the shape the recorded runs measured.** 17.9k
  chunks on 2026-08-17 against the ~81k the throughput notes recorded; the
  D-0020 ignore stack landed in between. Any comparison across those dates is
  indicative only, and re-pinning the corpus is a prerequisite for the next
  controlled matrix run.
