---
name: lore-vault
description: Set up a lore-v1 design vault in a repository — the .lore.toml authority profile, the 0_Canon decision ledger, and the agent instructions that keep it honest. Use when a repo should start recording decisions Lore can rank by, when `lore status` says a project declares no authority profile, when someone asks what `design_status` or a D-NNNN entry means or how to write one, or when the user says "set up a design vault", "adopt lore-v1", or "/lore-vault".
---

# Setting up a `lore-v1` design vault

Lore indexes every registered repo. It *interprets* one only when the repo
commits a `.lore.toml` naming an authority profile (D-0012). Without that file a
repo gets neutral retrieval: no `design_status` parsing, no ledger, no path
ceilings, no authority metadata on results. Nothing is wrong with that. But it
means a document that says `design_status: decided` reads as ordinary prose —
and so does one that is lying.

`lore-v1` is the profile that changes it. It buys three things:

- **A decision ledger** — an append-only record of what was actually decided,
  which Lore parses into an *active* set and pins above every other document.
- **Validated claims** — a document declaring `decided` ranks as decided only if
  it cites a decision that is still active. Uncited `decided` drops to neutral.
  Authority laundering stops being free.
- **Path ceilings** — research is capped as evidence, scratch as scratch, no
  matter how confident the prose sounds.

Your job: read the repo, turn the profile on, scaffold the minimum canon, and
propose the agent instructions. Then verify and report.

## What is load-bearing, and what is decoration

This is the part nobody can infer, so settle it before creating anything.

**Lore reads these names. Rename them and the semantics silently stop:**

| Path | What it means |
| --- | --- |
| `**/0_Canon/DECISIONS.md` | the mono ledger; pinned above all other documents |
| `**/0_Canon/decisions/D-NNNN-<slug>.md` | one decision per file, same grammar (D-0013) |
| `**/7_Research/**` | capped at evidence — never outranks a decision |
| `**/99_Scratch/**` | capped at the floor |

**Free — use whatever the repo already does:**

- The parent directory. `design/`, `docs/`, `Docs/`, `notes/` all work; Lore
  matches `0_Canon` at any depth.
- Every other folder. `1_Architecture/`, `2_Memory/` and friends are one
  project's subject headings, not a schema. Do not scaffold someone else's table
  of contents into a repo that has its own.

**One more constraint:** a project gets **one** `0_Canon` directory. Two is a
namespace hazard Lore reports as a violation, not a feature.

## 1. Ground yourself before writing anything

Read the repo first. You are adding to a house that already exists.

- Does it keep design docs already? Find where. That directory is your parent —
  do not create a sibling `design/` next to a live `docs/`.
- Does it record decisions already? ADRs under `docs/adr/`, a changelog of
  rationale, a decisions section in the README. **Say so and stop before
  converting anything.** Migrating an existing record is the user's call and a
  different job from starting one.
- Read `CLAUDE.md` / `AGENTS.md` / `CONTRIBUTING.md`. You will propose an edit to
  the first two in §5, and it has to sound like the rest of the file.
- Check `lore status` for the project. If it already declares a profile there is
  nothing to turn on — go straight to whatever the user actually asked for.

## 2. Turn the profile on

Write `.lore.toml` at the **registered repo root** — nested copies are ordinary
files, not configuration. Commit it; it follows the repo.

```toml
# Lore repo-side configuration (D-0012). Committed; follows the repo.
[authority]
profile = "lore-v1"
behavior = "annotate"
```

`behavior` is the one real choice, so present it rather than picking silently:

- **`annotate`** (the default when the key is absent) — compute and expose
  authority metadata, leave result ordering alone. Start here. Annotation's
  value is well evidenced; reranking's is less so.
- **`rank`** — also apply the authority weights to ordering. Worth it in a vault
  with a populated ledger people actually trust.
- **`off`** — declared but suspended. Indexes exactly like an unconfigured repo
  while staying visible in `lore status`, which is the point.

An unknown profile or behavior is a *visible* error, not a silent fallback: the
repo keeps indexing neutrally and `lore status` shows the problem. If the repo
already has a `[project]`, `[[sources]]` or `[plugins]` table, add `[authority]`
alongside it — the tables are independent and none implies another.

Adding this file re-chunks the repo's Markdown, because the same bytes chunk
differently under a profile. Expect the next index pass to do real work.

## 3. Scaffold the minimum canon

Two files. Resist writing more.

**`<parent>/0_Canon/DECISIONS.md`** — the ledger. Append-only, newest entries at
the bottom. Seed it with the entry that adopts the model, so the file is not
empty and demonstrates its own format:

```markdown
---
design_status: decided
last_reviewed: <today>
---

# <Project> Decision Ledger

Append-only. Newest entries at the bottom. Schema per [[README]].

## D-0001 — Adopt the lore-v1 authority model

- **Date:** <today>
- **Status:** Accepted
- **Scope:** All design documentation and planning work in this repo
- **Decided by:** <who authorized it>
- **Decision:** This vault is tentative by default. Documents become binding
  only through an accepted entry in this ledger.
- **Rationale:** <why this repo wants it>
- **Consequences:** Agents consult this ledger before treating any document as
  binding; promotion requires explicit authorization.
- **Supersedes:** None
- **Canonical sources:** [[README]]
```

**`<parent>/0_Canon/README.md`** — the constitution. State the authority order
(current instruction > active accepted entries > documents those entries name >
`decided` briefs within their declared scope > everything else), the four
`design_status` values, and the promotion rules. Adapt the wording to the repo;
keep the substance. Mark it `design_status: decided` and cite D-0001.

Do not create empty numbered folders. Documents arrive with their own shape, and
a tree of stubs just makes a vault look abandoned.

## 4. The entry grammar — parsed vs. convention

Teach this accurately, because most of it is convention and two fields are not.

**Lore parses exactly this:**

- The heading `## D-NNNN — Title`. The id is `D-` plus **exactly four digits**;
  `D-004` and `D-00041` are not ids. In a per-file record the *filename* is
  authoritative, and a heading that disagrees is a reported violation.
- `**Status:**` — an entry is active only when it reads `Accepted`.
- `**Supersedes:**` — retires other entries only when the value is a bare id list
  (`D-0004`, `D-0002 and D-0003`). **Any non-id word makes it partial and it
  retires nothing** — `None (extends D-0015)` and `D-0002's caching clause only`
  both leave the target active, which is correct: the target's surviving clauses
  are still canon.

Only an *accepted* entry can supersede. That restriction is load-bearing: agents
are invited to draft proposed entries, and a `Proposed` entry naming
`Supersedes: D-0001` must not be able to deactivate live canon.

**Everything else — `Date`, `Scope`, `Decided by`, `Decision`, `Rationale`,
`Consequences`, `Canonical sources`, and `last_reviewed` in frontmatter — is for
the humans and agents reading the file.** Lore does not parse it. Keep it anyway:
an entry without a rationale is how a decision gets re-litigated in six months.

**On ordinary documents**, two frontmatter keys are read: `design_status`
(`exploration` | `leaning` | `decided` | `deprecated`; absent means unclassified,
treated as exploration) and `decision_refs` (a list of ids). A `decided` document
citing no *active* decision is demoted to neutral — still searchable, it just
stops outranking things.

Supersession is a new entry, never a rewrite. Accepted entries are immutable.

## 5. Propose the agent instructions — do not apply them silently

**This is the step that makes the rest hold, and the one most likely to be
skipped.** A skill loads when its description matches. The rules that matter —
written does not mean canonical, preserve modality, never promote without
authorization — must be in force when an agent is halfway through editing a
design document, which is exactly when nothing will think to load a skill about
vault setup. They belong in the file the host always reads.

Draft a block for `CLAUDE.md` (and `AGENTS.md` if the repo keeps one), in that
file's existing voice, covering:

- Read `<parent>/0_Canon/README.md` and the relevant ledger entries before
  treating design material as binding.
- Written, polished, detailed, or implemented does not mean canonical. Only
  active accepted entries and their cited sources bind.
- Documents without `design_status` are unclassified exploration.
- Preserve modality when synthesizing — never turn a leaning, a proposal, an
  example, or a current implementation into a requirement.
- Do not add an accepted entry, mark a document `decided`, or supersede canon
  without the owner's explicit authorization for that specific choice.
- Anything under `7_Research/` is evidence, not decisions.

**Show it and get agreement before writing.** `CLAUDE.md` encodes a team's
conventions, and Lore's installer deliberately never touches it. You may, because
you have read the repo and can match its voice — but not silently. Append to the
existing file; never reflow or rewrite what is already there.

## 6. Verify, then report

```
lore index <project>
lore status --project <project>
```

`lore status` is the check: it should now name the profile and behavior and
report the decision count (`1/1 decisions active` for a fresh ledger). If it
reports a config error or a decision violation, fix it now — a vault that fails
quietly is worse than no vault.

Then confirm the semantics are live rather than merely configured:

```
lore search "<something in the ledger>" --status decided
```

Report to the user:

- **Files created**, and the parent directory you chose, with the reason.
- **The `behavior` you set** and why — say plainly that `rank` is available.
- **The `CLAUDE.md` block**, as a proposal, unless they already approved it.
- **Anything you found and did not touch** — an existing ADR directory, a second
  `0_Canon`, design docs already carrying `design_status`. Name it; do not
  quietly work around it.

The vault is the repo's now, not Lore's. Everything you wrote is a starting point
a human owns and edits.
