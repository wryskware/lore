---
design_status: decided
last_reviewed: 2026-08-17
decision_refs:
  - D-0021
---

# Link traversal — decision brief

How Lore treats symbolic links, and on Windows directory junctions, during
observation. Decided as **D-0021** on 2026-08-17; the canonical design
statement is [[1.2_Ingestion]]. This brief records the argument, the platform
facts it rests on, and — at the bottom — the alternatives that were considered
and lost, so they are not re-proposed.

Extends D-0020's descent/rule-source distinction. Amends no D-0020 or D-0015
clause.

## The rule

> [!accepted] D-0021
> **Symlinks do not implicitly extend a Lore project.** Filesystem topology
> does not define project topology.

Concretely:

- **Link to a directory, inside the project:** not descended into.
- **Link to a file, inside the project:** the target is not indexed *through*
  the link either. Following it would put the same content under a second
  logical path, and a store keyed `(project, path)` would carry it twice.
- **Link whose target escapes the project root:** not followed, emphatically.
  A link must never be able to turn `~/code/game` into an index of `~/secrets`,
  a company mount, or an enormous vendored SDK.
- **A link being created, deleted, or retargeted:** the filesystem event is
  processed like any other, so indexed state that has gone stale is reconciled.
  What is *not* done is traversing the new target.
- **The project root may itself be a link.** This is the single exception, and
  it is not really one: the root is canonicalized at registration, so what the
  daemon walks, watches and stores containment against is the physical root.
  Windows junctions and reparse points take the identical semantic.
- **Declined links are reported, never silently dropped** — a walk names them,
  a pass counts them (`links_skipped`) and warns with the paths.

There is **no `follow_symlinks` option**, and there will not be one.

## Why this and not an opt-in

The first proposal in this brief was an opt-in: follow a link when the
project's own `.loreignore` re-includes it with `!`, reusing the escape hatch
the nested-repository boundary already has. That is rejected. The reasoning
that killed it is worth keeping, because the symmetry is genuinely tempting.

**A `!` re-include on a nested repository and a `!` re-include on a link are
not the same act.** A vendored repository's bytes are physically under the
project root; the only question is *whose content that is*, and the project
owner can legitimately answer "mine". A link's bytes are somewhere else
entirely, so re-including it does not reclassify content — it **extends the
project's extent to a different part of the filesystem**. One is a judgement
about ownership, the other is a change of scope, and they should not share a
mechanism just because they share a syntax.

`follow_symlinks = true` is worse still. It unleashes a small filesystem hydra
that has to be answered all at once: cycles, duplicate aliases (two sibling
links to one tree are not a cycle and `walkdir` will not catch them), escapes
outside the root, unclear `.loreignore` semantics for a path that exists at two
logical locations, physical-versus-logical event paths, and separately arming
watches on targets.

That last item was the whole of this brief's second half, and the reason it is
now three lines instead of four options: **when nothing is ever followed, the
watcher-coverage problem does not exist.** There is no subtree outside the root
to watch, so `ReadDirectoryChangesW`'s refusal to traverse reparse points stops
mattering. The nastiest sub-problem dissolved rather than being solved.

The rule is also the one already built next door. The nested-repository
boundary exists because the walk is *deliberate about whose content belongs to
the project* rather than walking whatever the filesystem happens to connect.
D-0021 is that same principle applied to the other way a filesystem connects
things.

## Platform ground truth

Established by test, not by reading:

- Windows sets the reparse-point attribute on a junction with a name-surrogate
  tag (`0xa0000003`), and Rust's `FileType::is_symlink()` answers **true** for
  it. Junctions and symlinks are one case, not two, and no code distinguishes
  them.
- The `ignore` crate walks straight into a nested repository under either
  `require_git` setting, but stops dead at a link under `follow_links(false)`.
- `walkdir` (2.5.0) carries ancestor-loop detection that activates only once
  `follow_links` is on — so it is not a reason to turn following on, and it
  would not have covered sibling aliases anyway.
- `ReadDirectoryChangesW` does not traverse reparse points. Moot under
  D-0021, recorded because it is the fact that would matter again if anyone
  reopened following.
- The daemon has **no periodic rescan**: full scans run at startup, on explicit
  `lore index`, on a policy-file edit, on watcher overflow, and on
  registration. Also moot under D-0021, and also recorded for the same reason.

## The split-brain case, specifically

The one implementation hazard that survives the rule, and the failure this
whole seam exists to prevent: **a watcher event whose path traverses a link
must not reach the incremental index when the full walker would reject it.**
The watcher is deliberately dumb and names paths no walk produced; if the
incremental path indexed `corpus/behind.md` because `std::fs::metadata`
resolved the link happily, the next full scan would delete it, forever.

The mechanism that prevents it is already the one D-0020 built: `observe_paths`
confirms every named file by listing its parent *through the same walker the
full scan uses*, and that listing is rooted at the project root, so it never
enters the link. A path behind a link is therefore absent from the
micro-manifest — and, being in scope, is **deleted** rather than left as an
orphan. That is also what reconciles a link that was just created over a
directory that used to be real content.

Pinned by test rather than left to inspection, at both the observation and the
pass level.

## What prompted it

`Lexomancy-bench` is a workspace assembled from three directory junctions over
two other checkouts, plus three loose files. It indexed the three loose files.
Editing its `.loreignore` — including writing `!design/`, `!tools/` and
`!Lexomancy/` on the explicit expectation that those lines would pull the
corpus in — changed nothing, and could not have: ignore rules are never
consulted for a tree the walk does not enter.

Two defects sat inside that symptom, with different answers:

- **Silence.** The project reported `files: 3` and nothing else, making it
  indistinguishable from a project that genuinely had three files. Fixed
  (`Walk::links`, `PassSummary::links_skipped`, a warning naming the paths).
- **Traversal.** Answered by D-0021: the junctions are correctly not walked,
  and the `!` lines are correctly inert.

**Consequence for the bench:** those three `!` lines are permanently inert and
should be deleted from that project's `.loreignore` — they read as intent that
will never take effect. The bench's existing arrangement (both Lexomancy slots
retrieve from the main `Lexomancy` project, which is registered in its own
right) is now *decided policy* rather than a limitation awaiting a daemon-side
fix, and `bench/README.md` is updated to say so.

## The future shape, if external content is ever wanted

> [!candidate]
> If a project should genuinely include a tree that lives elsewhere, the way to
> ask is an **explicit additional source root** — conceptually:
>
> ```toml
> [[sources]]
> path = "."
>
> [[sources]]
> path = "../shared-engine"
> mount = "shared-engine"
> ```
>
> — and never a blanket `follow_symlinks`. A declared mount is auditable, has
> one unambiguous logical path per file, can be watched deliberately, and does
> not depend on what the filesystem happens to be wired up to on this machine.

This is a **leaning about direction, not a design and not scheduled.** It is
recorded so that the next person who wants external content has somewhere
better to go than re-proposing following.

If an intermediate mode is ever wanted, the only acceptable one is
`follow_symlinks = "within-project"` — canonicalizing targets, requiring them
to stay under the canonical project root, deduplicating canonical directories,
detecting cycles, and separately ensuring watcher coverage. Explicitly **not
implemented now**, and explicitly never the unrestricted boolean.

## Rejected alternatives

- **`!` re-include as a follow opt-in.** Rejected above: it conflates a
  judgement about ownership with a change of project extent.
- **Secondary watches on link targets.** Would have armed an extra recursive
  watch per followed target and mapped events back through the link. Required
  refactoring `Watches` from per-project to per-watch, a reverse path map in
  `containing`, a `WatchSender` on `IndexContext`, and a spike on
  `notify-debouncer-full`'s file-id cache when two watched paths alias one
  directory — all in the component whose failure mode is silent. Moot: nothing
  is followed.
- **Periodic rescan of link-bearing projects.** Proposed as the cheaper way to
  give followed content freshness without touching the watcher. Moot for the
  same reason. (The observation that the daemon has *no* rescan cadence at all
  stands on its own and is unrelated to links.)
- **`follow_symlinks = true`.** Refused as a matter of policy, not cost.
- **A `.lore.toml` traversal key.** `.lore.toml` holds `[project]` and
  `[authority]` and deliberately no ingestion table — D-0020 deleted the one it
  had. D-0021 adds nothing there.

## What this brief does not propose

- **No change to the hard floor.** `.git` stays pruned by name at any depth.
- **No client-side walking in `lore add`.** A second observer would contradict
  D-0015; registration already queues a full scan, so the daemon's own warning
  arrives seconds later and is the right place for it.
- **No storage of the logical root alongside the physical one.** Canonicalizing
  at registration already gives the correct *semantics*; showing a user the
  path they typed rather than the resolved one is a display nicety, recorded
  here as available work rather than as part of the decision.
