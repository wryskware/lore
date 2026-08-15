---
design_status: exploration
last_reviewed: 2026-08-14
---

# Adversarial Review Briefs — M1 (2026-08-14)

Four staged review sessions for an external frontier model (GPT/Codex)
with local repo access. Run them in order. Later sessions should read prior
reports, but each session brief still carries enough context to stand alone.
Paste the **shared preamble** plus **one session brief** per session.

The briefs are threat models and attention guides, not contracts or exhaustive
checklists. Reviewers should use their own judgment about what constitutes an
adequate review, rebalance effort when the code points somewhere more important,
and include useful "could be nicer" observations separately from formal defects.

Scratch/process doc — not design canon.

---

## Progress

- **Session 1 complete:**
  `design/9_Scratch/2026-08-14_adversarial-review-session-1.md`
- **Session 2 complete:**
  `design/9_Scratch/2026-08-14_adversarial-review-session-2.md`
- **Session 3 complete:**
  `design/9_Scratch/2026-08-14_adversarial-review-session-3.md`
- **Next:** Session 4 — test-suite quality audit.
- Before starting a later session, read all existing
  `design/9_Scratch/2026-08-14_adversarial-review-session-*.md` reports. Do not
  repeat an identical finding; extend it with a distinct failure mode, stronger
  evidence, a correction, or a session-specific consequence.

### Handoff notes for Session 4

- Session 2 confirmed eight findings: shutdown can withdraw ownership while
  uncancellable work survives; deletion/corruption of `daemon.json` permits a
  live-incumbent takeover; overlapping project roots update only one project;
  lowest-project-ID scheduling can starve later projects; the watcher event
  channel is unbounded ahead of `IndexQueue`; failed watch arming is neither
  retried nor surfaced; backward clock steps can extend the known restart delay
  arbitrarily; and an older query-embed failure can overwrite a newer Ready
  health observation for up to the 60-second worker tick. Use the report's
  exact scenarios and interleavings for de-duplication.
- Session 3 confirmed eight findings: window collapse also suppresses distinct
  C# overloads and repeated Markdown headings; 5,000 poisoned low-rowid chunks
  hide every later embedding candidate and falsely end the drain; `expand` can
  return unrelated current lines after a file shifts; the fixed 50-candidate
  pools can omit the mathematically best RRF result and underfill after window
  collapse; a UTF-8 BOM disables vault frontmatter parsing; path-prefix SQL uses
  UTF-8 byte length where SQLite counts characters and remains case-sensitive
  on Windows; Markdown parent introductions below 24 bytes are dropped; and the
  worker/store vector-validity mismatch can wedge a batch while health remains
  Ready. Use the report's exact inputs and consequences for de-duplication.
- Session 3's poison-window lead was confirmed, not merely suspected. The
  smallest decisive boundary test is more than 5,000 missing rows with the
  oldest 5,000 in the poison set; `drain` must not report `Idle` while an
  unpoisoned later row exists. Do not substitute a small poison test — the
  existing one-row test passes and misses the `MAX_FETCH` cap interaction.
- The highest-value Session 3 mutations/tests for Session 4 are: remove the
  `#w` predicate from collapse (it is effectively absent today) and challenge
  overload/repeated-heading preservation; move a file's indexed text down while
  keeping the old line number valid before `expand`; put one shared candidate
  at rank 51 in both arms; prefix a CRLF vault fixture with a UTF-8 BOM; filter
  `données/parser.cs` by `données/` and a Windows path by alternate casing;
  place a sub-24-byte rule before a child heading; and return a finite vector
  whose norm is nonzero but at most `f32::EPSILON`.
- Do not turn Session 2's long store-lock observations into missing correctness
  tests without a distinct invariant. Session 3 found no transaction/FTS
  partial-commit failure: the existing insert/update/delete/file-removal FTS5
  integrity test is meaningful coverage. Likewise, direct spot checks of three
  real Lexomancy C# files found exact spans and only expected `#w` overlap; test
  quality should distinguish these covered properties from the untested
  collapse semantics downstream.
- Session 4 should treat all Session 1–3 findings as missing-test evidence. In
  particular, the existing future-heartbeat test deliberately pins the
  clock-rollback outage rather than challenging it, the existing poison test
  proves only one rejected chunk does not block a tiny backlog, and the existing
  `expand` tests cover stable files plus deletion but not a shifted live file.
  The independently counted suite total remained **217 green tests** after
  Session 3; recount because later reports or fixes may change it.

---

## Shared preamble (include in every session)

You are performing an **adversarial review** of Lore, a local context daemon
for AI coding agents, written in Rust. Your job is to **falsify claims and
find defects**, not to summarize or praise. Assume the code was written by
competent authors under time pressure and that the interesting bugs are in
the seams between components, in Windows-specific behavior, and in the
failure paths nobody exercises daily.

**Repo:** `C:\Users\perag\wryskware\lore`. The M1 implementation baseline is
commit `3e791d2`; its implementation range is `46bedff..3e791d2`, ~14k lines
of Rust across three crates. Do not assume `3e791d2` is still `HEAD`: later
commits may add briefs, reports, or fixes. Start with `git status`, recent log,
and `git diff 3e791d2..HEAD`. Review the current implementation when source has
moved, and state the actual target in the report. Preserve unrelated and
uncommitted work.

**Workspace layout:**
- `crates/lore-core` — wire contract: HTTP API types + daemon discovery
  (`daemon.json` handshake record, data-dir resolution). Thin clients depend
  only on this.
- `crates/lore` — everything else: `types` (chunk data model), `chunk`
  (tree-sitter + Markdown chunking), `store` (SQLite: metadata + FTS5 +
  vectors, one transaction domain), `embed` (OpenAI-compatible client,
  worker, health), `daemon` (axum HTTP, single-instance handshake, watcher,
  indexer, hybrid search), `config`, `cli`, `main`.
- `crates/lore-mcp` — thin MCP stdio proxy (rmcp 3.1) exposing
  `search`/`expand`/`status`; proxies to the daemon over loopback HTTP.

**Toolchain baseline:** Rust edition 2024, MSRV 1.88. CI = `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, tests, on Windows
MSVC only (deliberate). The M1 baseline had 216 green tests; recount and report
the current total rather than treating that number as permanent.

**Binding constraints (treat violations as findings, cite the ID):** the
design vault (`design/`) has an authority model — only `design/0_Canon/DECISIONS.md`
entries are binding; everything else is context whose `design_status`
frontmatter states its modality. The load-bearing decisions:
- **D-0003** — Windows-native (no WSL). C#/Unity is the flagship target.
  Embeddings are **local-only** (OpenAI-compatible endpoint at a local
  server; TLS deliberately absent from reqwest). **Exactly one authoritative
  owner of index state** — a second writer is disqualifying by construction.
- **D-0004** — v0.1 is a retrieval-first slice; `design_status`/D-NNNN
  awareness is first-class in the indexer.
- **D-0005** — **no graph subsystem, no call extraction, ever.**
- **D-0007** — one versioned loopback HTTP API (axum); thin clients
  (CLI, `lore-mcp`) never touch index state; absent/unhealthy embedding
  endpoint ⇒ **lexical-only degradation, visibly surfaced, never an error**.
- Design detail (leaning, but implemented): `design/3_Retrieval/3.1_Chunking_and_Ranking.md`
  — content-addressed chunk IDs; ranking = BM25 + cosine fused with RRF,
  then vault-authority modifier ordered decided > leaning >
  exploration/unclassified > deprecated.

**Ground rules:**
1. Every finding: `file:line`, a **concrete failure scenario** (inputs/state
   → wrong behavior), severity (critical / major / minor), and confidence.
   If you cannot construct a failure scenario, it is not a finding — it may
   be listed separately as a smell, debt, hardening idea, or "could be nicer"
   observation. Those observations may be explained when the reasoning is
   useful; they are not restricted to one line.
2. **Verify against the code, not against comments or this brief.** Module
   docs make strong claims; your job includes checking whether the code
   actually delivers them.
3. **Use judgment; do not mechanically exhaust the checklist.** The named
   attack surfaces identify likely seams, not the boundary of the review. Follow
   stronger evidence outside them, skip sterile prompts, and prefer a smaller
   set of well-proven findings over shallow coverage. Report an important defect
   discovered outside the nominal session scope rather than saving it for later.
4. Windows is the primary platform. Path casing, `\\?\` verbatim prefixes,
   CRLF, file locking/sharing, ephemeral-port exhaustion, and
   `ReadDirectoryChangesW` overflow are all in scope.
5. **Known issues — do not re-report:** GitHub issues #1–#9 on
   wryskware/lore. Highlights: multi-word lexical queries fail on FTS5 AND
   semantics (#4); query-embed timeout vs model cold-load health flap (#5);
   HTTP 400 wrapping transient transport errors poisons batches (#6);
   64-hex chunk_id token cost over MCP (#7); 45s restart window after hard
   kill, no `lore stop` (#8); cosmetics/config grab-bag incl. `bin`
   hard-exclude, RRF-vs-authority tuning, fingerprint dims:0 (#9).
   If repository access permits, read the live issues because this summary is
   lossy. Also read prior session reports and apply the same no-duplication rule.
6. Do not spend primary review time on style nits, speculative dependency
   swaps, or "rewrite it my way." The SQLite→Tantivy+arroy implementation is a
   deliberately deferred choice. It is still valid to report a seam that makes
   that deferred choice materially harder, or a grounded feature/hardening idea
   in the separate observations section.
7. Deliverable: write one self-contained Markdown report at
   `design/9_Scratch/2026-08-14_adversarial-review-session-N.md`. Give it
   `design_status: exploration` frontmatter so its D-number citations cannot be
   mistaken for canon. Rank formal findings most-severe first, then include
   smells/debts/improvements, then the session-specific deliverable. A concise
   scope/target note is welcome; do not pad the report with a code summary.
8. Run tests, probes, or small reproductions when they materially improve
   confidence; reading alone is acceptable when the failure follows directly
   from the code. Record meaningful verification and distinguish observed
   behavior from reasoned interleavings.
9. Report only. Do not fix production code, push commits, open issues, or
   silently clean the working tree. Creating the requested report is the only
   expected repository edit.

---

## Session 1 — Architecture and design conformance

**Objective:** stress the shape, not the lines. Does the implementation
actually deliver the architecture canon promises, and where will this shape
hurt at M2 (vault polish), M3 (session ledger `session_log`/`recall`), M4
(Tantivy+arroy swap, daemon-managed llama-server, named-pipe transport)?

Read first: `design/0_Canon/DECISIONS.md`, `design/1_Architecture/1.1_Overview.md`,
`design/3_Retrieval/3.1_Chunking_and_Ranking.md`, `design/4_Interfaces/4.1_MCP_Surface.md`,
`design/5_Implementation/5.1_Milestones.md`. Then the code, breadth-first.

Attack surface:
1. **The SearchStore seam.** Module docs claim nothing SQLite-shaped crosses
   the `store` boundary and a Tantivy+arroy engine could implement the same
   methods. Is that true in practice — or do BM25 score semantics, FTS5
   sanitization behavior, rowid assumptions, or transactional coupling leak
   through the API in ways a different engine could not honor?
2. **Single-owner invariant (D-0003).** Enumerate every path that can open
   the SQLite file or mutate index state. Is the daemon *provably* the only
   writer, or merely the only writer by convention? What does a second
   `Store::open` from CLI tooling actually do?
3. **Wire contract stability.** `lore-core` is v1. Which M3 features
   (session_log/recall) or M4 features will force breaking changes that
   could be cheaply avoided now? Is anything stringly-typed in a way that
   will rot (status labels, project name-or-id resolution)?
4. **Layering violations.** Does `lore-mcp` really depend only on
   `lore-core`? Does anything in `daemon` reach around `store`'s API? Is
   discovery (handshake read) duplicated anywhere it could drift?
5. **The two-tier memory model (D-0006/D-0008)** isn't built yet — but does
   anything in the current schema or daemon shape actively obstruct it
   (e.g. session-ledger chunks needing a recency term and an authority cap
   below vault material — can the current ranking pipeline express that)?
6. **Modality audit.** Find places where the implementation hardened a
   `leaning`/`working` design detail into something expensive to change,
   without a decision entry.

Session deliverable (extra): a ranked list of "pay now vs pay at M3/M4"
architectural debts, each with the cheap-now fix.

---

## Session 2 — Concurrency, lifecycle, and Windows behavior

**Objective:** break the daemon. Races, deadlocks, lost wakeups, shutdown
hangs, double-owners, watcher storms, and Windows-specific failure modes.

**Prior overlap:** Session 1 confirmed that two simultaneous starters can both
pass the check-then-publish handshake admission and become index owners. Do not
restate that exact race as new. Look for distinct takeover/liveness variants,
consequences, or evidence that changes its severity or proposed remedy.

Focus files: `crates/lore/src/daemon/**` (especially `mod.rs`,
`store_handle.rs`, `index.rs`, `queue.rs`, `watch.rs`, `handshake.rs`),
`crates/lore/src/embed/worker.rs`, `crates/lore/src/embed/health.rs`,
`crates/lore/src/main.rs` (runtime setup).

Claims to falsify (each is asserted in module docs or worker reports):
1. "The store mutex guard **cannot** be held across an `.await` — this is a
   compile-time guarantee." Look for `blocking()` called from async context,
   long-held guards inside `spawn_blocking` starving search, and the
   poisoning-recovery path masking real corruption.
2. "A pass takes and releases the lock per file, so a full scan never
   starves `/v1/search`." Measure the actual critical sections — is there
   any per-file work under the lock that is O(file size)? What about
   `chunks_missing_embeddings`' widened fetch under one lock?
3. "The handshake protocol never allows two owners." Attack the takeover
   window: stale heartbeat + slow-but-alive daemon (probe timeout 1s vs a
   daemon blocked >1s), two daemons racing takeover simultaneously, clock
   skew, heartbeat write failing silently mid-run, `daemon.json` deleted by
   hand while a daemon runs.
4. "A save storm cannot OOM or starve queries" — the IndexQueue coalesces
   and collapses to full rescan past 4096 paths. Check the notify/race
   handling, the watcher-event → queue → indexer handoff for lost-wakeup
   windows, and what happens when full_scan is requested *while* a full
   scan for the same project is running.
5. **Shutdown.** Cancellation races an in-flight embed HTTP call, an
   in-flight index pass, and axum graceful shutdown, all under a 10s
   deadline. Find the path where shutdown exceeds the deadline or leaks the
   handshake file wrongly; find work that continues after cancel.
6. **Embed worker ↔ indexer signaling** uses `Notify::notify_one` with the
   stored-permit argument. Verify no lost-wakeup when a pulse lands during
   drain; verify the 60s fallback tick can't double-drain concurrently with
   a notified drain.
7. **Watcher gap:** registration sends Watch before scan but arming is
   async (~300ms observed gap; known). Look for *other* gaps: watcher
   restart after error, overflow/rescan handling, rename pairs where only
   one half arrives, events for paths inside the data dir, case-mismatched
   paths on Windows (`relative_to` is ASCII-case-insensitive — is that
   sufficient for NTFS?).
8. **Health state machine** (`Unconfigured/Unreachable/Ready`) is written
   from three places (worker probe, search-path demotion, startup). Find an
   interleaving that lies to `/v1/status` for longer than one probe cycle,
   or that flaps forever.

Session deliverable (extra): for each confirmed race, a minimal interleaving
trace (thread A / thread B step list).

---

## Session 3 — Data-path correctness (chunking → store → search → wire)

**Objective:** wrong-answer bugs. Silent index corruption, ranking that
doesn't implement its spec, and lossy round-trips.

Focus files: `crates/lore/src/types.rs`, `chunk/**`, `store/**`,
`daemon/search.rs`, `daemon/expand.rs`, `embed/text.rs`, `embed/client.rs`,
and `crates/lore-mcp/src/render.rs`.

Claims to falsify:
1. "Re-chunking identical bytes yields identical chunk IDs, so embeddings
   and FTS rows survive re-index." Attack: anchor collisions (the `#`
   discriminator namespace is shared by convention — `Type#Trait` vs
   `#w0/#d1/#s0`), the dedup pass appending `#d{n}` (is `n` assignment
   order-stable across runs?), path normalization (`\` → `/`,
   case-insensitive filesystems), CRLF vs LF checkouts changing byte spans.
2. "Kept-chunk upsert never rewrites text/path/anchor, so an ID collision
   proves they're identical." Check the actual upsert SQL: what *does* it
   update, and can vault-status/span drift produce a chunk whose stored
   metadata disagrees with its FTS row or embedding?
3. **Transactionality.** `replace_file_chunks` claims all-or-nothing across
   chunks + FTS + embeddings. Verify the trigger set (insert/delete/update)
   covers every mutation path, incl. `remove_file` cascades with
   `recursive_triggers`, and that a mid-transaction error cannot commit a
   partial state. Check `chunks_fts` external-content integrity after every
   write path (the `'integrity-check'` command is only run in tests).
4. **FTS5 sanitization** claims syntax errors are unreachable. Attack with:
   unicode edge cases (ZWJ, RTL, combining marks — `is_alphanumeric` on
   what?), the 64-term cap interacting with prefix `*`, empty-query and
   all-operator inputs, and whether a "no usable terms" query really skips
   MATCH everywhere it's reachable.
5. **Ranking spec conformance** (`daemon/search.rs`): RRF over
   1-based ranks with k=60, authority multiplier *after* fusion,
   unclassified-but-cited ranked as leaning, `#w` collapse keeping the best
   window. Check: candidate-pool truncation before fusion biasing results
   (`limit.max(50)` per arm), tie-breaking stability, float comparison via
   `partial_cmp` with the `unwrap_or(Equal)` fallback, score meaning when
   the same chunk appears in both arms vs one.
6. **Vector path:** store normalizes on write and claims cosine == dot.
   Query vectors: normalized or not, and does it matter for *ranking*
   (it shouldn't) or for any threshold/score surfaced to users (it might)?
   Dimension-mismatch handling mid-re-embed (fingerprint moved under a live
   query) — degraded or wrong-answer?
7. **Chunker fidelity spot-check (C# flagship):** pick 3 nontrivial real
   files from a Unity project shape (partial classes, `#region`, nested
   types, expression-bodied members, generics with constraints) and verify
   spans are exact, headers don't double-index member bodies, and the
   container-header rule ("stops where the first substantial member
   starts") holds. Use an accessible Unity repo when available; otherwise
   construct focused adversarial files and say so. Same, briefly, for
   Markdown: setext headings are known
   unsupported; look for *other* silent losses (HTML blocks, footnotes,
   frontmatter edge cases like `---` inside code fences at file start).
8. **Expand:** disk read of the *current* file vs stored chunk spans — a
   file edited since indexing can shift lines. What does expand return, is
   it labeled honestly, and can it panic on a shrunken file?
9. **Embedding text construction:** discriminator stripping, the 8KB
   truncation (char-safe?), header for `group` chunks, and the
   query/document prefix asymmetry — anything that embeds text A but
   indexes text B in a way that misleads retrieval.

Session deliverable (extra): a table of every silent data-loss path found
(input class → what is lost → is it visible anywhere).

---

## Session 4 — Test-suite quality audit

**Objective:** answer "are these tests good, or slop?" with evidence. The M1
baseline count was 216; use the current count in the report.
The suite was written by the same class of model that wrote the code —
the known failure mode is tests that **confirm the author's understanding**
rather than challenge the implementation.

Scope: every test target (`crates/lore` lib unit tests, `crates/lore/tests/*`,
`crates/lore-mcp` unit + `tests/mcp_golden.rs`, `crates/lore-core` unit).
Read the prior session reports too: each confirmed defect is evidence about a
missing or misleading test, and the audit should identify the precise test that
would have caught it.

Method — for each of the ~10 core invariants below, apply a **mutation
lens**: propose 2–3 specific, subtle code mutations (off-by-one, inverted
condition, dropped branch, swapped argument) and determine *by reading the
tests* whether any existing test would fail. Report each invariant as
COVERED / PARTIALLY / THEATER with the mutation that survives.

1. Chunk-ID stability across re-chunk (and across path spellings).
2. `replace_file_chunks` atomicity + embedding survival for unchanged IDs.
3. FTS row lifecycle (insert/update/delete triggers; no orphans after churn).
4. FTS query sanitization (hostile input never errors, cap enforced).
5. Vector top-k ordering + filter pushdown + dimension mismatch.
6. RRF arithmetic + authority ordering + window collapse.
7. Handshake freshness/takeover matrix (fresh/stale × probe ok/fail/stranger).
8. Indexer change detection (hash short-circuit, prune-by-diff,
   skip-removal — note: the skip-removal *bug fixed on 2026-08-14* was
   missed by the suite; explain what test would have caught it and check
   whether an equivalent gap-class remains).
9. Embed worker: fingerprint reconcile, poison isolation, notify wakeup,
   cancellation.
10. Wire contract: HTTP handlers (status/register/search/expand error
    paths), MCP golden files.

Also assess:
- **Tautologies:** tests that re-derive the expected value using the same
  logic as the implementation (compute-then-assert-computed).
- **Snapshot quality:** do the insta snapshots pin behavior a human
  reviewed, or just freeze whatever the code did first?
- **Timing:** every wait/poll loop — bound, flake risk on slow CI, and any
  test that passes vacuously if a background task never runs.
- **Negative-path coverage:** count assertions on error paths vs happy
  paths per module; name the three worst-covered failure paths in the
  codebase.
- **Test-support honesty:** stub servers (embed stub, MCP stub daemon) —
  do they drift from real behavior in ways that make passing meaningless
  (e.g. the synonym-vector stub's geometry vs a real model)?

Session deliverable (extra): the **top 10 missing tests**, ranked by
risk × cheapness, each specified concretely enough to hand to an
implementer (name, arrange/act/assert sketch, which invariant it guards).

---

## After the sessions

Reports go to the user/orchestrating agent for triage. Each finding is verified
against the code and de-duplicated against the tracker before anything is filed
or fixed. Review sessions do not push commits, open issues, or "fix while
you're there" — report only.
