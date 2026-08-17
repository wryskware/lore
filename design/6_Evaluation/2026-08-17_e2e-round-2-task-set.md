---
design_status: exploration
last_reviewed: 2026-08-17
decision_refs:
  - D-0009
---

# E2E round 2 — task set, prompts and grading keys

Self-contained. This document supersedes, **for round-2 purposes only**,
[[2026-08-15_e2e-round-1-plan]] § "Task archetypes" / § "Per-repo
instantiations", [[2026-08-15_e2e-round-1-answer-key]] § "Prompts", and all of
[[2026-08-17_e2e-round-1-key-addendum]] (Revision A). Those three stay on disk
unchanged as the record of what round 1 was actually graded against. Nothing
here re-grades round 1.

**Never shown to benchmark models.** Prompts are verbatim; everything else is
key material.

## What changed, and why it was allowed to change

Round 2 is **independent of round 1**. Comparability with round-1 scores is
explicitly not a goal (Wrysk, 2026-08-17): the server has moved materially
since, so there is nothing to compare. That revokes the constraint Revision A
was written under. Revision A repeatedly chose "preserve comparability" and
"do not change the prompt" over "make the test correct"; every one of those
choices is re-decided here on the merits.

### The arm-neutrality rule, restated

The old worry was that a key edited toward lore's output would flatter the on
arm. That worry is about **hidden or post-hoc** criteria, and it still stands:

- A criterion the off arm could not have known about is a bad test.
- A criterion **both arms are told about**, which the on arm satisfies more
  easily, is a valid result — that is the product claim under test.

So the operative rule for round 2 is: **every scored criterion must be
derivable from the prompt.** If the key wants a recorded rationale cited, the
prompt asks for it. If the key wants persisted state migrated, the prompt says
the change has to reach files that are already indexed. Nothing is graded that
the prompt did not ask for.

### Prompt style (unchanged)

Short, lazy, hint-free, phrased the way Wrysk actually asks. Hint-free means
not leaking **where the answer lives** or **how to find it**. It does not mean
being vague about **what is being asked for**. State the ask, then stop. No
numbered sub-questions, no "be sure to check X".

## Defects this round fixes

| # | Defect | Resolution |
| --- | --- | --- |
| D1 | **The answer key is inside the corpus.** `lore-bench` at pin `977364a` contains `design/9_Scratch/2026-08-15_e2e-round-1-plan.md`, which spells out the task list *and the graded answers* for all three repos, including lore's T1 hop chain, T2 framing, T4 rationale ("the key-exchange convergence argument — commit 60b3599"), and T5 bug description. | **Scrub the file from both bench trees at setup**, and make the reset unable to resurrect it. See § Corpus scrub. |
| D2 | lore T3's prompt asked half of what the key graded, *and* both arms scored 1 in every round-1 cell — a task that measures nothing. | T3 replaced with the enumeration the key half wanted, on a deliberately grep-hostile vocabulary. Prompt rewritten to ask for exactly that. |
| D3 | lore T5 and `CHUNK_FORMAT_VERSION`: key silent, on arm bumped, off arm did not, Revision A declared it out of scope. | Bumping is correct behaviour and the constant's own doc comment says so. **Required**, and the prompt now asks for it in behavioural terms. |
| D4 | The T4 archetype demanded a prose source of record that the prompts never mentioned; both round-1 T4 cells took symmetric 0.5s. Worse, for lore the recorded rationale **is in the code** (a doc comment), so the criterion punished the best available answer. | Archetype redesigned around **rejected alternatives** — negative space that code cannot document. Prompt asks for the reason *and where it is written down*. See § T4, redesigned. |
| D5 | lore T1/T3 keys were structural, and Revision A's completeness lists were transcribed from round-1 *answers*, not derived from code. | Re-derived from source at the pin, every item carrying `file:line`. See § lore. |
| D6 | lexomancy T5 shield normalisation was under-specified; Revision A freed the denominator. | Kept free, tightened: the prompt now states the deterministic and degenerate-case requirements that were previously key-only. |
| D7 | Bench projects indexed with `authority: none`, so the lore repo's own authority/modality task ran against a neutrally-indexed project. | Round 2 runs `lore-bench` under the `lore-v1` profile. See § Run protocol, and `bench/README.md` § Round-2 setup step 3b. |
| D8 | `metrics.json` recorded no prompt identity, so a results directory could not be attributed to a prompt version. | `run.ps1` now records `task_set` and `prompt_sha256`. |
| D9 | **The Lexomancy vault pin is not enforced.** The bench workspace's `design` junction resolves to the live vault working tree, 3 commits ahead of `d5e0d53310`. Round 1 ran against an unpinned vault while claiming a pin. | **Resolved 2026-08-17 by restating the pin, not by enforcing it.** Wrysk: the post-pin commits are trivial and "would have already been present just not tracked", and "nothing has been touched there since we ran the last bench" — so the working tree is content-identical to what round 1 ran against and the drift is bookkeeping. Corroborated independently: every vault directory these keys cite is byte-identical pin↔HEAD, with one irrelevant post-pin addition. The vault pin is therefore **the working tree as of run day, SHA recorded in the results notes**; at time of writing that is `7604c27ed2f9a6d1764b44acbb348607d872b019`. Re-record it on run day rather than trusting this value. |
| D10 | Round-1 keys carried wrong paths and wrong facts that would have marked correct answers wrong: `2_Encounters/` (really `2_BattleMechanics/`), `5_Prototypes/` (really `5_Implementation/`), "1.6_ForgingSystem and 6.1_Lexinomicon claim axiom capacity" (they claim residue cost, nothing about capacity), "D-0016 reserves per-unit taunt state" (it mentions taunt; it reserves the seam), lexomancy T1's terminus at `PayloadExecutor` (which never touches `BattleUnit`), terrarium T1's physarum-only terminus. | All corrected below, each with the correction called out where a grader might otherwise carry the old belief forward. |

## Pins — unchanged

| Repo | Pin |
| --- | --- |
| lore | git `977364a` |
| latent-music-terrarium | git `3b1eacd56f` |
| Lexomancy code | cm `cs:134` |
| Lexomancy design vault | git `d5e0d53310` |
| Lexomancy tools | git `35a45a26ad` |

Every key below is written against this state. Moving a pin invalidates every
`file:line` in this document. See § Recommendation on the pins.

## Corpus scrub

`bench/setup-worktrees.ps1 -Apply -Scrub` deletes these paths from **both slots**
of each bench repo, and `bench/run.ps1` refuses to run a cell while any of
them exists:

| Repo | Path | Why |
| --- | --- | --- |
| lore | `design/9_Scratch/2026-08-15_e2e-round-1-plan.md` | Contains the round-1 task list and graded answers for all three repos. |

Deletion is not enough on its own: `run.ps1`'s post-T5 `git checkout -- .`
would restore a deleted tracked file mid-round. The scrubbed paths are
therefore excluded from the T5 `add -N`/`diff` pathspecs and from the restoring
checkout, exactly as `.lore.toml` and `.loreignore` already are.

`design/9_Scratch/2026-08-14_deferred-backlog.md` is **kept**. It names the ATX
trailing-`#` bug and the function it lives in, but it is a genuine repo
artifact that predates the benchmark and is the intended seam for T5 — a bug
tracker naming a bug is not contamination. Its effect on the task is recorded
in the T5 key.

## Task archetypes

- **T1 — Feature location / cross-file trace.** "Where does X happen; walk me
  through it." Graded on hop-list overlap against an enumerated key derived
  from source. Retrieval should shine on the entry hop.
- **T2 — Authority / modality.** "Is X still true, and is that the decided
  design?" Graded on separating current behaviour from binding canon, and on
  correctly reading the modality of the documents involved (`design_status`,
  supersession, self-declared authority). Lore's differentiator.
- **T3 — Recall sweep.** "List every place that does X." Enumerable key,
  graded on precision/recall. **A T3 whose vocabulary is consistent is not a
  T3** — a single grep wins it and both arms score 1, which is what happened to
  round-1 lore T3. Every round-2 T3 targets a concept whose spellings diverge.
- **T4 — The rejected-alternative "why".** See below.
- **T5 — Bounded implementation.** A <100-line change graded on the diff plus a
  green suite. Where the seam is trivially findable, the difficulty must live
  in the *correctness* of the change instead, and the key says which.

### T4, redesigned

Round-1 T4 asked "why is Y built this way?" and the key gave full marks only
for citing a prose source of record. Two things were wrong with that.

1. **The prompts never said so.** A criterion the off arm cannot know about is
   a bad test, whatever it is measuring.
2. **For a rationale-rich codebase the premise is false.** The lore T4 answer
   — the key-exchange convergence argument — is written out in full in a *doc
   comment* at `crates/lore/src/store/mod.rs:413-446` and again at
   `crates/lore/src/registry.rs:291-305`, both more completely than the commit
   message the key named. An answer citing the code was not a lesser answer;
   it was the better one. The key punished the correct behaviour.

The fix is not to loosen the citation rule. It is to ask a question **code
cannot answer**. A rejected alternative leaves no code behind: there is no
function to comment, no test to name, no symbol to grep. The only record of
why something was *not* built is prose — a ledger entry, a decision brief, a
review report, a commit message. So:

> **T4 — the rejected-alternative "why".** "Did we decide against X? Why, and
> where is that written down?" Full marks require (a) the actual recorded
> reason, not a plausible reconstruction, and (b) a citation to the source of
> record. The prompt asks for both in so many words, so both arms are aiming at
> the same target.

Scale: **1** = the recorded reason, materially complete, with a source of
record cited. **0.5** = the right reason but no source, or a source with a
materially incomplete reason. **0** = a reason that is not the recorded one,
including a plausible-sounding invention.

What counts as a source of record — any one is enough: a decision-ledger entry
(by ID or file), a decision brief, design note, handoff, roadmap, plan-doc
revision, review report, session report, or **the commit message** that records
it. Where the rationale genuinely does live in code as well, the key says so
explicitly and a code citation counts; that is a per-task fact, not a rule.

**On discrimination.** A rejected-alternative question is usually answerable by
both arms if either finds the document at all, so T4 will often be 1/1 on
score. That is fine and expected: **T4's primary signal is cost, not
correctness.** Round 1's largest single on-arm token win was a T4 cell (luna /
Lexomancy, −67% input tokens). The score exists to catch the failure mode where
an arm invents a rationale rather than finding one — which is exactly what an
agent that cannot find the prose will do.

## Metrics and grading

Unchanged from round 1: tool calls (native vs lore split), tokens, wall time,
compactions, and task success at 0 / 0.5 / 1 against the keys below.

**Every criterion below is checkable from what `run.ps1` actually captures**:
`answer.md` (concatenated assistant text), `diff.patch` (T5), `metrics.json`,
`events.jsonl` (raw stream, including tool arguments), `stderr.log`, and for
Lexomancy `cm-changed.txt`. Suites are run by the grader, not the agent, and
are not captured by the harness — so "suite green" is a grader action, recorded
by hand. Nothing in these keys grades an agent's *process*, because the harness
captures reasoning only as raw events and no grader should be asked to
adjudicate that.

Suite commands (grader-run, at the pin):

- lore: `cargo test --workspace`
- terrarium: `cd analysis; uv run --extra dev --extra server pytest -q`
- Lexomancy: EditMode run against an editor the grader keeps open. The agent
  never touches Unity.

---

## lore

Retrieval project: `lore-bench` (slot a) / `lore-bench-b` (slot b). Pin
`977364a`. All citations below are at that commit.

### lore T1 — trace an MCP search call to the ranked results

**Prompt**

> walk me through what happens when an mcp search call comes in, from the proxy
> to the ranked results. files and functions at each hop

Unchanged from round 1: it already states the ask plainly and the round-1 key
was right in shape, only unenumerated. The key below is the enumeration,
derived from source.

**Key — required hops (all seven for 1):**

1. **MCP tool entry** — `crates/lore-mcp/src/server.rs:160`
   `LoreServer::search`, with the wire request built by
   `impl From<SearchParams> for SearchRequest` at `crates/lore-mcp/src/server.rs:85`.
2. **Proxy → daemon over loopback HTTP** — `crates/lore-mcp/src/daemon.rs:151`
   `Client::search` → `Client::post` at `crates/lore-mcp/src/daemon.rs:165`;
   the base URL comes from the discovered handshake,
   `crates/lore-mcp/src/daemon.rs:39` `Endpoint::base_url`.
3. **Daemon route** — `crates/lore/src/daemon/http.rs:69` registers
   `POST /v1/search` → `crates/lore/src/daemon/http.rs:326` `search_route`.
4. **Query embedding, before the store lock** —
   `crates/lore/src/daemon/http.rs:333` calls
   `crates/lore/src/embed/mod.rs:166` `Embedder::embed_query`. `None` is not an
   error; it means this request runs lexical-only (D-0007).
5. **Search execution** — `crates/lore/src/daemon/search.rs:151`
   `search::execute`, entered through the single-owner store handle at
   `crates/lore/src/daemon/http.rs:337` (`state.store.with(...)`,
   `crates/lore/src/daemon/store_handle.rs`).
6. **Two arms** — `crates/lore/src/store/mod.rs:902` `Store::lexical_search`
   (BM25 over FTS5) and `crates/lore/src/store/mod.rs:949`
   `Store::vector_search` (cosine), both called from the acquisition loop at
   `crates/lore/src/daemon/search.rs:196-248`.
7. **Merge point** — `crates/lore/src/daemon/search.rs:351` `fuse_detailed`:
   Reciprocal Rank Fusion, `RRF_K = 60` at
   `crates/lore/src/daemon/search.rs:106`. *This is the answer to "where are FTS
   and vector candidates merged".*

**Credited, not required** (name any of these and it is a better answer, but
their absence does not cost score):

- the authority multiplier applied **after** fusion —
  `crates/lore/src/daemon/search.rs:384` via `authority_weight` at
  `crates/lore/src/daemon/search.rs:473`, constants at `:117-125`;
- window collapse — `crates/lore/src/daemon/search.rs:410-455`;
- the finality check that decides whether to fetch deeper —
  `crates/lore/src/daemon/search.rs:277` `page_is_final`;
- result projection — `crates/lore/src/daemon/search.rs:493` `to_result`;
- rendering back to the agent — `crates/lore-mcp/src/render.rs:30`
  `render::search`.

**Scale.** 1 = all seven required hops, in order, with the merge point named
(`fuse_detailed`) or the RRF fusion at that seam described unambiguously.
0.5 = one or two required hops missing or misattributed, or the merge point
left vague. 0 = the funnel is wrong (e.g. claims the MCP binary queries SQLite
directly, or that fusion happens in the store).

**Self-check.** Off arm can plainly succeed — the crate layout is legible and
`fuse` is greppable. On arm can plainly fail — the acquisition loop makes the
middle of `execute` easy to mis-narrate as a single fixed pull.

### lore T2 — is the fixed 50-candidate pool the decided design

**Prompt**

> is the fixed 50-candidate pool per search arm still how it works? is that the
> decided design

Unchanged. The prompt asks both halves the key grades, and the second half is
the authority question.

**Key — two conjuncts, both required for 1.**

*Current behaviour (no, it is not fixed):*

- Acquisition is a **loop**, not one fixed pull —
  `crates/lore/src/daemon/search.rs:196-248`.
- 50 is now the **first-round floor**, not a cap:
  `LEXICAL_CANDIDATES = 50` (`crates/lore/src/daemon/search.rs:70`),
  `VECTOR_CANDIDATES = 50` (`:73`), and the initial depth is
  `limit.clamp(max(50,50), MAX_CANDIDATES)` (`:194`) so a `limit = 100` page is
  never served from a pool of 50.
- Growth is `CANDIDATE_GROWTH = 4` (`:83`), ladder 50 → 200 → 800 →
  `MAX_CANDIDATES = 1000` (`:101`).
- It stops when the page is **provably** final (`page_is_final`, `:277`), when
  both arms are exhausted (`open == 0`, `:225`), or at the cap (`:233`).
- Credited: the reason it had to change — RRF means cross-arm agreement deep in
  both lists can outscore a shallow singleton, so a fixed pull omits results
  that belong at the *top* of the ranking, not just the tail
  (`crates/lore/src/daemon/search.rs:17-38`).

*Authority (no, it was never "decided"):*

- The detail design `design/3_Retrieval/3.1_Chunking_and_Ranking.md` is
  `design_status: leaning`, `decision_refs: [D-0004]`, and says of itself
  "Drafted by agent from research patterns; **not ratified** — treat specifics
  as `[!working]` unless a decision cites them" (`:10`). It is not canon.
- **No ledger entry fixes a candidate pool size.** D-0004 mandates hybrid
  search; D-0007 mandates graceful lexical degradation; neither speaks to
  depth. An answer that names D-0004 as the binding entry and says it does not
  constrain the number is correct.
- The change originated in
  `design/9_Scratch/2026-08-14_adversarial-review-session-3.md:117` ("The fixed
  candidate pool can omit the mathematically best RRF result and underfill
  pages") — a **scratch** review, evidence rather than canon.

**Scale.** 1 = both conjuncts: current behaviour described as an adaptive,
proof-terminated loop with 50 as a floor, **and** the modality answered
correctly (3.1 is leaning / not ratified; no decision fixes the number).
0.5 = current behaviour right but the design treated as decided because 3.1 or
a scratch doc says so, or vice versa. 0 = still describes a fixed 50-per-arm
pull as current behaviour.

**Trap this task is testing.** A confident-sounding detail-design document with
`design_status: leaning` is exactly the artifact an agent reads as canon. The
on arm should see the status annotation; the off arm has to open the
frontmatter and notice.

### lore T3 — every consumer of `design_status`

**Prompt**

> where does a doc's design_status actually get used? list every place in the
> code that reads it or acts on it

**Why this replaces the round-1 T3.** Round-1 T3 asked for index triggers. Its
key also demanded `design_status` consumers, which the prompt never mentioned
— that was the logged gap. But the *deeper* problem is that the trigger
enumeration is a bad T3: every producer calls `IndexQueue::request_full` or
`request_paths`, so one grep for `request_` finds all eight sites, and every
round-1 cell in both arms scored 1. A task both arms win by construction
measures nothing. The `design_status` sweep is the version of this task that
has the property T3 is supposed to have: **the concept is spelled several
different ways**, so a literal grep for `design_status` misses roughly half the
pipeline. (The index-trigger enumeration is preserved below as a reference, in
case a later round wants it back.)

**Key — the pipeline, ten stages. All citations at `977364a`.**

*Grep for `design_status` finds these (stages 1, 2, 5, 6, 8, 9):*

1. **Parse** — `crates/lore/src/chunk/markdown.rs:187` maps the frontmatter key
   to a value via `parse_status` at `crates/lore/src/chunk/markdown.rs:196`.
2. **Carriers** — `crates/lore/src/chunk/common.rs:127-130` `FileVault`
   (file-level, cloned onto every chunk) and `crates/lore/src/types.rs:93-100`
   `VaultMeta` (the public per-chunk shape), copied across at
   `crates/lore/src/chunk/common.rs:257`.
5. **Store write** — `crates/lore/src/store/mod.rs:754` writes
   `design_status, authority_tier, effective_tier, demotion`; the upsert at
   `:764`; the value taken from the chunk at `:770`.
6. **Schema** — `crates/lore/src/store/schema.rs:73-74` the denormalized
   column (NULL == unclassified), `:75` the precomputed
   `authority_tier`, `:82` the `chunks_by_status` index.
8. **Read back** — `crates/lore/src/store/mod.rs:1156` reconstitutes the
   status onto a `Chunk`.
9. **Wire + reporting** — `crates/lore-core/src/lib.rs:221`
   `SearchResult.design_status` (declared) and `:224` (effective tier /
   demotion, which is *not* the same field);
   `crates/lore/src/daemon/search.rs:501-522` `to_result` reports declared
   status and citations separately from the effective tier; label spelling at
   `crates/lore/src/daemon/search.rs:556` `status_label`.

*Grep for `design_status` does **not** find these (stages 3, 4, 7, 10) — this
is the discriminating half of the task:*

3. **Declared tier** — `crates/lore/src/types.rs:82` `authority_tier(status)`
   maps the enum to 3/2/1/0. Named `authority_tier`, not `design_status`.
4. **Effective tier** — `crates/lore/src/authority.rs:223` `effective(...)`
   validates the declaration against the project's active decision set and the
   file's path (ceilings for `9_Scratch` / `7_Research`, the session cap),
   reached via `crates/lore/src/store/mod.rs:1152`
   `AuthorityContext::verdict`. **This is where a declaration stops being
   believed.** An answer that omits `authority.rs` has missed the point of the
   whole pipeline.
7. **Recompute on index** — `crates/lore/src/daemon/index.rs:267`
   `refresh_authority`, called unconditionally on a full scan
   (`crates/lore/src/daemon/index.rs:139`) and conditionally on a path-scoped
   pass; it is what makes a ledger edit retroactively change what `decided`
   means, and it feeds `PassSummary.authority_recomputed` /
   `authority_violations` (`crates/lore/src/daemon/index.rs:83-88`).
10. **Ranking and filtering** —
   - ranking: `crates/lore/src/daemon/search.rs:473` `authority_weight` over
     the **effective** tier, applied after fusion at `:384`; constants at
     `:117-125`;
   - filtering: `crates/lore/src/store/mod.rs:178-195` `StatusFilter`
     (explicitly over the **declared** status), lowered to SQL at
     `crates/lore/src/store/query.rs:143` (unclassified → `IS NULL`) and
     `:154` (`IN (...)`);
   - the wire vocabulary the agent can pass:
     `crates/lore-mcp/src/server.rs:37-46` `StatusFilter` / `as_wire`, parsed
     back at `crates/lore/src/daemon/search.rs:482` `parse_status`;
   - human rendering: `crates/lore-mcp/src/render.rs:84` and
     `crates/lore/src/cli.rs:327`.

**Credited, not required.** Tests are correct colour and never required:
`crates/lore/tests/daemon_authority.rs`,
`crates/lore/tests/authority_laundering.rs`,
`crates/lore/tests/embed_search.rs:259-305`,
`crates/lore-core/tests/wire_compat.rs`.

**The distinction that separates a good answer from a complete one:** the
declared status and the effective tier are two different things. Declared is
what the document claims and is what `status:` filtering matches; effective is
what ranking uses, after `authority.rs` has validated the claim. An answer that
treats them as one field is at most 0.5 however many call sites it lists.

**Scale.** 1 = at least eight of the ten stages, **including** stage 4
(`authority.rs`) and the declared-vs-effective distinction. 0.5 = five to seven
stages, or ten stages with the distinction collapsed. 0 = only the literal
`design_status` string hits, i.e. the grep-shaped answer.

**Self-check.** Off arm can plainly succeed — following `DesignStatus` through
the type system gets there, it is just more expensive. On arm can plainly fail
— a semantic search for "design status" returns the parse and the wire types
and can miss `authority_weight` entirely.

**Reference (not graded in round 2): index-trigger enumeration.** Retained
because it was derived and may be wanted later. Full-scan producers, all
calling `IndexQueue::request_full` (`crates/lore/src/daemon/queue.rs:72`):
daemon startup seeding every registered project
(`crates/lore/src/daemon/mod.rs:230`); project registration via
`POST /v1/projects` (`crates/lore/src/daemon/http.rs:300`); explicit
`POST /v1/index`, no argument meaning all projects
(`crates/lore/src/daemon/http.rs:318-319`, CLI entry
`crates/lore/src/cli.rs:117`); watcher event-channel overflow, the sink's own
dropped-batch flag (`crates/lore/src/daemon/watch.rs:516-523`, set at `:157`);
a delivered event whose `need_rescan()` is set
(`crates/lore/src/daemon/watch.rs:565-569`); a `.gitignore` change inside a
project (`crates/lore/src/daemon/watch.rs:588-590`); watcher backend error
recovery (`crates/lore/src/daemon/watch.rs:604-615`); and the pending-path
storm promotion inside `request_paths` itself, past `MAX_PENDING_PATHS = 4096`
(`crates/lore/src/daemon/queue.rs:95-103`). Path-scoped producer: ordinary
debounced events (`crates/lore/src/daemon/watch.rs:594`). Correct non-triggers:
`lore-mcp` exposes neither registration nor reindex
(`crates/lore-mcp/src/server.rs:4`); startup registry reconciliation enqueues
nothing itself (`crates/lore/src/registry.rs` has no `request_*` call);
`search`/`expand`/`status` enqueue nothing; embedding-worker ticks are not
index passes.

### lore T4 — the code-graph subsystem we did not build

**Prompt**

> did we ever decide not to build a code graph thing? why, and where's that
> written down

**Why this replaces the round-1 T4.** Round-1 lore T4 asked why registry
reconciliation applies atomically. The recorded rationale for that is a **code
doc comment** — `crates/lore/src/store/mod.rs:413-446` states the key-exchange
argument, the `UNIQUE constraint failed: projects.key` failure, and the
three-step removals/release/claim ordering in more detail than commit
`60b3599`'s message does; `crates/lore/src/registry.rs:291-305` repeats the
argument. A key that awarded 0.5 for citing code was penalising the best
source. The task is not salvageable as a T4 because its premise ("the answer
lives in prose, not code") is false. It would make a perfectly good T1.

D-0005 is the rejected-alternative question: **no code exists for a subsystem
that was never built**, so nothing but prose can answer it.

**Key.** Source of record: `design/0_Canon/DECISIONS.md`, **D-0005 — "No graph
subsystem; structural queries are out of scope"** (`:58-64`). Supporting
evidence: `design/7_Research/raw/D_codegraph.md` (the research that fed it) and
`design/3_Retrieval/3.1_Chunking_and_Ranking.md`, which cites the CodeGraph
counter-churn failure as its anti-pattern for chunk identity.

Required content:

- **A decision exists and is active** — D-0005, in the ledger, not superseded.
- **The recorded reason**: semantic + lexical retrieval is expected to carry
  navigation on its own, and extra graph tool calls are **sharply diminishing
  returns when the goal is saving tokens** (`design/0_Canon/DECISIONS.md:64`).
  The token-cost framing is the load-bearing part; "we wanted to keep it
  simple" is a reconstruction, not the record.
- **A source of record cited** — the ledger entry by ID or by file.

Credited, not required: the coexistence clause — `codebase-memory-mcp` may keep
running alongside Lore for structural queries "as long as it earns its keep"
(`design/0_Canon/DECISIONS.md:64`). An answer that reports the decision as
"never build a graph, ever" without the coexistence clause is materially
complete enough for 1; one that reports it as "we might add one later" is not.

**Scale.** 1 = the decision, the recorded reason including the token-cost
framing, and a source of record. 0.5 = right decision, reason paraphrased away
from the record or no source cited. 0 = invents a reason, or reports that no
such decision was taken.

**Self-check.** Off arm can plainly succeed — `design/` is ~25 files and a grep
for "graph" reaches the ledger. On arm can plainly fail — a semantic search for
"code graph" surfaces `7_Research/raw/D_codegraph.md`, which is research
evidence with a path-ceilinged authority tier, and an answer sourced only there
reports the landscape rather than the decision. The expected signal here is
cost, not score; see § T4, redesigned.

### lore T5 — ATX trailing `#`

**Prompt**

> headings like "# Learning C#" lose the trailing # in heading paths. fix that
> per commonmark rules and add a test. make sure files that are already indexed
> pick up the fix

**What changed and why.** The last sentence is new. Round-1's key was silent on
persisted state; the on arm bumped `CHUNK_FORMAT_VERSION` 4 → 5, the off arm
did not, and Revision A ruled the bump unscoreable to avoid retro-fitting a
criterion onto one arm's behaviour. That was the right call *given the prompt*.
It is the wrong resting place, because the bump is genuinely correct: changing
what the chunker produces without invalidating the hash short-circuit leaves
every already-indexed file serving stale heading paths until its bytes next
change. The constant's own doc comment says exactly this — "Bump whenever
chunking policy changes in a way that should re-chunk unchanged files"
(`crates/lore/src/chunk/mod.rs:32-35`) — and it is a sibling module of the file
being edited, so it is reachable by both arms.

With the prompt asking for it in behavioural terms ("files that are already
indexed pick up the fix"), the criterion is fair: both arms are told, neither
is told where the mechanism lives.

**Key — scored criteria, all five required for 1:**

1. **The CommonMark closing-sequence rule is implemented.** A run of `#` at the
   end of an ATX heading closes it only when preceded by a space or tab (or
   when the line is nothing but `#`s), and may be followed only by spaces/tabs.
   Current code trims unconditionally:
   `crates/lore/src/chunk/markdown.rs:123`
   (`rest.trim().trim_end_matches('#').trim()`).
2. **`# Learning C#` keeps its trailing `#`** in the resulting heading path.
3. **A real closing sequence (`## Foo ##`) is still trimmed** — the fix must
   not be "stop trimming".
4. **Persisted state is invalidated**: `CHUNK_FORMAT_VERSION` is bumped from
   `4` (`crates/lore/src/chunk/mod.rs:44`), which is mixed into the per-file
   content hash at `crates/lore/src/daemon/index.rs:389-393` and so defeats the
   unchanged-file short-circuit at `:396`. Any equivalent mechanism that
   demonstrably re-chunks already-indexed files earns the same credit; a diff
   that only edits `scan_headings` does not.
5. **A regression test covers criteria 2 and 3.** Existing home:
   `crates/lore/src/chunk/markdown.rs` unit tests or
   `crates/lore/tests/chunk_windows.rs` / `chunk_invariants.rs`.

Plus: **`cargo test --workspace` green at the pin** (grader-run). Note that
bumping `CHUNK_FORMAT_VERSION` may require updating chunk snapshot tests under
`crates/lore/tests/snapshots/` if they encode the version; a diff that bumps
the constant and leaves the suite red fails on the suite criterion, not on 4.

**Scale.** 1 = all five plus green suite. 0.5 = criteria 1–3 and 5 correct but
4 missing (a correct fix that does not reach already-indexed files), or 4
present with the rule subtly wrong (e.g. requires a space but permits trailing
non-space characters after the run). 0 = the trailing `#` is still lost, or
`## Foo ##` is no longer trimmed, or the suite is red for reasons the diff
introduced.

**Self-check.** The seam is *not* hidden: `design/9_Scratch/2026-08-14_deferred-backlog.md:25-26`
names both the bug and `scan_headings`, so both arms locate it cheaply. The
difficulty is therefore deliberately relocated into correctness — the
CommonMark rule has three clauses and the persistence step is invisible unless
you go looking. Off arm can plainly succeed. On arm can plainly fail: retrieval
finds the function fast and the fast path is exactly the one that skips the
version bump.

---

## latent-music-terrarium

Retrieval project: `terrarium-bench` / `terrarium-bench-b`. Pin `3b1eacd56f`.
All citations at that commit. Indexed with **no authority profile**, on purpose
— the repo has no `design_status` convention and no ledger, so declaring
`lore-v1` there would assert a policy the corpus does not follow.

### terrarium T1 — stem loudness to species brightness

**Prompt**

> how does stem loudness end up changing how bright a species is on screen?
> trace it from the python side all the way through

Unchanged. The round-1 key was under-specified at two hops and over-specified
at the terminus; both are fixed below.

**Key — required hops (all seven for 1):**

1. `analysis/src/terrarium_analysis/stages/stems.py:138` `run(ctx)`, writing
   `ctx.stems` at `:165` from the per-stem curve `stem_activity` (`:96`).
2. `analysis/src/terrarium_analysis/emitter.py:14` `build(ctx)` — the `"stems"`
   channel is packed at `:18` via `pack(...)` at `:38`; the file-writing wrapper
   `emit` (`:44-47`) is equally creditable.
3. `web/src/timeline/loader.ts:13` `loadTimeline`.
4. **The sampler, both halves.** `web/src/timeline/sampler.ts:194` `getChannel`
   yields `{offset, dims}` only — the *values* come from
   `web/src/timeline/sampler.ts:164` `sampleAt` → `:168` `sampleAtTime`, called
   from `web/src/main.ts:1532` / `:1543`. An answer naming only `getChannel`
   has not traced the data.
5. `web/src/mapping/modulation.ts:1032` `Modulator.updateStemFollow`, copying
   `frame.values[c.offset + i]` into `stemRow` (`:1039`) and calling through at
   `:1041`; reached from `Modulator.update` (`:989`) ← `web/src/main.ts:1450`
   inside `advance` (`:1441`). Round 1's key skipped this hop entirely.
6. `web/src/mapping/stemfollow.ts:125` `StemFollow.update` and `:58`
   `followMultiplier`.
7. **The wire and the composition.** `ModTarget.setBrightFollow`
   (`web/src/mapping/target.ts:93`, impl `web/src/sim/physarum/physarum.ts:298`)
   hands the array **by reference**, once, at
   `web/src/mapping/modulation.ts:563`; the value is consumed in
   `uploadSpecies` (`web/src/sim/physarum/physarum.ts:1302`) where
   `follow = this.brightFollow[k]` and
   `light = min(brightness * follow, MAX_BRIGHTNESS)` (`:1389-1393`) is written
   to the species buffer at `:1397`.

**Any substrate satisfies hop 7.** `web/src/sim/plife/plife.ts:2684` /
`:2722` (`setBrightFollow` at `:872`) and `web/src/sim/vizfx/vizfx.ts:1187`
(`:328`) do the identical composition. `docs/roadmap.md:8` says "Particle life
is the product", so a plife-terminated trace is if anything the better answer.
The round-1 key named physarum only; that was an accident of scouting, not a
requirement.

**Credited, not required:** the headless export host as an alternative to
`main.ts` (`web/src/export/node-timeline-loader.ts:8` ←
`web/src/export/worker.ts:414`, sampler at `:466`, `sampleAt` + `modulator.update`
at `:556-557`); naming `web/src/runtime/sim-bundle.ts:192` as the convergence
point where both hosts construct the `Modulator`.

**The wrong wire — mark it.** `web/src/runtime/sim-bundle.ts:165`
`sim.setStemChannel(...)` also carries stems, into deposit/population world
mechanics (`physarum.ts:993-996`, `plife.ts:2209`), explicitly **not**
brightness (`sim-bundle.ts:163-164`). An answer that terminates there has
traced a real path to the wrong destination: **0.5 at most**, and 0 if it
asserts that is how brightness is set.

**Scale.** 1 = all seven hops with hop 4 and hop 5 both present. 0.5 = one or
two hops missing, or the by-reference wire described as a per-frame call, or
the stem-channel path traced instead. 0 = the analysis side is missing entirely
or the chain does not connect.

### terrarium T2 — is the raw embedding still the input

**Prompt**

> does the mapping still run off the raw 1024-dim embedding? and is that still
> the plan

**Why this replaces the round-1 T2.** Round 1 asked whether the web app still
uses `embedding.json` and whether `docs/handoff.md` is still accurate. The
handoff half is a giveaway: `CLAUDE.md:6-8` states in as many words that
handoff.md "is historical context, not authority" and that plan.md wins. An
agent that reads `CLAUDE.md` — which every agent does — has been handed the
answer. The genuinely hard modality problem in this repo is *inside*
`docs/plan.md`, and the new prompt points at it without naming it.

**Key — two conjuncts, both required for 1.**

*Current behaviour (no):*

- Nothing in `web/src` fetches `embedding.json` / `embedding.bin`. Every
  mention is a comment recording the removal:
  `web/src/timeline/types.ts:68-77` ("not loaded by the runtime any more
  (plan.md Revision 4) … nothing in `web/` fetches it"),
  `web/src/timeline/loader.ts:28-31`, `web/src/mapping/modulation.ts:15` and
  `:318`, `web/src/mapping/preset.ts:155` and `:278-280`,
  `web/src/mapping/stemfollow.ts:4`, `web/src/mapping/types.ts:42`,
  `web/src/sim/physarum/config.ts:79`,
  `web/src/mapping/tuninglog.ts:51` (a string label).
- What replaced it: the **driver bank**, ~16 named signals, built by
  `buildDriverBank` (`web/src/mapping/modulation.ts:322-356`).
- **The split state**: `analysis` still *optionally emits* the sidecar —
  `analysis/src/terrarium_analysis/emitter.py:49-68`, gated on
  `ctx.cfg.dump_embedding` (flag `analysis/src/terrarium_analysis/cli.py:70-72`,
  field `context.py:26`, populated `stages/character.py:138`,
  documented `analysis/README.md:168`). "It's gone" is only half true and an
  answer that says so without the analysis half is materially incomplete.
- **False-positive to mark**: `web/src/timeline/cache.ts:42` matches
  "embeddings" but means JS host embeddings, not MuQ. Counting it as a runtime
  consumer is an error; not mentioning it is fine.

*Modality (no — that plan was superseded, inside its own document):*

- `docs/plan.md:399-401` (Revision 3) states the raw-1024 input as the design.
  This is the plausible wrong answer and it is the one a top-down reader finds.
- `docs/plan.md:413-424` (Revision 4, 2026-08-07) supersedes it fourteen lines
  later: "Raw-1024 random projections react to everything and isolate nothing;
  and nobody can tune 1024 weights" (`:415-416`) → "Driver bank, ~16 named
  signals" (`:418`).
- Compounding it: `docs/plan.md:35-70` (Decision 1) and `:195-235` (Decision 4)
  sit **above** both revisions, are framed as settled (`CLAUDE.md:71` "The
  decisions in `plan.md` are settled"), and `docs/plan.md:381` records Decision
  4's mapping model as "rejected by the user after hands-on use". **Later
  revisions of this document override its own earlier Decisions.** Saying so is
  the modality half of the answer.

**Bonus traps, credited not required:** `README.md:3-4` ("Status: placeholder …
Nothing is implemented yet") and `CLAUDE.md:3-4` ("placeholder repo. No code,
no build, no dependencies") over ~200 source files, a shipping server and an
export pipeline — contradicted in the same file at `CLAUDE.md:44-54`.
`docs/roadmap.md:3-4` claims to be "the current sequencing authority for feature
work" — sequencing, not architecture — and `CLAUDE.md:64-72` never routes to it
at all, so an agent obeying CLAUDE.md misses the newest document.

Note that the placeholder claims exist **only at the pin**: upstream commits
`822c0fd` and `7f4e55a` rewrote the README and `8506c7d` ("docs: CLAUDE.md stops
calling this a placeholder repo") removed the other. These traps vanish if the
terrarium pin ever moves — one of the reasons § Recommendation on the pins
argues against moving it.

**Scale.** 1 = both conjuncts: the runtime no longer consumes it *and* analysis
still emits it, plus Revision 4 identified as superseding Revision 3 within
plan.md. 0.5 = current behaviour right but the plan read off Revision 3 or off
a "Decision" heading, or the analysis half missed. 0 = claims the runtime still
consumes the sidecar.

### terrarium T3 — every runtime reader of channel and event data

**Prompt**

> list every place in web/src that reads channel or event values at runtime.
> sampler calls and features-frame reads both count

**Why the second sentence.** Round-1's wording ("reads timeline channel or
event data after load") does not settle whether a `FeaturesFrame` pass-through
counts, whether reading `manifest.track.id` for a filename counts, or whether
the substrates' per-tick `frame.values[...]` reads count. They must — they are
the busiest consumer path in the app, and round 1's key omitted all three of
them. An ambiguity both arms are graded against but neither was told about is
the defect this fixes; naming the two categories settles scope without saying
where anything is.

**Key.** Grade **required core + precision**, not a recall percentage. The
denominator legitimately swings between 8 and 16 files depending on where the
producer/consumer line falls, so a percentage threshold grades the rubric
author, not the agent.

*Required core — all seven for 1:*

1. `web/src/main.ts` — the advance loop `sampler.sampleAt(tick)` (`:1532`,
   `:1543`), `segmentIndexAt` (`:1383`), and the explorer tiles
   `getChannel('stems'|'novelty16'|'actChorus')` (`:553`, `:575-576`, `:585`).
2. `web/src/mapping/modulation.ts` — `buildDriverBank` reading
   `timeline.manifest.grid` (`:324`) and `timeline.channels.get` (`:325`,
   `:334`); `getChannel('stems')` (`:555`); frame reads at `:982-984`, `:997`,
   `:1006`, `:1039`; `segmentIndexAt` (`:1046`).
3. `web/src/runtime/sim-bundle.ts:165`, `:167`, `:173`.
4. `web/src/sim/impulses.ts:340` (`new EventCursor(events, …)`), `:417`,
   `:424`.
5. `web/src/debug/overlay.ts` — `getChannel`/`hasChannel` (`:70`, `:72`, `:91`,
   `:105`), `segmentAt`/`beatStateAt` (`:201-202`), `duration` (`:208`),
   `sampler.timeline.manifest…` (`:213`, `:236`, `:270`, `:298-299`, `:366`),
   `rawAt` (`:340`, `:352`, `:371`).
6. `web/src/export/worker.ts:416`, `:419`, `:556`.
7. **At least one simulation substrate** —
   `web/src/sim/physarum/physarum.ts:270-282` + `:993-996`, or
   `web/src/sim/plife/plife.ts:820-833`, `:848-854`, `:2191`, `:2209`,
   `:2220-2221`, or `web/src/sim/vizfx/vizfx.ts:342-355`, `:1094`, `:1103`.

*Credited, never required, and never penalised either way* (state this to the
grader so nobody adjudicates a boundary the agent was not given):
`web/src/mapping/stemfollow.ts:125-141` (reads an already-extracted `stemRow`,
never the sampler); `web/src/explore/rig.ts:309-312` (pure `FeaturesFrame`
pass-through, acknowledged in the comment at `:308`);
`web/src/sim/types.ts:50-57` `NullSim` (real code, effectively dead in shipped
hosts); `web/src/main.ts:864`, `:953` (`manifest.track.id` — metadata, not
channel data); `web/src/main.ts:298-300`, `:323`, `:330`.

*Precision is graded and is checkable:* every `file:line` an answer cites must
resolve to a real read. Invented or wrong citations cost score; a short,
correct, well-cited list beats a long speculative one.

**Scale.** 1 = all seven core items, no fabricated citations. 0.5 = five or six
core items, or seven with one or more citations that do not resolve. 0 = misses
the advance loop or `modulation.ts`, i.e. does not find the main consumption
path.

**Self-check.** Off arm can plainly succeed — `getChannel` is greppable. On arm
can plainly fail — the substrates read `frame.values[offset + k]` with no
sampler call anywhere nearby, which is exactly what a semantic search for
"reads timeline channel data" will not surface, and it is where round 1's own
key fell down.

### terrarium T4 — the simulation substrate we dropped

**Prompt**

> which sim substrate were we leaning toward before the current one? why'd we
> drop it, and where's that written down

**Why this replaces the round-1 T4.** Round 1 asked why modulation uses a
seeded random projection over a driver bank instead of the raw embedding. The
rationale is stated nearly verbatim in the header comment of the one file any
agent must open to answer at all — `web/src/mapping/modulation.ts:14-28`:
"Revision 3 projected from the raw 1024-dim MuQ embedding. That reacts to
everything and isolates nothing … Nobody can tune 1024 weights." A `grep -rn
"driver bank" docs/` also lands on `docs/plan.md:413` in one query. Both arms
win it; it measures nothing. (The *supersession* around it is worth testing —
it is now terrarium T2.)

The dropped Lenia / continuous-CA substrate is the one recorded rejection in
this repo that is **genuinely absent from code, comments, tests and config**. A
tree-wide search for `Lenia | reaction-diffusion | continuous.CA | fractal
shell` hits only `docs/plan.md`, `docs/scaffolding-notes.md`,
`docs/research/simulation.md` and `docs/research/webgpu-simulation.md` —
nothing under `web/`, `analysis/`, `tools/` or `web/tests/`. And a lazy grep
for `reject|abandon|dropped|discard|ruled out` across those docs returns
**zero** hits touching it: reaching it means reading the scaffolding-notes
supersession block or the simulation survey, not matching a keyword.

**Keep the prompt keyword-free.** Naming "Lenia" in the prompt collapses the
task to one grep. The difficulty here is phrasing-sensitive and that is the
whole point.

**Key.** Sources of record: `docs/research/simulation.md:38-40` and,
equivalently, `docs/scaffolding-notes.md:10-12`.

Required for 1:

- **Names the dropped leaning** — Lenia / continuous cellular automata as the
  substrate the project was heading for before the current ones.
- **The recorded reason**, which is a specific technical argument, not "it was
  too complicated": "Interesting behaviour lives on a thin fractal shell, so
  the midpoint of two good parameter sets is usually bad. That breaks every
  safe interpolation scheme. Not viable as a v1 substrate."
  (`docs/research/simulation.md:38-40`.)
- **A source of record cited** — either doc above.

Credited, not required: the supporting evidence at
`docs/research/simulation.md:24-36` (the arXiv µ–σ mapping, 30.5% vs 14%
interesting-on-boundary, "death, metamorphosis, or explosion") and
`:203`; and the inverted contrast with physarum's own justification at
`docs/plan.md:132-134` ("essentially cannot die and cannot explode"), which is
the same axis and is what actually decided it.

**Precision penalty — the built-in trap.** *Flow*-Lenia **survived** as a v2
option: `docs/plan.md:186` and `:191` keep it "on the table as a second
interpretation of the same timeline (v2, not v1)". An answer that reports
Flow-Lenia as rejected has keyword-matched rather than read; cap it at 0.5.

**Scale.** 1 = the substrate, the interpolation/fractal-shell reason, and a
source of record. 0.5 = the substrate and a source but the reason paraphrased
into something the record does not say, or Flow-Lenia reported as rejected.
0 = names no dropped substrate, or invents a reason.

**Second choice, if this one is ever burned.** The preset simplex / k-means
anchors (`docs/plan.md:195-234` for the model, `:381-385` for the rejection).
Weaker, because the artifact is all over the code — `mapping/preset.ts`,
`persist.ts`, `slew.ts`, `tuninglog.ts`, `ui/workbench.ts`, and a live test at
`web/tests/modulation.test.ts:787-809` asserting "the simplex is gone" — so an
agent can confabulate a technical rationale the record does not support. If
used, scope the criterion strictly to the *recorded* reason, which is a taste
and workload verdict and **not** a technical failure: "I do not care about
scenes. I want real time reactivity and morphing parameters."

**Do not write a key that says the simplex rejection led to today's system** —
it skips a hop. Revision 3 replaced the simplex with a seeded random projection
*over the raw 1024-dim embedding* (`docs/plan.md:399-401`); Revision 4
(`:413-424`) then replaced *that* with the driver bank.

**Self-check.** Off arm can plainly succeed — `docs/` is small and a patient
agent reading `scaffolding-notes.md` finds it. On arm can plainly fail — the
retrievable phrasing is a substrate survey, and semantic search for "why did we
drop" returns nothing, since the record never uses rejection vocabulary.

**Coverage caveat.** `docs/research/audio-analysis.md`,
`audio-embeddings.md`, `structure-analysis.md` and the `docs/handoffs/` briefs
were swept by keyword only, not read end to end. A rejection recorded there in
unusual vocabulary could have been missed; if a run surfaces one, credit it.

### terrarium T5 — put the WAV in the track content version

**Prompt**

> the server's track content version only hashes the timeline files but we
> serve audio.wav too. make the version cover the wav and update the test

**What changed.** "include the wav in the version" → "make the version cover
the wav". The stronger wording is deliberate: a version that does not change
when the wav changes has not covered it, which puts the memoisation fingerprint
inside what the prompt asks for rather than inside the key alone.

**Key — three scored criteria plus the suite.**

1. **The hash covers the wav's bytes.**
   `analysis/src/terrarium_analysis/server.py:242`
   `timeline_content_version(manifest_path, binary_path)` currently hashes
   `manifest_path.read_bytes()` (`:244`) and `binary_path.read_bytes()` (`:245`)
   and returns `sha256(...).hexdigest()[:16]` (`:246`). Both call sites move
   with it: `server.py:291` (`track_entry`, the `/tracks` `version` field) and
   `server.py:484` (`snapshot_export_inputs`, the export staleness guard) — or
   the wav path is derived internally from the track dir.
2. **The memoisation fingerprint covers it too.** `server.py:267`
   `fingerprint = (ms.st_size, ms.st_mtime_ns, bs.st_size, bs.st_mtime_ns)`,
   checked against `store.cache` at `:268-270`. **Hashing the wav without
   extending this leaves the bug fully intact** — the version simply never
   recomputes when only the wav changed. This is the discriminating criterion;
   weight it accordingly.
3. **A missing wav is handled.** Audio is optional (`hasAudio`,
   `server.py:290`); `audio.wav` is written at `server.py:322-326` and served
   from `server.py:711-719`, whitelisted at `server.py:82`. A diff that raises
   `FileNotFoundError` on an audio-less track fails.
4. **A test alongside `analysis/tests/test_server.py:153`**
   (`test_version_changes_only_when_the_timeline_content_changes`) that mutates
   `audio.wav` and asserts the version moves. Fixture `write_track`
   (`test_server.py:85`) writes `audio.wav` by default (`:101-102`).

Plus **`uv run --extra dev --extra server pytest -q` from `analysis/` green**
(grader-run; extras confirmed at `analysis/pyproject.toml:19` and `:28`).

**Wrong-but-plausible implementations, all of which fail:** hashing the wav's
mtime or `st_mtime_ns` rather than its bytes — this contradicts the function's
own docstring argument (`server.py:252-256`: "not an mtime: re-running the
pipeline with the same seed rewrites both files with identical bytes") and
breaks the *existing* first half of the test at `test_server.py:157-160`;
hashing the path string or the size; fixing the hash and not the fingerprint.

**Not required, and not an error either way:** bumping
`CACHE_NAME = 'lmt-tracks-v1'` (`web/src/timeline/cache.ts:27`). The browser
cache self-heals — `urlsFor` already includes `audio.wav` (`cache.ts:69`) and
`invalidateIfStale` (`cache.ts:106-112`) evicts on a version change. An answer
that *claims* the bump is required is mildly wrong; one that makes it anyway is
not penalised. Genuinely stale after this change: the comment at
`cache.ts:20-21`. Irrelevant: `EXPORT_RECIPE_VERSION`
(`web/src/runtime/recipe.ts:37`), `MODULATION_VERSION`,
`analysis/.../timeline.py:21 VERSION`.

**Scale.** 1 = criteria 1–4 and a green suite. 0.5 = criteria 1, 3, 4 with the
fingerprint (2) missed — a correct-looking fix that does not actually work — or
all four with the suite red for reasons the diff introduced. 0 = mtime-based,
or crashes on an audio-less track, or no test.

**Self-check.** Without criterion 2 this task is a two-line edit both arms
produce and it measures nothing; with it, the task is "notice the cache in
front of the thing you fixed", which neither arm gets for free.

---

## Lexomancy

Retrieval project: **`Lexomancy`** (the main root) for both slots — the walker
does not follow junctions, so a bench root indexes only its own loose files.
The main root is frozen and read-only during runs and already indexes under
`lore-v1 (rank)`. Pins: code `cs:134`, vault `d5e0d53310`, tools `35a45a26ad`.
Code paths below are relative to `Lexomancy/Assets/Scripts/`; vault paths to
`design/`.

**Corpus caveat, verified 2026-08-17 and not fixed here.** The bench
workspace's `design` junction resolves to the **live vault working tree**, which
is currently 3 commits ahead of the stated pin `d5e0d53310`. The vault
directories every key below cites were checked byte-identical between the pin
and HEAD, with one exception that nothing here depends on
(`2_BattleMechanics/2.6_impl/50_N4_Content_Proposal.md`, added after the pin).
But the vault pin is **not actually enforced by the setup** — it is a claim, not
a mechanism. Either freeze the vault worktree at `d5e0d53310` before the round
or restate the pin as "vault working tree as of run day, recorded by SHA". Two
directory names in the round-1 plan doc were also wrong: `2_Encounters/` is
really `2_BattleMechanics/`, and `5_Prototypes/` is really `5_Implementation/`.

The C# **is** at the pin: `cm status` shows `cs:134 - head` with no
`Assets/Scripts` entries in the changed list, and `BattleKernel`, `Loot` and
`State` are identical between the main workspace and `Lexomancy-alt`.

### lexomancy T1 — a Surge cast to enemy damage

**Prompt**

> how does a surge cast i submit actually end up damaging an enemy? walk me
> through the code path, files and classes at each step

Unchanged. (Note that the round-1 *plan doc* described this as starting "from
the wordplay UI", which is actively misleading — `BattleDirector` lives under
`Gameplay/WordplayScene/Battle/` but the submitting UI lives under
`Gameplay/BattleScene/Composer/`. The prompt never said that and does not need
to; the key below just has to not repeat the mistake.)

**Key — eight required hops.** The round-1 key was right in shape and wrong at
both ends: it started one hop too late and stopped one hop too early.

1. **The UI gesture** — `Gameplay/BattleScene/Composer/BattleComposerController.cs:979-992`:
   `Submit()` builds `SurgeAction.Cast(cast, preview.Focus.Value, _aim.Aim)`
   (`:983`) and calls `BattleDirector.Instance.SubmitSurgeAction(action)`
   (`:992`). Siblings `Mulligan()` `:1002`, `Pass()` `:1012`.
2. **`BattleDirector.SubmitSurgeAction`** —
   `Gameplay/WordplayScene/Battle/BattleDirector.cs:293-301`. It **does not
   step**; it sets `submittedSurgeAction` (`:300`) behind a guard (`:296-298`).
   An answer that has this call the simulator is wrong about the control flow.
3. **`PlayOut` detects the hold** — `BattleDirector.cs:311-341`, branch at
   `:328-335` (`sim.HasInteractiveUnit && sim.IsInteractiveUnitDueNext` `:328`,
   `yield return AwaitSurgeAction()` `:333`).
4. **`AwaitSurgeAction`** — `BattleDirector.cs:385-406`: `WaitUntil` `:393`,
   consume `:395-397`, **`sim.Step(action)` at `:400`**. This is the UI→kernel
   boundary.
5. **`BattleSimulator.Step(SurgeAction)`** —
   `BattleKernel/BattleSimulator.cs:227-238` (contract guard `:231-236`,
   `StepInternal(action)` `:237`) → **`StepInternal`** `:240-297`, dispatching
   at `:281-282`.
6. **`ApplyCast`** — `BattleSimulator.cs:344-370`, the four consequences.
7. **`ExecuteAbility` → `PayloadExecutor`** — `BattleSimulator.cs:743-775`, the
   call at `:750`, into
   `EncounterKernel/Effects/Payloads/PayloadExecutor.cs:58` `ExecuteAll` →
   `:40` `Execute` → the handler registered at `:18`. (`_executor` constructed
   at `BattleSimulator.cs:77`, field `:34`.)
8. **The damage actually lands** —
   `EncounterKernel/Effects/Handlers/DamageHandler.cs:34` calls
   `context.ResolveDamage(query)` → `BattleKernel/BattleEffectContext.cs:86`,
   where `evt.ActualDealt = target.TakeDamage(evt.Remaining)` at `:135` →
   `BattleKernel/BattleUnit.cs:125-133` mutates Block then CurrentHP.
   **Round 1's key stopped at `PayloadExecutor`, which never touches
   `BattleUnit`.** The question asks how an enemy gets damaged; an answer that
   stops at the executor has not answered it.

**Legitimately skippable** (thin dispatch, no score cost): `ActInteractive`
(`BattleSimulator.cs:310-329`), `ScalePayloads` (`:414`),
`PayloadExecutor.Execute`'s inner dispatch,
`ExecuteSurgeExpression`/`ResolveTargets` (`:409-420`) as a named hop.

**Credited, never required** — all real, all on or beside the path:
`SurgeState.CommitCast` (`BattleKernel/Surge/SurgeState.cs:128`, called
`BattleSimulator.cs:357`); `ResolveCounterForCast` (`:1069`, called `:349`) and
`ApplyCounterConsequences` (`:363`); `EvaluateFocusTarget` (`:486`, called
`:353`); `ApplyBroadRider` (`:373`, called `:361`); `CommissionSpecial`
(`:428`, called `:368`); `TargetHeuristic.ChooseTarget` (via `ResolveTargets`,
`:1238`); `ISurgePresenter.OnSurgeCast/OnSurgeResume`
(`BattleDirector.cs:402-403`); `SurgeCommission`, `CastPreview`,
`IBattleObserver.OnCommissionReaimed`.

**One adjacent symbol that is a trap.** `SurgeCounterScoring`
(`Gameplay/WordplayScene/Battle/SurgeCounterScoring.cs:56`) is **UI-side
preview scoring only** — called from `BattleComposerController.cs:1069`,
`:1071`, `:1395`, `:1397` and `SurgeBoundaryScorer.cs:129`, `:160`. Mentioning
it as related is fine. **Placing it inside the kernel chain is wrong** and
costs score.

**Scale.** 1 = all eight required hops, in order, terminating at
`BattleUnit.TakeDamage`. 0.5 = one or two hops missing, or the chain stops at
`PayloadExecutor`, or `SubmitSurgeAction` is described as stepping the
simulator. 0 = the UI hop and the kernel hop are not connected, or
`SurgeCounterScoring` is presented as the damage path.

### lexomancy T2 — do axioms cost residue, and are there slots

**Prompt**

> do axioms cost lexic residue? is there a slot limit or tiers?

Unchanged — it is the strongest task in the set and the prompt is already
right.

**Why it is the strongest.** The ledger has **no `Superseded-by:`
back-reference anywhere** (verified absent in `design/0_Canon/DECISIONS.md`).
Supersession is forward-only, so establishing that a document is dead means
reading forward through the whole ledger. That is a multi-hop authority
question that grep does not shorten.

**Key — the answer.** No, axioms do not cost Lexic Residue, and there are no
slots or tiers. Full credit requires citing the **ledger supersessions**;
citing `1.6.5_Axioms.md` as authority fails.

- **D-0006 — Axiom capacity and duplicates** (`design/0_Canon/DECISIONS.md:125`,
  Status **Accepted** `:128`): "Axiom acquisition is unlimited; axioms are fixed
  and unique … no capped-capacity machinery is built" (`:131-133`); "No slot
  array, forced replacement, or `slotCost` in the framework" (`:137-139`).
  Supersedes clause verbatim at `:140`: "**Supersedes:** [[1.6.5_Axioms]] §3–4
  slots/tiers/replacement direction."
- **D-0002 — Forge moment and economy** (`:41`, Accepted `:44`): "The Axiom
  forge occurs post-guardian only. **Lexic Residue does not exist**; unforged
  words expire" (`:47-50`); "No residue currency anywhere in the design"
  (`:53-54`). Supersedes clause at `:55-56`: "**Supersedes:**
  [[MetaProgression_Forging]] §3 residue direction; [[1.6.5_Axioms]] §2
  residue-cost acquisition."
- **D-0010** (`:244`, Accepted `:247`) *partially* supersedes D-0002 at
  `:262-265` — "D-0002's 'phrase content words are consumed by inscription'
  clause only; the rest of D-0002 … stands" — i.e. it **re-affirms** no
  residue. An answer that reads D-0010 as retiring D-0002 has misread a partial
  supersession and caps at 0.5.

**The document under test.** `design/1_GameSystems/1.6_Forging/1.6.5_Axioms.md`
has **no frontmatter** (line 1 is `# Axioms (Draft Spec)`; the `---` at `:7` is
a horizontal rule, not a fence). Residue cost at `:20`; slots/tiers section at
`:25-33`. **It specifies no slot count and no named tiers** — "may be limited",
"(names TBD)", "slot cap (TBD)". A key must not ask how many slots it defines;
that is unanswerable from source.

**Traps — two of round 1's four framings were false and must not be graded:**

- `design/1_GameSystems/1.6_ForgingSystem.md:41-47` — "Lexic Residue is the
  primary forging resource", cost at `:36`. **It makes no axiom-capacity
  claim.** No frontmatter, no supersession notice: *silently* stale, and
  therefore the sharper of the two traps.
- `design/6_UserInterface/6.1_Lexinomicon.md:44` — "committing consumes Lexic
  Residue". **Also no axiom-capacity claim**, and it **self-flags** at `:1-5`
  ("[!warning] Under rework (2026-07-27) … leans on retired concepts (bind
  slots, Lexic Residue, forge tab)"). A retriever that fetches only the `:42-44`
  window misses a disclaimer forty lines above — a good failure mode to watch,
  but a *weaker* trap than the silent one.
- `design/2_BattleMechanics/2.4_GuardianBattle_Surge.md` —
  `design_status: exploration` (`:2`), `decision_refs: D-0015, D-0016`
  (`:4-6`), "[!warning] Partially superseded by D-0015 / D-0016" (`:11`).
  Conflict worth crediting if spotted: `DECISIONS.md:460` still calls it
  "unclassified workspace", which its own frontmatter contradicts.
- `design/5_Implementation/5.8_BoardEffectSubstrate_Stages.md` (`leaning`,
  `:2`) claims at `:27-29` that "D-0004 — presence-gating is **still the
  accepted model**" (also `:16-18`, `:204`); the same claim at
  `design/5_Implementation/5.9_PlayablePrototype_EchoUX.md:39-40` (`leaning`,
  `:2`). **Both are wrong and the trap is sharper than round 1 knew**: D-0007
  (`:145`, Accepted `:148`) carries "**Supersedes:** D-0004." at `:172` — bare
  and unqualified, unlike every partial supersession in the ledger — while
  D-0004's own Status line at `:84` **still reads `Accepted`**. Reading status
  fields alone gets this wrong.
- Do **not** conflate D-0008's removal of save/bind capacity (`:180`, `:190`,
  `:194-195`, `:203`) with axiom slots. Those are socket slots.

**Scale.** 1 = no residue and no slots/tiers, sourced to D-0006 and D-0002 by
their supersession clauses, with `1.6.5_Axioms.md` correctly identified as
superseded rather than authoritative. 0.5 = right conclusion sourced only to
`1.6.5` or to a stale doc, or a partial supersession misread as a full one.
0 = reports that axioms cost residue or that slots/tiers exist.

### lexomancy T3 — residue still referenced in code

**Prompt**

> i thought we removed residue from the design. is it still referenced anywhere
> in the game? code and assets, list everything you find

**What changed.** "in the code anywhere" → "anywhere in the game … code and
assets". Round 1's scope was ambiguous in a way that made the key
unfalsifiable: `Assets/Scripts` and `Assets/` give *different correct answers*,
and the three genuine non-`.cs` references are the most interesting finding in
the task. Naming the scope is stating the ask; it leaks nothing.

**Key — the naming mismatch is the point.** Code says **`Lexonic`**; the ledger
says **`Lexic`**. `Lexic Residue` / `LexicResidue` appears **zero times** in
`Assets/Scripts`. There is no third variant.

*Required — the seven C# gameplay files:*

| File (under `Assets/Scripts/`) | Lines |
| --- | --- |
| `State/PlayerStats.cs` | 18, 28, 37, 44, 52, 71, 81, 90, 100, 109, 113, 117, 125, 134, 197, 199, 201, 203, 207, 209, 211, 220, 228, 235 |
| `Loot/LootApplicator.cs` | 55, 56, 57, 58, 91, 97, 108 |
| `Loot/LootTableSO.cs` | 52, 78, 93, 94, 109, 110, 125 |
| `Loot/LootRewardDefinitionSO.cs` | 64, 66, 67, 69, 73 |
| `Loot/LootTypes.cs` | 14, 295 |
| `State/RunState.cs` | 252 |
| `UI/LootPanelController.cs` | 154 |

*Required — the three non-`.cs` gameplay references round 1 missed entirely:*

- `Assets/Zones/TestZone/Loot/LexonicResidueReward.asset:13-16` — a real
  ScriptableObject instance; `:14` binds
  `Assembly-CSharp:Lexomancy.Loot:LexonicResidueRewardDefinitionSO`, `:15`
  `displayName: Lexonic Residue`, `:16` the description string.
- `Assets/Zones/TestZone/Loot/TestLootTable.asset:31` — `- GroupName: Residue`.
- `Assets/Prefabs/GlobalManagers.prefab:298` — `LexonicResidue: 0`, a
  serialized `PlayerStats` field.

**This coupling is the insight the task exists to surface**: deleting the C#
alone breaks live authored assets. An answer that says so scores 1 even if its
line lists are slightly short.

*Required to be excluded — the false positives:* the English word "residue" in
the playable-word corpora, `Assets/Scripts/Dictionary/enable1.txt:128675-128676`,
`playable_words.txt:130097-130098`, `us-clean.txt:57459-57460`, plus the
lexicon/tokenizer `.asset` noise (`Assets/Content/Lexicon/*`,
`Assets/Resources/WordFrequencies.asset`,
`Assets/StreamingAssets/ONNXModels/tokenizer_export/*`). Reporting a raw grep
count that folds these in is exactly the failure mode. It is fine to list them
*as* noise; it is not fine to list them as references.

*Credited:* noticing that **no test anywhere mentions residue** —
`PlayerStats.SpendLexonicResidue` / `GainLexonicResidue` are entirely untested.
Also: there are no localization or string tables in this project, so the
user-facing strings are hardcoded (`LootTypes.cs:295`,
`LootPanelController.cs:154`, and the `.asset` `displayName`).

**Why this is a real recall test — the numbers.** In `Assets/Scripts`:

| Query | Yield |
| --- | --- |
| the ledger's spelling, `"LexicResidue"` / `"Lexic Residue"` | **0 true hits** |
| the ledger's word, `"Lexic"` | 992 lines, **0 true hits** (every `Lexicon`, `lexical`, `LexicalPower`) |
| the correct identifier, `"LexonicResidue"` | 37 of 53 lines — **misses 16** |
| ground truth, `-i "residue"` | 53 lines / 10 files (47 lines / 7 files are gameplay) |

The 16 misses are the bare-word occurrences (`totalResidue`,
`LootTableSO.cs:78`, `:109`; the `residue` parameter and prose at
`PlayerStats.cs:113`, `:117`, `:197`, `:207`, `:235`) and the space-separated
display strings (`LootApplicator.cs:58`, `:108`,
`LootRewardDefinitionSO.cs:64`). An agent that starts from the ledger's
vocabulary gets **nothing**.

**Scale.** 1 = all seven C# files plus at least two of the three asset/prefab
references, with the dictionary hits either omitted or explicitly marked as
noise. 0.5 = the seven C# files only, or the seven plus unmarked dictionary
false positives. 0 = fewer than five C# files, or reports residue as absent.

### lexomancy T4 — the lanes we did not build

**Prompt**

> did we look at lanes for targeting? why'd we drop it, and where's that
> written down

**Why this replaces the round-1 T4.** Round 1 asked why targeting is RNG-free
and what the pick order is. Both halves are recoverable from code — the
determinism rationale is in `BattleKernel/TargetHeuristic.cs:10-11` ("the same
board and the same payloads always produce the same target, which is what keeps
a replay byte-identical"), the ladder is a 23-line XML doc comment in the same
file's header, and the **recorded** `enemies[0]` motivation is quoted almost
verbatim in *four* test comments:
`BattleKernel/Tests/AimAndTargetHeuristicTests.cs:22-24` and `:179-182`,
`Spellslinger/Tests/Editor/SlingEnemyIntentTests.cs:109-111`, and
`SlingCastVerdictTests.cs:282-284` — the last two quoting the ledger sentence
directly. A code-only agent wins it. It measures nothing.

The **lanes** rejection is the negative space: `grep -i lane` across
`BattleKernel` returns only mechanical uses of *lane order* as a tiebreak
(`BattleSimulator.cs:13`, `:26`, `:42`, `:44`, `:88`, `:374`;
`ScoreTimeline.cs:9-12`, `:17`; `BattleTimelineMath.cs:9`, `:16`, `:39`,
`:41`) and nothing about lanes ever having been a design option. No code exists
for a thing that was not built.

**Key.** Source of record: `design/0_Canon/DECISIONS.md`, **D-0016** (heading
`:467`, Status **Accepted** `:470`), rationale block `:492-495`.

Required for 1:

- **Lanes were considered as the targeting model and rejected.** Verbatim:
  "**Lanes rejected as added complexity**" (`:492-495`); independently
  "No lanes, no modal, no sticky pre-selection" (`:481`).
- **What the rejection was weighed against** — the recorded problem it had to
  solve, "`enemies[0]` made every **enemy** attack default onto the
  Lexomancer", and what shipped instead: aim visible at decision time
  (projection), committed atomically (drag/confirm), and absent when the
  question is absent (skip-when-singular) (`:492-495`).
- **A source of record cited** — D-0016 by ID or by file.

**Grade against the verbatim text, not round 1's paraphrase.** Round 1's key
said "every attack" (it is "every *enemy* attack") and "lanes were rejected as
complexity" (it is "**added** complexity", a bare four-word dismissal). Neither
paraphrase should be treated as the standard an answer is held to.

**Precision trap.** "Lane order" survives as rung 4 of the targeting ladder. An
answer that reports lanes as *present in the design* because the tiebreak
mentions them has keyword-matched; cap at 0.5. Conversely, noticing the tension
— lanes rejected player-facing while lane order remains the internal final
tiebreak — is worth crediting.

**Scale.** 1 = lanes named as a rejected alternative, the recorded reason, and
D-0016 cited. 0.5 = lanes identified but the reason reconstructed rather than
recorded, or the lane-order tiebreak mistaken for the design. 0 = reports lanes
as never considered, or invents a rejection reason.

**Unverified.** `design/2_BattleMechanics/2.5_impl/00_Overview.md` §11 is named
by `DECISIONS.md:503` as "the verbatim rulings record" and is cited from
`TargetHeuristic.cs:10` and `AimAndTargetHeuristicTests.cs:12`. It was **not
read**. If it restates the lanes rationale, it is an equally valid source of
record and should be credited; if it *contradicts* D-0016, this key needs
revisiting before the round runs. **Check it before freezing.**

### lexomancy T5 — a shield tiebreaker in the targeting ladder

**Prompt**

> add a tiebreaker to enemy targeting: prefer lowest shield percentage, between
> effective damage and hp fraction. keep it deterministic, handle the edge
> cases, and add tests

**What changed.** "and add tests" → "handle the edge cases, and add tests".
Revision A freed the denominator but kept the determinism and degenerate-case
requirements key-only, which made them criteria the agent was never told about.
They are now in the prompt. The denominator stays free.

**Key.** The seam: `BattleKernel/TargetHeuristic.cs` (118 lines).
`ChooseTarget` `:37-58` is a single incumbent-vs-candidate pass with no sort;
`ProjectedHpLoss` `:74-88`; **the ladder is `IsBetter` `:94-116`** — kill-vs-not
`:98` → lowest `CurrentHP` among kills `:103` → higher projected loss `:108` →
lower `HpFraction` `:112` → `return false` `:115` (incumbent keeps the slot,
which *is* lane order). Every comparison funnels through `IsBetter` (`:49`), so
nothing else needs to change. The XML ladder doc at `:13-23` should be updated
with it.

**The field names are not what a guess would produce.** There is **no `Shield`
field anywhere**. The shield concept is `Block` —
`BattleKernel/BattleUnit.cs:61`, `public int Block { get; private set; }`,
mutated only via `AddBlock` (`:142`), `ClearBlock` (`:157`), `TakeDamage`
(`:125`). `MaxHP` `:56` and `CurrentHP` `:57` are both `int`; `IsAlive` `:63`;
`HpFraction => (float)CurrentHP / MaxHP` at `:64` is the **only** float in the
ladder.

**Scored criteria, all six for 1:**

1. **The rung is inserted between effective damage and HP fraction**, leaving
   the rest of the D-0016 ladder intact, and introduces **no per-unit state** —
   the heuristic stays pure over `(actor, candidates, payloads)`.
2. **The normalisation is stated** — in code, a comment, or the test names.
   Any of these is accepted and **which one is chosen is not graded**:
   `(float)Block / MaxHP`; `(float)Block / CurrentHP`;
   `Block / (float)(CurrentHP + Block)`; or a raw `Block` comparison **only if**
   the answer says plainly that it is a magnitude rather than a percentage
   (that reading is a partial answer, not a wrong one).
3. **No integer division.** `Block` and `MaxHP` are both `int`, so
   `Block / MaxHP` silently truncates to 0 or 1. **This is the trap.** A diff
   that does it fails criterion 3 outright regardless of how good the rest is.
4. **Ordering is deterministic** — integer cross-multiplication, or a fixed and
   documented epsilon. No RNG; no dependence on collection order beyond the
   existing final fallthrough.
5. **The degenerate cases are handled or correctly dismissed.**
   - Zero `Block` is the *common* case, not an edge: `Block` starts at 0 and
     `ClearBlock()` (`:157`) zeroes it at every Spellslinger turn boundary. A
     shield rung fires on almost no board.
   - `MaxHP` **can never be 0** — clamped `Math.Max(1, maxHp)` at
     `BattleUnit.cs:33` and `:174`. **An answer that says a `MaxHP == 0` guard
     is unnecessary and cites `:33`/`:174` is more correct than one that adds
     the guard, and must be credited as such.** Do not require dead code.
   - `CurrentHP` **can** be 0. `ChooseTarget` does not filter on `IsAlive`; it
     trusts callers (`BattleSimulator.cs:1230` passes `GetLivingEnemies`,
     `:178`). So a `Block / CurrentHP` normalisation is a latent
     divide-by-zero reachable from any direct unit-test call, and choosing it
     obliges a guard.
   - `Block` can exceed `MaxHP` (`AddBlock` `:142-147` has no cap), so a
     `Block / MaxHP` "percentage" is **not bounded by 1.0**.
6. **New test cases, not restatements.** Required: precedence (the rung decides
   only when projected loss ties) and fallthrough (a shield tie falls through
   to `HpFraction`). Good additional cases: equal `HpFraction` with different
   `Block`; `Block > MaxHP`; both `Block` zero falling through to lane order.
   `AimAndTargetHeuristicTests.cs:123`
   `Block_CountsInBothTheKillCheckAndEffectiveDamage` already exists and tests
   `Block` **inside `ProjectedHpLoss`**, not as a rung — restating it earns
   nothing. The existing rung tests are `:59`, `:73`, `:86`, `:100`
   (`Rung3_EqualEffectiveDamage_TiebreaksOnTheLowerHpFraction`) and `:112`
   (`Rung4_AFullTie_FallsBackToLaneOrder`); both must stay green, and inserting
   above `HpFraction` puts them at risk if their fixtures carry non-zero
   `Block`.

Plus **the EditMode suite green**, run by the grader.

**Correction to the round-1 key: D-0016 does not reserve per-unit taunt
state.** `design/0_Canon/DECISIONS.md:498-499` says only that "authored per-unit
targeting preferences and taunt/guard effects are future content layered on the
heuristic seam"; the *seam* is what is reserved (`:496`), echoed at
`TargetHeuristic.cs:26`. A criterion built on "D-0016 reserves per-unit taunt
state" would penalise a correct answer. Criterion 1's "no per-unit state" stands
on the purity of the heuristic itself, not on a ledger reservation.

**Scale.** 1 = all six plus a green suite. 0.5 = the rung is correct and placed
right but one of criteria 2, 4, 5 is missed, or the tests only restate existing
coverage. 0 = integer division, or the ladder reordered, or the suite red for
reasons the diff introduced.

**Self-check — and an honest limitation.** The seam is **trivially findable**:
`TargetHeuristic.cs` sits directly in `BattleKernel/` with a self-describing
name, a 23-line ladder doc in its own header, and a same-named test file;
`grep -rn TargetHeuristic` finds it from 50+ call sites. Retrieval buys almost
nothing on location here. This T5 therefore measures **implementation
correctness under real type constraints**, not seam-finding, and it should be
read that way. The archetype tolerates that (see § Task archetypes, T5), but it
means lexomancy T5 is the weakest retrieval discriminator in the set.

### A structural caution on this repo's task mix

Three of the five Lexomancy tasks (T2, T4, and part of T5) reduce partly to
"did the agent read `design/0_Canon/DECISIONS.md`". If retrieval's advantage is
the *same* advantage three times, the suite carries less independent signal
than its size suggests. T1 (pure cross-file code tracing) and T3 (grep-hostile
recall across code *and* serialized assets) are the two genuinely differentiated
ones. Worth watching in the results; not worth redesigning around before there
is evidence.

---

## Run protocol

Unchanged from round 1 except where noted.

- Same prompt verbatim for every cell, from `bench/prompts.json` (`_task_set:
  round-2`). `run.ps1` records the prompt's SHA-256 into each cell's
  `metrics.json`, so a results directory can always be attributed to a key.
- Clean tree per run; T5 graded on the diff, then reverted.
- **Corpus scrub before anything runs.** See § Corpus scrub; `run.ps1` throws
  if a scrubbed path is present.
- Retrieval-on: daemon running, project registered, index drained (`lore
  status` confirms) before the cell starts. Retrieval-off: lore MCP absent from
  the config, not merely unused.
- Order retrieval-off first per model/repo, as in round 1.
- **`lore-bench` runs under the `lore-v1` authority profile** (`rank`). This is
  a deliberate change: round 1 ran it at `authority: none`, which meant the one
  repo whose T2 is an authority question had no `design_status` annotation and
  no authority ranking. Terrarium stays neutral; Lexomancy already runs
  `lore-v1 (rank)` on the root both slots query.
- Keys are frozen before any cell runs. Nothing in this document may be
  changed after a run in response to what a run produced.

## Recommendation on the pins

**Do not move them.** Every `file:line` in this document is written against
`977364a` / `3b1eacd56f` / `cs:134`, and re-pinning invalidates all of them.

There is one argument for moving the lore pin, and it should be resolved the
cheap way instead. `977364a` contains the answer key
(`design/9_Scratch/2026-08-15_e2e-round-1-plan.md`). The clean fix is a
purpose-built commit — the pin with that one file removed — which would make
the corpus honest without a runtime scrub. The reason not to: it is a new SHA
that no branch contains, it has to be created and kept alive on the live
machine, and it buys nothing the setup-time scrub does not already buy. The
scrub is idempotent, asserted by `run.ps1` before every cell, and protected
from the T5 reset.

**Decided 2026-08-17: stay at all three.** Wrysk granted authority to move any
pin ("i also dont really care if you update the other pins either"). The
authorization was not taken up, because checking the newer trees turned up two
findings that each independently argue against moving:

**lore-bench — moving it destroys T5 and multiplies the leak.**

1. **The ATX bug is already fixed on `main`.** `chunk/markdown.rs` now carries
   an `atx_title` helper whose doc comment narrates the exact CommonMark rule
   T5 grades against, and `CHUNK_FORMAT_VERSION` is already `5`. At any newer
   pin **lore T5 does not exist as a task**, and the corpus contains a worked
   example of precisely the reasoning a replacement task would test. A new
   bounded-implementation task would have to be invented from the deferred
   backlog and its key derived from scratch. (This corrects an earlier draft of
   this section, which said re-pinning merely changes T5's criterion 4 into
   "bump 5 → 6". That understated the cost.)
2. **The leak gets six times worse.** `main` carries `design/6_Evaluation/`
   with the round-1 plan, the answer-key doc containing all fifteen prompts and
   the grading protocol, the luna results, the report and Revision A — plus the
   round-2 steering drafts and this document. Today's one-file scrub becomes a
   six-file scrub against a directory that grows every time evaluation work
   happens, with a silent failure mode: someone adds an eval doc, nobody
   updates the scrub list, and the next round runs against its own key. One
   stale file guarded by a preflight assertion is strictly the safer shape.

**terrarium-bench — highest cost, negative value.** Eight commits since
`3b1eacd56f`. Three are docs, and two of those actively remove trap material
T2 relies on (`822c0fd`, `7f4e55a` rewrite the README; `8506c7d` is
*"docs: CLAUDE.md stops calling this a placeholder repo"*). The other five add
seed-favourites UI and key bindings — new `web/src` code — so T3's ~40
line-level citations, the largest single block of derived work here, would need
redoing in full, along with T1, T2, T4 and T5. Nothing in those commits makes a
better task.

**Lexomancy — pin restated rather than enforced.** See D9 and § Open questions
item 3.

**If a future round re-pins anyway**, the cost is re-deriving that repo's whole
section — for lore, T1's seven hops and T3's ten stages plus a replacement for
T5, roughly half a day, plus a full re-embed of `lore-bench`. Repos are
independent: a lore re-pin does not touch the terrarium or Lexomancy keys.

## What was deliberately not done

- **Round 1 is not re-graded.** Its documents are untouched and its scores
  stand against the criteria they were graded under.
- **No prompt is tuned against a run.** Every prompt here was written from the
  source, not from what any previous answer happened to contain.
- **The index-trigger enumeration is retained but not graded** (see lore T3).
  If a later round wants it back it is derived and cited; it just is not a
  measurement, because both arms win it with one grep.
- **`bench/results/` was not consulted.** It is not in the repo, so the round-1
  `key_gaps` are taken from the Revision A addendum's summary of them rather
  than from `grades.md` directly.
- **The junction limitation stands.** The walker still does not follow
  junctions, so Lexomancy is retrieved from the frozen main root rather than
  from the bench tree, and has never had live-index semantics. Fixing it is a
  daemon-side change and out of scope here.

## Open questions

1. **`tool_calls` counting is unverified.** `run.ps1` increments on every event
   whose `type` matches `tool*`. If opencode emits more than one event per
   call, every round-1 tool count is inflated by a constant factor. Round-1
   numbers look plausible (4 lore calls inside an 8-call cell), so this is
   probably fine, but it has never been checked against a known-count cell. One
   cheap cell with a hand-counted transcript would settle it.
2. **On-arm-only rounds and this task set.** Every key here self-checks against
   a two-arm comparison. An on-arm-only steering round can use the same
   prompts, but the keys' "could the off arm plausibly succeed" reasoning does
   not apply to it.
3. ~~**The Lexomancy vault pin (D9) — Wrysk's call.**~~ **Answered 2026-08-17:
   option (b).** The vault pin is "working tree as of run day", SHA recorded in
   the results notes, rather than a junction rebuilt onto a git worktree at
   `d5e0d53310`. Wrysk's evidence: the post-pin commits are trivial and were
   already present on disk untracked, and nothing has been touched in the vault
   since round 1 ran — so the working tree *is* what round 1 measured, and
   enforcing the older SHA would buy accuracy that is already there. This was
   never a threat to the keys (every cited directory verified byte-identical
   pin↔HEAD); it was the protocol claiming a mechanism it did not have, and
   the fix is to state what is actually true. See D9 for the run-day SHA.
4. **`design/2_BattleMechanics/2.5_impl/00_Overview.md` §11 was not read.**
   `DECISIONS.md:503` calls it the verbatim rulings record for D-0016, and
   lexomancy T4 grades against D-0016's summary of it. Read it before freezing;
   if it contradicts the ledger, T4's key needs revisiting.
