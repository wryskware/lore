# Lore steering — final drafts (Lever A + Lever B)

Status: **drafted, nothing applied.** Wrysk is holding off on another bench
round; these are ready to apply whenever. Updated after the round-1 grading
pass, which changed the framing: unsteered adoption cost **zero
correctness** (qwen 12.5/15 both arms; no cell anywhere scored worse with
lore on), so steering's job is not rescuing quality — it is (a) capturing
the token/wall win that only materialized where the model actually leaned
on retrieval (Lexomancy: luna −55% tokens at equal-or-better scores) and
(b) avoiding the aimless-exploration tax where lore was carried but unused
(qwen terrarium/lore T3/T5 cells). That also means steering can ship on its
own merits and be validated by dogfood; a round-2 bench validates it but is
not a prerequisite.

## Lever B — lore-mcp `search` tool description (recommended first; ships to every consumer)

`crates/lore-mcp/src/server.rs`, `search` tool. Replacement description —
the first two sentences are new, the authority text is unchanged from
current:

```text
Your first tool for any "where is X / how does Y work / what was decided
about Z" question in an indexed project: one query replaces a chain of
exploratory greps and directory reads, and also surfaces design-doc and
decision context that grep cannot see. Prefer it whenever you do not
already know the exact file; fall back to grep only for exhaustive
literal sweeps (every occurrence of an exact string) — and note that
inconsistently-named concepts defeat literal grep but not this search.
Hybrid lexical+semantic search over the code projects and design vaults
indexed on this machine. Each hit carries provenance (file, line span,
symbol path for code, heading path for Markdown), the status the
document declares, and the authority Lore actually assigns it. Those
differ on purpose: `decided` is honored only when the document cites a
decision still active in the project's ledger, and scratch/research
paths are capped whatever they declare - a demoted hit says why in its
authority note. Prefer sources whose *effective* authority is `decided`
when documents disagree; cited decision IDs are provenance, not
authority. Excerpts are truncated; call `expand` with a hit's
project_key and chunk_id to read it.
```

The "inconsistently-named concepts" cue is earned: the round-1 Lexomancy
T3 trap (code says Lexonic, ledger says Lexic) is exactly the case where a
literal grep silently loses half the results.

`expand`/`status` descriptions unchanged. Application checklist:
- update `mcp_golden__tools_list.snap` (`cargo insta` or snapshot review)
- keep `tool_descriptions_steer_the_agent_rather_than_restating_the_name`
  green
- separate commit, no other changes mixed in

## Lever A — repo AGENTS.md nudge (hold unless B proves insufficient)

Identical text for each bench repo (and usable verbatim in any real repo
lore indexes). Final wording:

```markdown
## Code and design search

This repo is indexed by the lore context daemon. For any "where is X",
"how does Y work", or "what was decided about Z" question, call
`lore_search` FIRST — it searches code and design docs together and is
faster and more complete than grepping, especially when you don't know
the exact file or the concept's naming is inconsistent. After a hit, use
`lore_expand` to read it in full before quoting or editing. Use
grep/glob only for exhaustive literal sweeps of an exact string.
```

Bench-application caveats (recorded from round 1, matter only if a round 2
actually runs):
- opencode injects AGENTS.md into context on BOTH arms; the off arm just
  has no lore tools to act on it. Note it when grading.
- Lexomancy: the file must live at the bench junction root and stay out of
  the cm workspace so T5's undo pass never captures it; add to the run-day
  checklist rather than the tree.

## If/when a round 2 runs

Only the 15 on-arm cells need re-running (~20 min qwen serial, ~10 min luna
4-wide) for an off / on-unsteered / on-steered three-way. Apply B alone
first; A only if B doesn't move adoption. Key fixes to fold in first, from
the graders' key_gaps notes (see bench/results/grades.md 2026-08-16
section): lore-T3's prompt asks only half its key bullet; lore-T5 should
state whether CHUNK_FORMAT_VERSION must bump; lexomancy-T5 should pin the
shield-percentage normalization (answers chose Block/MaxHP); the two T4
keys should say whether code citations satisfy the prose-source
requirement (both arms took symmetric 0.5s on the strict reading).
