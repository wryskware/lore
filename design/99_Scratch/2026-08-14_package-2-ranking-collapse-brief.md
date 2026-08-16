---
design_status: exploration
last_reviewed: 2026-08-14
---

# Package 2 brief — Ranking & collapse (interactive Fable thread)

Handoff brief for a fresh session. Read this, then the cited review findings,
then the code. This is one of two "interactive" packages: diagnose, write a
short design note for Wrysk, get approval, implement in a worktree, and have
Wrysk review the local diff before merging to main. No GitHub PRs.

## Scope — three findings, one coherent redesign of result selection

Full failure scenarios: `2026-08-14_adversarial-review-session-3.md` findings
1 and 4; `2026-08-14_adversarial-review-session-4.md` finding 4 (the test
audit, including the required tests). Cited line numbers reference commit
`3e791d2`; main has since merged three fix waves, so re-locate, but the
ranking logic is untouched.

1. **Window collapse folds distinct content (S3#1).** `collapse_anchor` in
   `crates/lore/src/daemon/search.rs` keys post-fusion dedup on
   `(project, path, anchor-with-#w-stripped)` for *every* candidate. Two C#
   overloads (`Parse(string)` / `Parse(Stream)` — same `Parser.Parse`
   symbol path, distinct chunk IDs) and two sibling `## Notes` sections fold
   into one result. Direction: collapse only candidates *positively* known to
   be members of the same generated `#w` window family; equal-anchor
   non-window chunks are distinct results. Related smell worth folding in:
   the `#w` namespace is inferred from user-authorable strings (a Markdown
   title literally named `#w0` is treated as bookkeeping) — making window
   membership explicit chunk metadata rather than string inference fixes both
   at the root, but changes `Chunk`/store shape; weigh cost, propose either.
2. **Fixed candidate depth omits provably-best results (S3#4).** Both arms
   fetch 50 candidates (`LEXICAL_CANDIDATES`/`VECTOR_CANDIDATES`); a chunk at
   rank 51 in *both* arms outscores a rank-1 singleton under RRF yet is never
   fetched. Separately, collapse can empty a page with no refill: 20
   requested, 1 returned. Direction: at minimum refill after collapse and
   treat per-arm depth as a documented approximation; adaptively fetching
   until unseen candidates cannot cross the cutoff (including authority
   weight) is the principled fix — cost it out.
3. **Tests certify the defect (S4#4).** Ranking unit tests call `fuse` with
   materialized vectors, never `execute`;
   `section_windows_collapse_but_distinct_headings_do_not` asserts the buggy
   fold as expected behavior. Required tests are listed in S4 finding 4 and
   Top-10 items 4 and 6: rank-51 cross-arm agreement through `execute`,
   overload/repeated-heading preservation, collapse-then-refill.

## Decision checkpoint for Wrysk

The one real policy call: candidate-acquisition strategy (fixed depth +
refill, vs adaptive fetch-until-provably-done) — present both with expected
cost on an 80k-chunk corpus (vector arm is a brute-force scan; deeper fetches
are not free). Explicit-window-metadata vs string-inference is a secondary
call if it touches the chunk schema (CHUNK_FORMAT_VERSION bump → full
re-chunk; one was just spent going 2→3, so a schema-touching fix should say
so plainly).

## Constraints

- Vault rules in `lore/CLAUDE.md` apply; D-0003 (C#/Unity flagship) is the
  binding constraint behind the overload case.
- `cargo test --workspace --all-targets` green throughout (235 at handoff);
  clippy and fmt clean; work in a git worktree; small logical commits;
  commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Files: `daemon/search.rs`, `chunk/common.rs` (discriminator machinery),
  possibly `types.rs`/store if window membership becomes explicit. The
  watcher package may be in flight on `watch.rs`/`http.rs` — no overlap, but
  rebase/merge from main before finishing.
