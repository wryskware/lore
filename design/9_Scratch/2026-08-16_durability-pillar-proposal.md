---
design_status: exploration
last_reviewed: 2026-08-16
decision_refs:
  - D-0001
  - D-0002
---

# Proposal — Durability pillar: value must survive model improvement

Drafted by an agent at Wrysk's request (2026-08-16 session). **Unpromoted.**
Becomes canon only if Wrysk authorizes the ledger entry below.

## Wrysk's articulation (source of intent)

> We should be striving to make something that remains useful even as LLMs
> become smarter and make fewer mistakes. If the only problem we're fixing is
> something that goes away when agents get smarter, we should reconsider.

Accompanying personal position, offered as a leaning, not a requirement:

> [!working] Wrysk's working assumption: LLMs are never going to have
> project-scope memory natively; skills/workflows/harnesses don't change the
> fact that a large amount of corpus must enter the context window to draw
> meaningful conclusions about a repo.

Modality note: the pillar (the *test*) is what Wrysk asked to integrate. The
"never" prediction is a working assumption the pillar deliberately does **not**
depend on — see rationale.

## Proposed ledger entry (draft — do not append without authorization)

## D-0015 — Durability pillar: value must survive model improvement

- **Date:** (on acceptance)
- **Status:** Proposed
- **Scope:** Product direction; admission test for all features
- **Decided by:** (Wrysk, on acceptance)
- **Decision:** Every Lore capability must justify its value against a future
  of substantially smarter, less error-prone agents. A feature whose value
  rests on agents being forgetful, sloppy, or bad at navigation is a
  depreciating asset and requires explicit reconsideration. Durable value must
  be grounded in at least one of:
  (a) **institutional fact** — information no intelligence can infer from the
  corpus, e.g. whether a document is binding (authority, provenance,
  decidedness);
  (b) **context economics** — the cost/latency of moving a growing corpus into
  a finite, priced context window;
  (c) **system state and coordination** — index ownership, freshness, and a
  shared source of truth across concurrent agents and sessions.
- **Rationale:** Tools that compensate for model weakness are obsoleted by
  model improvement; tools that supply what intelligence cannot derive
  appreciate with it. Smarter models extract *more* from a small, correct,
  trusted context — improving models raise the value of precision + authority
  and lower the value of bulk retrieval. The pillar intentionally does not
  depend on any prediction about whether native project-scope memory arrives:
  ground (a) survives even unbounded context, because decidedness is a fact
  about authorization, not a property of text.
- **Consequences:** Feature proposals state which ground (a/b/c) they rest on.
  Features resting only on "agents currently miss things" are framed as
  short-horizon conveniences, not core. Ranks Lore's own layers by durability:
  authority/decision memory > freshness/state ownership > generic code
  retrieval.
- **Supersedes:** None
- **Canonical sources:** this document (on acceptance)
