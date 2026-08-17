---
design_status: exploration
last_reviewed: 2026-08-17
decision_refs:
  - D-0009
---

# E2E answer keys — Revision A (2026-08-17)

> **Superseded for round 2 by [[2026-08-17_e2e-round-2-task-set]]** (same day,
> later). Every ruling below was decided under a constraint Wrysk has since
> revoked — that round 2 must stay comparable to round 1 and prompts must not
> change. Where this file says a ruling "wins for round 2 and later", read
> "wins for round 1's record only". Nothing else here is edited; it remains
> the account of how round 1's logged key gaps were resolved at the time.

Addendum to [[2026-08-15_e2e-round-1-plan]] (which holds the per-repo grading
keys) and [[2026-08-15_e2e-round-1-answer-key]] (prompts, pins, protocol).

**Nothing in the round-1 documents is rewritten.** Both keep their graded text
verbatim so round-1 scores stay readable against the criteria they were
actually graded under. This file is the delta: every ruling below **replaces**
the corresponding round-1 criterion for round 2 and later, and says what it
does to the round-1 scores.

## Why this exists

The 2026-08-16 blind grading pass logged `key_gaps` — places where a grader
found the *answer key* ambiguous, incomplete or misleading, as distinct from
anything wrong with a run. They are summarised at the end of
`bench/results/grades.md` (§ "2026-08-16 grading pass") and repeated in
`design/99_Scratch/2026-08-16_round-2-steering-drafts.md` § "If/when a round 2
runs". This addendum resolves each one.

## The arm-neutrality rule these rulings are held to

A key may not reward or punish anything that correlates with *how* an answer
was obtained. Concretely, for every ruling below:

- it is stated in terms of **claims and artifacts**, never phrasing, formatting
  or citation style that one retrieval path happens to produce;
- where round 1 showed one arm doing something the other did not, the ruling
  either scores that thing for **both** arms on the same terms or scores it for
  **neither** — it never quietly canonises the winner's behaviour;
- no ruling changes what a task **asks**. The frozen prompts in
  `bench/prompts.json` and the answer-key doc are **unchanged**; changing a
  prompt would break comparability with the 60 round-1 cells.

Where a gap actually indicts a *prompt* rather than a key, it is recorded in
§ "Open items for Wrysk" and left undone.

---

## KG-1 — lore T3: the prompt asks half of what the key grades

**Gap as logged:** "lore-T3 prompt only asked half the key bullet."

**Traced to:** plan doc, § lore, T3 bullet —

> T3: List every code path that can trigger an index recompute, **and every
> caller that consumes `design_status` from frontmatter.**

The frozen prompt is only the first half:

> what are all the ways an index pass can get triggered? list every code path

**Ruling.** The `design_status`-consumer clause is **struck from the T3 key.**
T3 grades the index-trigger enumeration and nothing else.

**Why arm-neutral.** It removes a criterion that *no* run of either arm could
have satisfied, because nothing in the prompt asked for it. Round-1 graders
already ignored it (all four lore-T3 cells scored 1 in both passes without it),
so no round-1 score moves.

**Not done:** the prompt is not extended to cover the dropped half. See
§ "Open items for Wrysk", item 1.

### T3 completeness reference (new)

The round-1 key was a structural description, so graders recorded that
"completeness beyond internal consistency is unverified"
(`grades.md` § 1, luna-lore-off-T3). The following checklist is provided so a
round-2 grader has something to grade recall against.

Trigger categories at pin `977364a` — full-scan producers:

1. daemon startup seeding every registered project
2. project registration (`lore add` → `POST /v1/projects`)
3. explicit `lore index [project]` (→ `POST /v1/index`; no argument = all
   projects)
4. watcher event-channel overflow
5. filesystem watcher `need_rescan()`
6. `.gitignore` change inside a project
7. watcher backend error (affected projects, else all watched)
8. pending-path storm — `request_paths` beyond `MAX_PENDING_PATHS` promotes to
   a full scan

Path-scoped producer:

9. ordinary debounced filesystem events routed to `request_paths`

Correct non-triggers, worth credit but not required: `lore-mcp` cannot
register or force a reindex; `search`/`expand`/`status` enqueue nothing;
watch-arm success or retry enqueues nothing; embedding-worker ticks are not
index passes; startup registry reconciliation does not itself enqueue.
Test-only direct calls to the scan functions are also correct colour, not a
requirement.

**Provenance and how to use it.** This list is transcribed from the round-1
**retrieval-off** answer (`bench/results/20260815-154136-luna-lore-off-T3/`),
which both grading passes scored 1 and which the on arm independently matched.
It is deliberately taken from the *off* arm so it cannot encode anything the
index made easy. It is **not** parent-verified against the pin line by line.
Therefore:

- Grade recall against the nine numbered producers, at **category** level, not
  by symbol name or wording.
- An answer that names a genuine trigger absent from this list is **credited,
  not penalised** — the list is a floor, not a closed set.
- 1 = all nine categories, correctly split between full and path-scoped.
  0.5 = one or two categories missing, or the full/incremental split confused.
  0 = the execution funnel itself is wrong.

## KG-2 — lore T5: is the `CHUNK_FORMAT_VERSION` bump required?

**Gap as logged:** "lore-T5 CHUNK_FORMAT_VERSION bump unspecified."

**Traced to:** plan doc, § lore, T5 bullet (the ATX trailing-`#` fix). The key
says "Fix + regression test" and is silent on persisted state. In round 1 the
retrieval-on cell additionally bumped `CHUNK_FORMAT_VERSION` 4 → 5 so already-
persisted chunks re-chunk; the off cell did not. Graders had no rule and the
grades doc wrote it up as an on-arm quality win (`grades.md` § 3).

**Ruling.** The bump is **out of scope for scoring: neither required nor
rewarded, and its absence is not penalised.** A grader must not add or subtract
score for it. If present, note it in the rationale text only.

Scored criteria for lore T5, in full:

- the closing-sequence rule is implemented per CommonMark — a run of `#` at the
  end of an ATX heading closes it only when preceded by a space (or when the
  line is nothing but `#`s), and may be followed only by spaces/tabs;
- `# Learning C#` keeps its trailing `#` in the resulting heading path;
- a real closing sequence (`## Foo ##`) is still trimmed — the fix must not be
  "stop trimming";
- a regression test covers **both** of the two cases above;
- `cargo test --workspace` green at the pin.

**Why arm-neutral.** This is the one gap where *both* available rulings are
non-neutral if adopted silently. Making the bump **required** would retro-fit a
criterion onto exactly the behaviour the on arm exhibited and the off arm did
not — the textbook way to flatter an arm. Making it **forbidden** would punish
the same behaviour. The neutral resolution is to score only what the prompt
asks for ("fix that per commonmark rules and add a test" — it says nothing
about persisted-state migration) and to make the silence explicit rather than
leave it to grader taste.

**Effect on round 1:** none. Both lore-T5 cells scored 1 in both passes; the
bump was cited as colour in the synthesis, not as a scoring input.

**Pin note:** at `977364a` the constant is `4`. It is `5` on current `main` for
unrelated ingestion work. Grade against the pin, not against `main`.

## KG-3 — lexomancy T5: "shield percentage" of what?

**Gap as logged:** "lexomancy-T5 shield normalization (Block/MaxHP)
unspecified."

**Traced to:** plan doc, § Lexomancy, T5 bullet ("e.g. lowest shield
percentage") and the frozen prompt ("prefer lowest shield percentage, between
effective damage and hp fraction"). Neither names a denominator, and round-1
answers picked one on their own.

**Ruling.** The denominator is **free**. Any of `Block / MaxHP`,
`Block / (Block + CurrentHP)`, `Block / CurrentHP`, or another stated ratio
earns full credit, provided all of:

1. the chosen normalisation is **stated** — in the code, a comment, or the
   test names — rather than left implicit;
2. ordering is **deterministic**: integer cross-multiplication, or a fixed and
   documented comparison epsilon. No RNG, no dependence on collection order
   beyond the key's final lane-order tiebreak;
3. the degenerate cases are defined — zero shield, and a zero denominator;
4. the new rung sits **strictly between** effective damage and HP fraction,
   leaving the D-0016 ladder otherwise intact (kill-secure → effective damage →
   *new rung* → HP fraction → lane order), and introduces no per-unit taunt
   state (D-0016 reserves that);
5. tests pin both the **precedence** (the new rung decides only when effective
   damage ties) and the **fallthrough** (a shield tie falls through to HP
   fraction).

Grade the diff on 1–5. Do **not** grade on which denominator was chosen.

**Why arm-neutral.** It widens the acceptable-answer set identically for both
arms; the denominator is not something retrieval can tell you, since no
document in the repo specifies one.

**Effect on round 1:** none. luna-lexomancy-off-T5's 0.5 was for a polluted
diff (a stray generated `.slnx` hunk) and a missing suite result — not for its
`Block/MaxHP` choice, which this ruling explicitly accepts.

## KG-4 — the two T4 keys: does a code citation satisfy a prose-source key?

**Gap as logged:** "the two T4 keys should say whether code citations satisfy
the prose-source requirement (both arms took symmetric 0.5s on the strict
reading)."

**Traced to:** plan doc § "Task archetypes", T4 —

> **T4 — The "why" question.** … Answer lives in prose (decision rationale,
> handoff, review report), not code. Graded on reaching the right document and
> reproducing the actual rationale.

— plus the three T4 key bullets. The archetype is a **conjunction** (reach the
document *and* reproduce the rationale) but the per-task bullets read like a
rationale description alone, so graders split.

**Ruling — this restates the pre-registered archetype; it does not move it.**
T4 is scored on two conjuncts:

- **Content:** the answer reproduces the *actually recorded* rationale, not a
  plausible reconstruction.
- **Source:** the answer cites at least one **prose source of record** for that
  rationale.

Scale: 1 = both. 0.5 = correct rationale, but only code/test citations, or the
rationale is materially incomplete. 0 = wrong or invented rationale.

What counts as a prose source of record — any one is sufficient:

- a decision-ledger entry, by ID or by file (e.g. Lexomancy `D-0016`);
- a decision brief, design note, handoff, roadmap, plan-doc revision, or review
  report;
- **the commit message** that records the rationale (e.g. lore `60b3599`);
- a session/work report in the vault.

Source of record per task, so graders need not adjudicate:

| Task | Prose source(s) of record | Content the answer must reach |
| --- | --- | --- |
| lore T4 | commit `60b3599` message · the package-3 design note · the session reports | the key-exchange convergence argument for applying the whole project set atomically |
| lexomancy T4 | Lexomancy ledger `D-0016` | the full ranked ladder **and** the recorded `enemies[0]`-defaulted-onto-the-Lexomancer motivation, plus lanes-rejected-as-complexity |
| terrarium T4 | `docs/plan.md` Revision 3 and Revision 4 | simplex preset "rejected by the user" → seeded random projection; raw-embedding modulation "reacts to everything and isolates nothing" |

**Why arm-neutral — stated plainly, because this one is not obvious.** A
source-of-record conjunct does structurally favour retrieval, since prose
retrieval is precisely what lore sells. That is not a defect introduced here:
it is the pre-registered design of the T4 archetype, frozen in the plan doc
before any cell ran, and it is the reading the round-1 graders applied. The
neutral act at this point is to **state the frozen reading precisely**, not to
loosen or harden it after seeing which arm it cost. Loosening it now (accepting
code-only citations) would be changing the experiment's design in response to
results, which is worse than the ambiguity it fixes.

**Effect on round 1:** none, and the direction of any future correction is
harmless — round 1's T4 penalties were **symmetric across arms** (lore T4 and
lexomancy T4 both 0.5/0.5 on qwen; terrarium T4 luna-off 0.5 on the re-run),
so promoting or keeping them cannot move the on-vs-off comparison. Whether to
promote them is Wrysk's call, not this addendum's — see § "Open items", item 2.

## KG-5 — lore T1: merge point named structurally, not by symbol

Not a logged `key_gap`, but the same grading pass recorded that "lore T1/T3
keys are structural descriptions rather than enumerated lists, so those four
1-scores rest partly on cross-arm agreement" (`grades.md` § 3, caveat 5). KG-1
covers T3. For T1 the hop list *is* enumerated; only the merge point is
described rather than named.

**Ruling.** The FTS/vector merge point is `fuse_detailed` in
`crates/lore/src/daemon/search.rs`. Naming the function, or describing the RRF
fusion at that seam unambiguously, both satisfy the key's "state where FTS and
vector candidates are merged".

Provenance: taken from the round-1 **retrieval-off** answer, which both passes
scored 1; not independently parent-verified at the pin. Credit an answer that
names a different but demonstrably correct seam.

---

## Open items for Wrysk (deliberately not acted on)

1. **lore T3's prompt covers half its intended task.** Repairing this needs a
   *prompt* change, which would break comparability with all four round-1
   lore-T3 cells. Options: (a) leave T3 as it stands (this addendum's default),
   (b) add the `design_status`-consumer sweep as a **new** task id in a later
   round, keeping T3 untouched. Not decided here.
2. **Whether the symmetric T4 0.5s promote to 1.** Already flagged in the
   round-1 report's closing "Remaining for Wrysk". KG-4 pins the *criterion*;
   it does not re-grade round 1.
3. **`lore-bench` indexes with no authority profile.** `lore status` on
   2026-08-17 shows `lore-bench` and `terrarium-bench` as
   `authority: none (no .lore.toml profile)`; only the `Lexomancy` root (which
   the Lexomancy arm queries) runs `lore-v1 (rank)`. At pin `977364a` the lore
   repo had no committed `.lore.toml`, so `lore add` wrote a `[project]`-only
   file. Consequence: for the lore repo, round 1's T2 "authority / modality"
   task ran against a **neutrally indexed** project — the on arm got no
   `design_status` annotation and no authority ranking there. This is a
   round-1 validity fact, not a key gap, and it is *not* corrected here:
   switching the bench worktrees to `lore-v1` for round 2 would strengthen the
   on arm relative to round 1 and make the two rounds incomparable. Wrysk's
   call whether a later round enables it deliberately as its own variable.
