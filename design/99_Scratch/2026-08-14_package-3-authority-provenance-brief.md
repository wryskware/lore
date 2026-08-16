---
design_status: exploration
last_reviewed: 2026-08-14
---

# Package 3 brief — Authority & provenance (design-first, interactive Fable thread)

Handoff brief for a fresh session. **Deliverable is a design proposal, not
code.** Every item below encodes policy that is Wrysk's to set; the thread's
job is to turn the review findings into a small set of concrete, decidable
options, walk Wrysk through them, and only then (in a follow-up or the same
thread after approval) implement. Canon rules in `lore/CLAUDE.md` are strict
here: do not add ledger entries, mark documents decided, or amend decided
docs without Wrysk's explicit authorization per choice.

## Scope — four findings from `2026-08-14_adversarial-review-session-1.md`

1. **Authority laundering (S1#2).** Any file declaring
   `design_status: decided` gets top ranking authority; any unclassified file
   citing a D-number gets the leaning multiplier; path is never consulted, so
   `99_Scratch` scratch material outranks its station. Direction: split
   *declared* status from *effective* authority computed at index time —
   validate that `decided` cites an active ledger entry, demote `99_Scratch`
   by path regardless of citations, surface invalid declarations instead of
   silently promoting. Policy questions for Wrysk: what exactly does
   `decided` require to be honored; what is the full path-based floor/ceiling
   map (99_Scratch, 7_Research, deprecated); how are violations surfaced
   (status? search result flag? log only?).
2. **Provenance model before M3 hardens the schema (S1#5).** Sessions
   (D-0008) and later issues need: stable source key, source kind
   (`repo`/`session`/`issue`), source timestamp, declared-vs-effective
   authority — none representable today (files require a repo project FK).
   Propose the engine-neutral fields and store-seam filters now; the session
   writer itself stays M3.
3. **Registry inside the rebuildable engine DB (S1#7).** Deleting a corrupt
   `lore.db` loses the only list of project roots. Propose a small daemon
   manifest (roots + stable project keys) outside engine state; engines
   rebuild from it. Interacts with S1#3's project-identity fix (stable
   opaque keys on the wire) — fold that in.
4. **Decided doc contradicts the wire (S1#8).** `4_Interfaces/4.1` (decided)
   promises `path_glob`; v1 implements literal `path_prefix` (now char-count
   and Windows-case correct, but still a prefix). Wrysk must either authorize
   amending 4.1 or order a real glob implementation. Present the cost of
   each; do not silently rename semantics.

## Also queued for Wrysk in this thread (small, related)

- Which deferred items become GitHub issues: discovery version negotiation
  (S1#4), engine-neutral store trait (S1#6), plus residuals — embed worker
  parked through a query-side health demotion (flagged in the embed-fix
  commit), loopback-without-auth before M3's write endpoint (S1 smell), BOM
  handling in code/text chunkers, ATX trailing-`#` trimming mangling
  `# Learning C#`.

## Constraints

- Proposal document lives in the vault (suggest `1_Architecture` or
  `99_Scratch` until accepted; its own `design_status` stays exploration
  until Wrysk says otherwise).
- If implementation follows approval: worktree, 235-test suite green,
  clippy/fmt clean, commit trailer
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Ranking package
  (package 2) also touches `daemon/search.rs` authority weighting — check
  whether it has merged and sequence accordingly.
