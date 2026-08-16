# Round-2 steering drafts — lore adoption on the on-arm

Drafted mid-round-1 (qwen matrix running, unsteered). Round 1 observation:
qwen3.8 on-arm lore usage is bimodal — 12/12 tool calls on lore/T1, then 0
on lore/T3, 0 on terrarium/T2, 1/19 on terrarium/T1. It reaches for lore
when the prompt smells like a search task and greps its way through
everything else. Two levers, deliberately separable experiments. Neither is
applied anywhere yet; both are Wrysk-review-then-apply.

## Lever A — repo-level nudge (AGENTS.md in each bench repo)

Cheap, per-repo, no code change, invisible to the off arm (the file only
matters if the agent reads it, but opencode injects AGENTS.md into context,
so it lands on both arms — the off arm just has no lore tools to act on it;
worth noting when grading).

Proposed text, identical in all three bench repos:

```markdown
# Working in this repo

This repo is indexed by the lore context daemon. When you need to find
where something is implemented, how a subsystem works, or what the design
docs decided, call `lore_search` FIRST — it does hybrid lexical+semantic
retrieval over code and design docs and is faster and more complete than
grepping. Use grep/glob only for exhaustive literal sweeps (every call
site of an exact string). After a search hit, use `lore_expand` to read
it before quoting or editing.
```

Placement: `AGENTS.md` at repo root of `lore-bench`, `terrarium-bench`, and
for Lexomancy the bench junction root (verify opencode picks it up through
the junction; Lexomancy-alt is under cm, so the file must be a local-only
add that T5's undo pass never captures — add it to the run-day checklist,
not the tree, or pre-freeze it before pinning).

## Lever B — tool-description rewording (lore-mcp server.rs)

Current `search` description is strong on authority semantics but silent on
*when to use the tool at all* — no contrast with the native grep/read tools
sitting next to it in the model's toolbox. Proposed: prepend a use-cue
sentence, keep the authority text intact.

```text
Your first tool for any "where is / how does / what decided" question
about an indexed project: hybrid lexical+semantic search over the code
and design vaults on this machine. Prefer this over grep or directory
listing when you don't already know the exact file — one query replaces
several exploratory greps and also surfaces design-doc context grep
cannot see. [existing provenance/authority text unchanged] ...
```

`expand` and `status` descriptions already carry their use-cues; no change.

Costs of B: touches `crates/lore-mcp/src/server.rs`, must update the golden
snapshot `mcp_golden__tools_list.snap`, and must keep the
`tool_descriptions_steer_the_agent_rather_than_restating_the_name` test
green. Benefits every consumer, not just the bench — arguably the
principled fix if round 1 shows the description undersells the tool.

## Protocol note

Round 1 ran the on-arm **unsteered** — record that in the round-1 plan doc
before grading so on-arm scores are read as organic-adoption numbers. A
round-2 rerun needs only the 15 on-arm cells (~20 min) to produce the
three-way off / on-unsteered / on-steered comparison. If both levers go in
at once we can't attribute the delta; recommended order is B alone first
(it ships to real users), A only if B is insufficient.
