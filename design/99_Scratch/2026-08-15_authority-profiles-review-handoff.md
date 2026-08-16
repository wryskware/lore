---
design_status: exploration
last_reviewed: 2026-08-15
decision_refs:
  - D-0001
  - D-0004
  - D-0006
  - D-0010
---

# Handoff — independent review of Lore's decision-authority system

## Status and purpose

This is an independent review and product-design handoff. It records findings,
recommendations, and candidate implementation steps; it does **not** change
canon, accept a profile design, or authorize implementation. The existing
Lexomancy-derived workflow is called **`lore-v1`** here at Wrysk's explicit
request. That name is deliberately product-facing rather than project-specific.

`lore-v1` should be presented as a versioned, revisable profile, not as Lore's
final theory of decision governance. Its documentation should openly invite
critique, measurement, and compatible or breaking improvements in later profile
versions.

## Executive conclusion

Keep the authority system as an important optional Lore capability, but do not
make every registered repository participate by default and do not require
users to opt out.

The core idea is strong: distinguish polished or plausible prose from
human-ratified project authority, preserve uncertainty during synthesis, and
make that distinction visible to agents at retrieval time. This is especially
valuable in repositories containing extensive exploratory design work or large
amounts of agent-authored documentation.

The current realization is nevertheless too specific to become universal
product law. It hardcodes one vault layout, one identifier grammar, one set of
document states, and one ranking policy. As a workflow for Lore and Lexomancy it
is credible and useful. As a general governance framework it is simultaneously:

- **overcomplicated as a universal user contract**, because ordinary retrieval
  users should not have to learn a documentation constitution; and
- **undercooked as a policy platform**, because activation, namespaces, scope,
  authorization, canonical-source semantics, and query-specific ranking remain
  insufficiently generalized.

Recommended product shape:

1. Authority governance is repository-opt-in through an explicit profile.
2. **`adr`** is the conventional profile, preferably based closely on MADR
   rather than a Lore-invented approximation.
3. **`lore-v1`** preserves the current D-NNNN workflow for dogfooding and
   compatibility while remaining explicitly open to improvement.
4. Parsing/annotation, validation, and search reranking are separate choices.
5. Repositories without an authority profile receive neutral Markdown and code
   retrieval with no hidden directory or frontmatter semantics.

## What was reviewed

The reviewed system is not only the D-NNNN ledger. It is the combined authority
model described by D-0001 and implemented across the Markdown chunker,
effective-authority policy, index refresh, store, ranking, CLI, and MCP output:

- documents are tentative by default;
- `design_status` declares `exploration`, `leaning`, `decided`, or `deprecated`;
- local callouts preserve accepted, working, candidate, open, rejected, and
  invariant claims;
- a user-authorized append-oriented ledger defines accepted decisions;
- supersession retires earlier decisions without erasing history;
- a `decided` declaration is honored only when it cites an active ledger entry;
- path ceilings demote research and scratch material;
- declared and effective authority are exposed separately; and
- effective authority modifies retrieval ranking.

The implementation is unusually defensive for its age. In particular, the
declared/effective split closes the original self-certification problem, body
citations no longer promote arbitrary prose, invalid `decided` declarations
remain searchable but visible as violations, and proposed/rejected entries
cannot retire active canon.

## Evidence and confidence

### Evidence that the workflow is useful

- It directly addresses authority laundering: an agent can no longer safely
  infer that polished, detailed, or repeatedly cited prose is binding.
- It records rejected and superseded reasoning, reducing blind revival of old
  ideas.
- It gives authors room to write exploratory material without first resolving
  every contradiction.
- Lexomancy dogfooding found a consequential supersession bug: harvesting any
  D-NNNN mention from qualified prose would have retired 6 of 16 decisions.
  D-0010 now gives full and partial supersession different behavior.
- Authority failures are surfaced rather than silently converted into ranking
  behavior.

### Evidence that is still missing

The repository contains an authority-sensitive E2E plan, but no completed
comparison demonstrating that the workflow causes agents to:

- answer more authority-sensitive questions correctly;
- cite fewer obsolete or exploratory claims as requirements;
- make fewer implementation changes contrary to current decisions;
- spend fewer tokens or tool calls resolving contradictions; or
- impose an acceptable authoring and maintenance burden.

Consequently, **"promising and qualitatively field-tested"** is supportable;
**"proven generally effective"** is not yet supportable. Future public claims
should preserve that distinction.

## Strongest parts worth preserving

### 1. Tentative-by-default content

Promotion should be affirmative. Ordinary prose, agent output, research, and
implementation notes should not acquire authority merely by looking finished.

### 2. Human promotion gate

Agents may prepare proposals and diffs, but durable binding authority should
require an explicit maintainer act. Lore should help enforce and explain that
boundary without pretending it can infer human authorization from prose.

### 3. Modality preservation

Accepted, working, candidate, open, rejected, and invariant statements mean
different things. Retrieval and synthesis should retain those differences
rather than flatten them into factual sentences.

### 4. History without silent deletion

Rejected, deprecated, amended, and superseded decisions remain valuable
context. The system is right to retain them while preventing them from being
treated as current authority.

### 5. Declared versus effective authority

Lore should continue to store what a document claims separately from what the
configured policy validates. A refused claim should remain visible, searchable,
and diagnosable.

## Main weaknesses and improvement opportunities

### 1. The current convention is globally hardcoded

`0_Canon/DECISIONS.md`, `7_Research`, `99_Scratch`, the four status values, and
the D-NNNN grammar currently have product-wide meaning. There is no authority
configuration surface. This is surprising for repositories that never adopted
the workflow and creates accidental coupling to otherwise ordinary path or
frontmatter names.

**Proposal:** no authority semantics without an explicit repository profile.

### 2. Any one active reference validates an entire file

A `decided` document receives top authority when at least one frontmatter
reference names an active decision. Lore does not verify that the decision's
scope covers the document, that every binding claim is supported, or that the
reference is the relevant one. One valid reference among stale or invented
references is sufficient.

**Proposal:** do not treat a citation as authority inheritance by default.
Decision records themselves are authoritative. Derived normative documents
need an explicit relation from the decision to a path or section, or a profile
rule that defines how that authority is granted.

### 3. Bindingness, document maturity, and retrieval preference are coupled

`design_status` simultaneously describes a document lifecycle and supplies a
ranking tier. But a stable implementation guide can be non-binding, an accepted
decision can be narrowly scoped, and current code can be more relevant than a
binding design decision for an implementation question.

**Proposal:** model at least these concepts separately:

- **decision state** — proposed, accepted, rejected, deprecated, superseded;
- **document maturity** — draft/working, stable, deprecated (if a profile wants
  document-level lifecycle at all); and
- **bindingness** — none, advisory, or binding within declared scope.

Retrieval preference should be a query/profile behavior, not an unavoidable
consequence of storing metadata.

### 4. `Canonical sources` currently mixes unlike relationships

Ledger entries use the field for research evidence, consolidated design prose,
and even implementation code. Canon's authority order appears to elevate these
sources, while the current implementation does not resolve the links and caps
all `7_Research` material at neutral.

**Proposal:** replace the ambiguous relation with explicit fields or links:

- `evidence` — supports the rationale but is not normative;
- `normative_sources` — elaborates the accepted contract within named scope;
- `implements` — code or artifacts realizing the decision; and
- `related` — useful context with no authority inheritance.

### 5. Scope is descriptive rather than operational

Ledger entries contain `Scope`, but it is free prose ignored by effective
authority. Broad records also bundle several choices, producing clause-level
supersession problems.

**Proposal:** encourage one coherent significant decision per record. Use
optional structured scopes such as repository paths, subsystems, or stable
topic keys where operational checking is worthwhile; retain prose scope for
human explanation.

### 6. Supersession semantics are encoded in prose shape

D-0010's bare-ID-list rule safely handles the existing corpus, but it is a
compatibility repair rather than an ideal general schema. Whether an entry
fully retires another should not depend on detecting qualifiers in natural
language.

**Proposal:** profiles should distinguish structured relations:

- `supersedes` fully retires the named record;
- `amends` or `refines` leaves the named record active; and
- prose explains the exact change but is not parsed for graph semantics.

`lore-v1` may retain the D-0010 parser for compatibility while a future
`lore-v2` changes the source format.

### 7. Multiple ledgers share an unsafe project-wide ID namespace

Lore currently unions active IDs from every recognized ledger under a
registered project. If two embedded vaults both define `D-0001`, one ledger's
entry can validate documents from the other authority domain.

**Proposal:** either permit one profile/authority root per registered project,
resolve references against the nearest containing authority root, or qualify
IDs with a profile-defined namespace. Never validate bare IDs against an
unscoped project-wide union when multiple ledgers exist.

### 8. Authorization is a cooperative claim, not authenticated fact

The parser can verify that a file says `Status: Accepted`; it cannot prove who
accepted it. `Decided by` is also prose. This is reasonable for a local
cooperative repository but should be described honestly.

**Proposal:** expose such state as **declared accepted** unless the profile has
an external acceptance mechanism. Optional stronger mechanisms include an
explicit human-run CLI action, commit/PR review policy, or signed provenance.
The first release need not implement these, but its terminology should not
overpromise.

### 9. "Append-only" needs a precise boundary

The workflow permits proposed entries and later acceptance, which ordinarily
requires editing `Status`. Corrections may also be necessary. Literal
append-only language is therefore ambiguous.

**Proposal:** define records as freely editable while proposed and
substantively immutable after acceptance. Later material changes become new
records linked by `amends` or `supersedes`; clerical corrections are dated and
reviewable.

## Proposed profile model

### Repository-level activation

Authority behavior should activate only through repository-owned configuration,
for example `.lore/authority.toml`:

```toml
[authority]
profile = "lore-v1" # or "adr"
behavior = "annotate" # off | annotate | rank
```

The exact file and key names remain open. The required product behavior is:

- no profile declaration means no authority interpretation;
- profile declaration is committed with the repository and follows it across
  machines and contributors;
- `lore status` names the active profile and policy version; and
- unknown profile versions fail visibly rather than silently degrading into a
  different authority model.

### Behavior modes

- **`off`** — ignore authority semantics while retaining ordinary Markdown
  indexing.
- **`annotate`** — parse, validate, filter, and display authority metadata, but
  do not modify general search ranking.
- **`rank`** — apply the profile's authority-aware ranking because the
  repository explicitly requested it.

`annotate` is the safer initial default after a profile is enabled. A profile
may recommend `rank`, but activation and reranking should not be the same hidden
act.

### Query-level intent

Search should eventually allow an explicit override such as:

```text
authority = ignore | prefer | require
```

Examples:

- "What does the implementation currently do?" normally wants `ignore` or a
  code-focused search.
- "What is the binding design?" wants `prefer` or `require`.
- General discovery can use the repository default.

Avoid opaque query-text inference in the first version. An explicit parameter
is inspectable, testable, and agent-visible.

## Proposed `adr` profile — conventional MADR

The preferred direction is to base `adr` closely on **Markdown Any Decision
Records (MADR)** rather than creating a superficially ADR-like Lore format.
MADR is documented at <https://adr.github.io/madr/> and provides established
templates for status, context/problem, considered options, decision outcome,
consequences, and related records.

The profile should document exactly which MADR version/subset it recognizes.
It should be liberal about normal Markdown layout and conservative about
claiming machine-validated authority.

Candidate behavior:

- recognize a configured decision-record directory, with a conventional
  default such as `docs/decisions/` or `docs/adr/`;
- treat each file as one decision record;
- recognize MADR status and explicit supersession relationships;
- return decision identity, status, and relationships as result metadata;
- support filtering to active accepted records;
- keep rejected and superseded records searchable but demoted when authority
  preference is requested; and
- generate or maintain an index for humans without making one monolithic file
  the source of truth.

Open compatibility questions to resolve against actual MADR documents:

1. Which MADR release and templates are the baseline?
2. Are identifiers derived from filenames, frontmatter, or both?
3. Which status spellings and supersession-link forms are recognized?
4. Does `accepted` mean binding, or merely an accepted decision record whose
   scope still needs interpretation?
5. How are malformed or partially recognized records surfaced?
6. Is authority reranking part of the ADR profile or an optional behavior on
   top of annotation?

The implementation should use a representative MADR fixture corpus rather than
only Lore-authored examples.

## Proposed `lore-v1` profile

`lore-v1` is the compatibility and dogfood profile for the current system. Its
initial definition should match implemented behavior closely enough that
Lexomancy and this repository do not change silently:

- decision IDs: `D-NNNN`;
- ledger discovery: `**/0_Canon/DECISIONS.md`;
- accepted and active set parsed from ledger headings and fields;
- D-0010 bare-list full supersession semantics;
- declared document states: exploration, leaning, decided, deprecated;
- `decided` honored only with at least one active frontmatter reference;
- `7_Research`, `99_Scratch`, ledger pinning, and session ceilings as currently
  implemented;
- declared/effective authority and violation notes exposed on results; and
- current authority weights when behavior is `rank`.

The profile documentation should also list known limitations rather than
quietly blessing them:

- file-level authority is coarse;
- any active reference is sufficient;
- canonical-source links are not resolved;
- multiple-ledger ID collisions are possible;
- accepted status is not authenticated;
- exact ranking weights lack completed E2E validation; and
- the supersession grammar exists for compatibility with real qualified prose.

Naming the profile `lore-v1` must not convert these limitations into permanent
requirements. Versioning creates permission to learn. Improvements that can be
made compatibly belong in the profile's policy revision; source-format or
meaning changes should become `lore-v2` with an explicit migration path.

## Alternative process recommended for future Lore profiles

The strongest long-term candidate combines conventional ADR modularity with
Lore's useful promotion and modality ideas:

1. One short file per significant decision.
2. Ordinary changes proceed through normal code/doc review; decision ceremony
   is reserved for choices whose rationale future maintainers need.
3. Proposed records can be edited freely.
4. Accepted records are substantively immutable.
5. Later records use structured `amends`, `supersedes`, and relationship links.
6. A generated index provides chronology and current-status overview.
7. Normal documentation does not become binding merely by citing a decision.
8. Normative delegation from a decision to another document is explicit and
   scoped.
9. Lore validates syntax and referential integrity, but does not pretend to
   prove semantic correctness or human authorization.

This direction aligns with Michael Nygard's original short, modular ADR model
(<https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions>)
and MADR while retaining Lore's distinctive authority-aware retrieval.

## Proposed implementation sequence

### Phase 0 — validate the product premise

- Run the planned authority-sensitive E2E tasks with authority ignored,
  annotated, and preferred.
- Record answer correctness, authority/citation mistakes, tool calls, tokens,
  and qualitative confusion.
- Test authoring burden with at least one repository that did not originate the
  workflow.
- Do not claim general efficacy before these results exist.

### Phase 1 — put existing behavior behind `lore-v1`

- Add repository-owned authority profile configuration.
- Make unconfigured repositories neutral.
- Move hardcoded policy behind a profile interface without changing
  `lore-v1` semantics.
- Surface active profile name, version, behavior, ledger count, and validation
  health through `status`.
- Add a compatibility test proving existing Lore/Lexomancy authority results
  remain stable when `lore-v1` is selected.

### Phase 2 — separate annotation from ranking

- Add `off`/`annotate`/`rank` behavior.
- Add query-level `ignore`/`prefer`/`require` semantics or equivalent names.
- Benchmark the current 1.15/1.05/1.0/0.7 weights and consider near-tie
  secondary ordering rather than unconditional multiplication.
- Ensure code and ordinary Markdown are not globally penalized merely because
  a decision framework exists.

### Phase 3 — implement the MADR-based `adr` profile

- Select and document the MADR version/subset.
- Build fixtures from conventional MADR examples.
- Parse one-record-per-file identity, status, and explicit relations.
- Expose annotations and filters first; make reranking separately selectable.
- Provide `lore authority init --profile adr` only after the generated files
  genuinely conform to MADR rather than a Lore-specific fork.

### Phase 4 — validation and profile tooling

Candidate `lore authority check` validations:

- duplicate or ambiguous IDs;
- broken decision references and relationship links;
- supersession cycles;
- missing normative-source paths;
- unsupported status spellings;
- `decided` declarations refused by `lore-v1`;
- multiple authority roots with an unsafe shared namespace;
- malformed profile configuration; and
- profile-version incompatibility.

Validation should distinguish errors, warnings, and informational limitations.
It should not fail a repository merely for choosing no authority profile.

### Phase 5 — evaluate `lore-v2`

Use dogfood and external-user findings to decide whether to evolve the source
workflow toward:

- one decision per file;
- structured `supersedes` versus `amends`;
- explicit evidence/normative/implementation relations;
- scoped authority delegation;
- authority-root namespaces; and
- a clearer separation of decision status, document maturity, and bindingness.

Do not design `lore-v2` solely from the current ledger's edge cases. Compare it
against the MADR profile and real repositories first.

## Acceptance criteria for a public authority feature

Before presenting authority profiles as a stable public capability:

1. An unconfigured repository experiences no authority-driven ranking or path
   semantics.
2. Profile activation is explicit, repository-owned, versioned, and visible.
3. `adr` consumes ordinary MADR documents without requiring Lore-specific
   rewrites.
4. `lore-v1` preserves current dogfood behavior and documents its limitations.
5. Annotation and reranking can be selected independently.
6. Multiple authority roots cannot accidentally validate each other's IDs.
7. Malformed or unsupported claims degrade visibly and conservatively.
8. E2E evidence demonstrates when authority preference helps and when it
   harms.
9. Documentation states that profile policies are open to measured evolution.
10. No profile claims authenticated human approval unless it actually verifies
    one.

## Decision checkpoints requiring Wrysk's explicit authorization

The following are proposals, not accepted decisions:

1. Authority profiles are opt-in at repository level.
2. No-profile repositories receive neutral retrieval.
3. Profiles separate annotation from reranking.
4. The conventional profile is named `adr` and based on a pinned MADR
   version/subset.
5. The existing compatibility profile is named `lore-v1`.
6. `lore-v1` initially preserves current semantics, limitations included.
7. A later `lore-v2` may change the workflow after benchmarks and external
   feedback.
8. Query-level authority intent is added to search.
9. Profile configuration is repository-owned and committed.
10. Implementation sequencing follows the phases above.

Wrysk explicitly chose the **`lore-v1`** name for this handoff and expressed a
leaning that **`adr` should use something conventional like MADR**. Per the
promotion rules, those instructions shape this proposal but do not by
themselves append a new accepted ledger entry or amend existing canon.

## Suggested next handoff

Before implementation, prepare a compact decision package resolving only the
minimum product boundary:

- activation/config file location;
- no-profile behavior;
- `lore-v1` compatibility promise;
- annotation versus ranking default;
- MADR version/subset selection; and
- treatment of multiple authority roots.

Keep detailed `lore-v2` schema design out of that decision. It should be earned
by E2E results and experience with the conventional `adr` profile.
