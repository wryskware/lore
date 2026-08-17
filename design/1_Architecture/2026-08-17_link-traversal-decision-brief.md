---
design_status: leaning
last_reviewed: 2026-08-17
---

# Link traversal and what the watcher can promise — decision brief

Proposes how Lore should treat symbolic links (and, on Windows, directory
junctions) during observation, and — the part that actually needs deciding —
what freshness the daemon may honestly claim for content reached through one.

Touches D-0020 (the ignore stack) and D-0015 (one filesystem observer per
machine); neither is amended by the proposal below. The traversal half was
argued to a conclusion in session on 2026-08-17; the coverage half is the open
question this brief exists to close.

## What prompted it

`Lexomancy-bench` is a workspace assembled from three directory junctions over
two other checkouts, plus three loose files. It indexed the three loose files.
Editing its `.loreignore` — including writing `!design/`, `!tools/` and
`!Lexomancy/` on the explicit expectation that those lines would pull the
corpus in — changed nothing, and could not have: **ignore rules are never
consulted for a tree the walk does not enter.** The walker sets
`follow_links(false)`, so the junctions were yielded as entries that were
neither files nor directories, and dropped.

Two separate defects sat inside that one symptom, and they have different
answers:

- **Silence.** The project reported `files: 3` and nothing else. In every
  number available it was indistinguishable from a project that genuinely had
  three files in it. *Fixed already* (`Walk::links`, `PassSummary::links_skipped`,
  and a warning naming the paths) — that fix stands whatever this brief
  concludes, because it is true under every option below.
- **Traversal.** Whether the content should be indexed at all, and if so how
  the user asks for it.

The backlog has carried the traversal half since round 1
([[../5_Implementation/2026-08-14_deferred-backlog]]), correctly noting that it
needs "a think about what the watcher can honestly promise across junction
targets on Windows."

## Platform ground truth

Established by test, not by reading:

- Windows sets the reparse-point attribute on a junction with a
  name-surrogate tag (`0xa0000003`), and Rust's `FileType::is_symlink()`
  answers **true** for it. Junctions and symlinks are therefore one case, not
  two, and no code needs to distinguish them.
- The `ignore` crate walks straight into a nested repository under either
  `require_git` setting but stops dead at a link under `follow_links(false)`.
- `walkdir` (2.5.0) carries ancestor-loop detection (`ErrorInner::Loop`) which
  activates only once `follow_links` is on. It does **not** deduplicate two
  sibling links to the same target — that is not a loop, and it would index
  the tree twice.
- `ReadDirectoryChangesW` does not traverse reparse points. A recursive watch
  on a project root reports nothing about changes inside a junction's target;
  those events belong to whoever watches the target's real path.
- The daemon has **no periodic rescan**. Full scans run at startup, on
  explicit `lore index`, on a policy-file edit (`.loreignore`, `.gitignore`,
  `.lore.toml`), on watcher overflow, and on registration. Nothing else.

That last point is what makes the coverage question load-bearing rather than
theoretical: a subtree with no watch coverage and no rescan cadence is stale
from daemon start until a human types a command, which on this machine has
meant days.

## Traversal: not followed by default, `!` is the override

> [!candidate]
> Links are a **descent boundary**, in the same category as the nested-repository
> boundary and not a fourth ignore rule source. The sole override is a `!`
> re-include in the project's own `.loreignore` — the identical escape hatch
> the repository boundary already uses. No `.lore.toml` key.

Why not-followed is the right default, beyond inertia:

1. **A link is an assertion about where bytes live, not about what is
   interesting.** Following by default lets a project silently absorb a tree it
   does not own. Concretely, on this machine, `Lexomancy-bench` would swallow
   the whole `Lexomancy` corpus — which is already project id 2. Same chunks in
   two projects, embedding cost paid twice, both copies competing in the
   rankings. That is precisely the harm the nested-repository boundary was
   added to prevent, arriving through a different door.
2. **Duplication is not fully solvable even with effort.** Ancestor loops come
   free with `follow_links(true)`; two sibling links to one target do not, and
   deduplicating by canonical target would be new machinery serving a rare
   shape.
3. **The watcher cannot honour it** (see below). A default that produces a
   silently stale index is worse than one that indexes nothing, because the
   second is visible.

Why the override belongs in `.loreignore` and not in `.lore.toml`:

- `.lore.toml` holds `[project]` and `[authority]` and deliberately no
  ingestion table — D-0020 deleted the one it had (`[ingest] allow_secret_paths`)
  on the grounds that exclusion policy is a line in a file the user can read at
  a precedence they can argue with. Putting traversal policy back there
  re-opens exactly that.
- The walker already frames descent as its own category, distinct from ignore
  rules, and already gives that category exactly one escape hatch: a `!`
  re-include in the project's own `.loreignore`, evaluated against a
  purpose-built matcher. A link boundary reuses it verbatim — no new rule
  system, which is what D-0020's working rule asks for.
- It makes the file the user already wrote correct as written. That is
  evidence about what the mechanism should be, not a coincidence.

Sovereignty is preserved in both directions: the project's own file, and only
it, may re-include. A machine-wide preference cannot know which of *this*
project's links is a deliberate corpus mount, which is the same reasoning the
repository boundary uses.

## Coverage: the question this brief is for

Assume a link is followed. Its content lives outside the project root, so the
root's recursive watch does not cover it. Four ways out.

### A — Do not watch; report the gap

`lore status` reports the project's coverage as degraded; freshness comes from
explicit `lore index`.

Cost: near zero. Honesty: complete. **Weakness: with no periodic rescan, "stale
until a human types a command" is a very weak promise** — and the user who
wrote `!corpus/` did so because they wanted the corpus searchable, not because
they wanted to remember to refresh it.

### B — Secondary watches on link targets

Arm an extra recursive watch per followed target and map events back through
the link.

Real work, and it is the watcher's most delicate code:

- The walk must report each followed link's resolved target, and publish that
  set to the pump after every full scan. `IndexContext` already carries a
  cross-task handle (`embed_notify`), so a `WatchSender` beside it is a shape
  the module already has — this does not break index.rs's synchronous,
  runtime-free testability.
- `Watches` becomes per-*watch* rather than per-*project*: one project may own
  a root plus N targets, each with its own arm/retry state. That is a genuine
  refactor of the retry and status machinery, not an addition to it.
- `Watches::containing` needs a reverse map — for each followed
  `(link_rel, target_abs)`, an event under `target_abs` becomes
  `link_rel / relative_to(target_abs, event)`.
- Aliasing hazards are real: a target that *contains* the project root gets
  every event routed twice (harmless — the queue is a set), and
  `notify-debouncer-full`'s file-id cache watching two paths that alias one
  directory is unspiked behaviour.

Cost: O(changes), like the root watch. Complexity: the highest of the four,
concentrated in the component with the worst failure mode (a watcher that
drops events fails silently).

### C — Refuse, and point at the other project

Notice that a link resolves into an already-registered project and tell the
user to search that project instead.

This is what the bench does by hand today — both Lexomancy slots retrieve from
the main `Lexomancy` project precisely because the junctions are not walked.
Zero machinery, and arguably the *correct* answer whenever the target is
already indexed. But it does nothing for a link to a tree that is not a
registered project, which is the general case.

### D — Periodic rescan, for link-bearing projects only  ← proposed

A followed subtree is **not live-watched**. Instead, a project that follows at
least one link is rescanned on an interval (config key; ~15 min default).
Everything *not* behind a link keeps full live coverage from the root watch, as
today.

Why this is the recommendation:

- **It sidesteps the platform problem entirely** rather than fighting it. No
  reverse mapping, no per-watch retry state, no debouncer aliasing, nothing
  Windows-specific. It behaves identically on every platform.
- **The cost is already designed for.** A rescan of an unchanged tree writes
  nothing: "a file whose manifest hash matches the stored one is never even
  read by the pipeline and touches no store state — in particular it does not
  rewrite `indexed_at`, so re-scans are genuinely free rather than merely
  fast." A tick costs one hash per file and no store traffic.
- **It matches what followed content actually is.** A link is opt-in, and the
  trees people mount that way are corpora — a design vault, a vendored
  reference, a word list. They are read, not edited. Freshness in seconds buys
  nothing there; presence buys everything. Where the target *is* a working
  tree, it is almost always a registered project already, whose own root watch
  covers it live (option C's observation, kept as guidance rather than
  enforcement).
- **It is ~30 lines and reversible.** A ticker that requests a full scan for
  projects with followed links. If someone eventually mounts a 200k-file tree
  and the ticks hurt, *that* is when option B earns its way in — paid for by a
  real complaint rather than by symmetry.

Accepted trade, stated plainly: content behind a link can be up to one interval
stale, and `lore status` must say so rather than reporting the project as
simply `armed`. A coverage state that describes "live at the root, interval
elsewhere" is part of this option, not a follow-up to it.

> [!open]
> Whether the interval is a global config key or per-project. Global is
> proposed (one knob, and the shape is a property of the deployment rather than
> of the repo); per-project would want a `.lore.toml` table, which the traversal
> half of this brief has just argued against.

## Draft ledger entry

Left **unpromoted** per [[README]]: the traversal half was settled in session,
but the coverage recommendation above is new and has not been put to Wrysk.
Appending it accepted would attribute a choice that was never made.

> **D-0021 — Links are a descent boundary; `!` re-include is the only override**
>
> - **Date:** 2026-08-17
> - **Status:** Proposed
> - **Scope:** Link traversal during observation, and the freshness the daemon
>   claims for content reached through a followed link. Extends D-0020's
>   descent/rule-source distinction; amends no D-0020 or D-0015 clause.
> - **Decision:** Symbolic links are **not followed** by default and are
>   **reported** rather than silently dropped. A Windows directory junction is
>   a symbolic link for every purpose here. Following is opt-in per link, and
>   the **only** way to ask is a `!` re-include in the project's own
>   `.loreignore` — the same escape hatch the nested-repository boundary uses,
>   evaluated by the same matcher. There is **no `.lore.toml` key**: `.lore.toml`
>   holds `[project]` and `[authority]` and no ingestion table, and D-0020's
>   working rule stands (a new rule system needs a decision, not a code path).
>   A followed subtree is **not live-watched** — `ReadDirectoryChangesW` does
>   not traverse reparse points — and freshness for it comes from a **periodic
>   rescan of link-bearing projects** (global config key, ~15 min default)
>   rather than from secondary watches. `lore status` reports such a project's
>   coverage as live-at-the-root/interval-elsewhere, never plain `armed`.
> - **Rationale:** A link asserts where bytes live, not what is interesting;
>   following by default lets a project absorb a tree it does not own (here,
>   the whole `Lexomancy` corpus into `Lexomancy-bench`), duplicating chunks and
>   embedding cost — the harm the repository boundary exists to prevent,
>   through a different door. The override belongs to the sovereign file
>   because descent boundaries already have exactly that hatch and because it
>   makes the file users already write correct as written. Periodic rescan is
>   chosen over secondary watches because it sidesteps the platform limitation
>   instead of fighting it, costs a tree hash with no store writes on an
>   unchanged tree, and matches what mounted trees actually are — corpora, read
>   rather than edited. Secondary watches remain the named upgrade path if a
>   large churning mount ever makes the interval hurt.
> - **Consequences:** `walk_files` reports declined links and a pass counts them
>   (shipped ahead of this entry; true under every option considered). The
>   walker moves to `follow_links(true)` with a `path_is_symlink` prune, which
>   also buys `walkdir`'s ancestor-loop detection; two sibling links to one
>   target still duplicate that tree, accepted and documented. The backlog item
>   "the walker does not follow junctions/symlinks" is closed by this entry.
> - **Supersedes:** None (extends D-0020).
> - **Canonical sources:** [[1.2_Ingestion]]; `crates/lore/src/daemon/walk.rs`

## What this brief does not propose

- **No canonical-target deduplication.** Two links to one tree index it twice.
  Named so it is a known cost rather than a surprise.
- **No cross-project duplicate detection.** Following a link into an
  already-registered project duplicates that project's content. Guidance
  (search the other project instead) rather than enforcement — the daemon
  refusing a user's explicit `!` would be the layering D-0020 deleted.
- **No change to the hard floor.** `.git` stays pruned by name at any depth,
  through a link or not.
- **Nothing about `lore add` walking.** A second observer on the client side
  would contradict D-0015; registration already queues a full scan, so the
  daemon's own warning arrives seconds later and is the right place for it.
