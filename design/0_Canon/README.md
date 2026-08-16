---
design_status: decided
last_reviewed: 2026-08-15
decision_refs:
  - D-0001
  - D-0012
  - D-0013
---

# Lore — Canon, Authority, and Certainty

Design is tentative by default. It becomes canon only through an explicit, user-authorized entry in [[DECISIONS]]. This vault adopts the conventions proven in the Lexomancy design vault.

## Authority order

1. Wrysk's current instruction.
2. Active accepted entries in [[DECISIONS]].
3. Documents named as canonical sources by those entries.
4. `decided` implementation briefs, only within their declared scope.
5. `leaning` / `exploration` / unclassified documents — non-binding context.
6. `deprecated` and `9_Scratch` material — historical or inspirational only.

## Document states (`design_status` frontmatter)

- `exploration` — divergent thinking; contradictions welcome.
- `leaning` — preferred direction with material doubts still open.
- `decided` — consolidated account of accepted decisions; must cite at least one active decision ID.
- `deprecated` — retained for lineage or rejected alternatives.
- *(absent)* — unclassified workspace material; treat as exploration.

## Local certainty callouts

> [!accepted] D-NNNN — backed by an active decision entry.
> [!working] — current model so work can proceed; revisable.
> [!candidate] — one live option, not a requirement.
> [!open] — unresolved; do not introduce dependency on either answer.
> [!rejected] D-NNNN — considered and declined; reason retained to prevent revival.
> [!invariant] — (sparingly) accepted decision whose preservation is itself core.

## Promotion rules

- Only Wrysk (or an explicitly delegated agent, for a named choice) may append accepted entries, mark a document `decided`, or supersede canon.
- [[DECISIONS]] is append-only. Supersession is a new entry with a `Supersedes` field, never a rewrite.
- Decisions may also be authored one-per-file under `0_Canon/decisions/D-NNNN-<slug>.md` with the same field grammar; the filename ID is authoritative (D-0013). Accepted records are substantively immutable in either format.
- Agents may draft proposed entries but must leave them unpromoted.
- Research findings ([[../7_Research/00_summary|7_Research]]) are evidence, not decisions.

## Tool interpretation is opt-in

These conventions bind agents working in this vault regardless of tooling. Lore-the-daemon, however, only *interprets* them for repositories that commit a `.lore.toml` authority profile (D-0012); this repo declares `profile = "lore-v1"`, `behavior = "rank"`.
