---
design_status: exploration
last_reviewed: 2026-08-16
---

# How does a request name its project?

**Status: unclassified exploration.** Nothing here is decided. Written to
escalate one interface question; both positions are stated at their strongest
rather than balanced toward the current leaning.

## The question

Lore's query surface is machine-global. `search` with no `project` argument
spans every project registered on the daemon, and `status` enumerates every
registered project's name, root path and file counts to any client that asks.

Wrysk's call (2026-08-16): **scoping to exactly one project becomes a
requirement, not a default.** Cross-project queries are dropped for now and may
be revisited later.

That much is settled. The open question is narrower:

> **When a client issues a request, how does the project get identified?**

## Why it matters more than it looks

Three forces meet here.

1. **Isolation.** Today two people sharing one daemon, each with private repos,
   read each other's code with no barrier. This is the immediate motivation.
2. **Remote daemons.** Wrysk intends to offload Lore onto a shared dev box
   "sooner than later" (the provisioned Linux GPU box). The daemon stops being
   co-located with the user.
3. **D-0012's optionality.** `.lore.toml` is currently *opt-in*. A registered
   repo without one is a first-class citizen that gets neutral retrieval. Any
   scheme that makes the file load-bearing for scoping changes that status.

The resolution mechanism has to survive all three, and (2) is the one that
eliminates otherwise-attractive designs.

## Position A — the client sends path context; the daemon resolves it

The client sends its absolute working directory with each request. The daemon
tests that path against its registered roots (`paths::is_within`, already
implemented), longest match winning for nested roots, and scopes to the
containing project. A path inside no registered root is an error.

**The case for it**

- **Zero repo configuration.** Works on every registered repo immediately,
  including ones that never adopted `.lore.toml`. This preserves D-0012's
  optionality exactly as written — the file stays purely an authority opt-in
  and gains no second job.
- **One source of truth.** The registry already holds the authoritative root
  list. Identity cannot drift, because there is only one place it lives.
- **Collisions are impossible by construction.** Filesystem roots are unique.
  Two repos cannot both claim to be `lore`; two checkouts of the same repo are
  distinct projects automatically, which is exactly right for the bench
  worktrees.
- **Subdirectory calls are free.** Containment is a prefix test, so
  `lore search` from `crates/lore/src/store/` resolves with no file lookup and
  no walk-up.
- **Nothing to migrate.** No new config key, no `lore init` change, no
  re-registration.

**The case against it**

- **It assumes co-location, and that assumption is being retired.** A remote
  daemon's registered roots are paths on *its* filesystem. `C:\Users\perag\...`
  means nothing to a Linux daemon, and two clients could send the same path
  meaning different repos. This is not a rough edge; the mechanism's core
  operation stops being well-defined.
- **It puts filesystem paths on the wire.** Directory structure and usernames
  leak to a daemon that, in the shared-box future, is not the user's own
  machine.
- **It does not travel.** The same repo checked out on two machines at
  different paths is two unrelated identities, so nothing about a project's
  identity follows it across machines or contributors.
- **Trust is unresolved anyway.** A client can claim any path, so this stops
  accidental bleed but not deliberate access.

## Position B — the repository declares its identity; the client sends that

`.lore.toml` gains a project identity (a stable key, and optionally a separate
display name). The client finds the file by walking up from its working
directory — the way git finds `.git` and cargo finds a workspace root — reads
the identifier, and sends *that*. No path crosses the wire.

**The case for it**

- **It survives remoteness, which is the deciding constraint.** An identifier
  is meaningful to a daemon anywhere. This is the only position compatible with
  the shared dev box as stated.
- **Identity follows the repo.** Same repo, same project key on every machine
  and for every contributor, because the file is committed. `lore add` stops
  guessing identity from a directory name — the mechanism that produced
  `Lexomancy-bench` / `lexomancy-bench` style drift.
- **It is philosophically continuous with D-0012.** That decision already
  established `.lore.toml` as committed configuration that follows the repo
  across machines and contributors. Identity is the same kind of fact as
  authority profile.
- **No path disclosure.** The wire carries an opaque name.
- **It is the precondition for the nested-project idea.** A parent `.lore.toml`
  claiming subdirectory projects that carry their own only makes sense if the
  file is the unit of identity.
- **Client-side resolution decouples the daemon from the client's filesystem**
  entirely, which is what a network service needs.

**The case against it**

- **It puts a second job on an opt-in file.** D-0012 deliberately made
  `.lore.toml` optional and unconfigured repos first-class. If scoping requires
  the file, it is effectively mandatory, and D-0012's optionality clause needs
  revisiting rather than quietly eroding.
- **Declared names collide.** Nothing stops two repos declaring the same key on
  one daemon — and it is *likely*, because forks, worktrees and bench copies
  inherit the file verbatim. The bench worktrees are a live example: they would
  all declare themselves `lore`. Needs an explicit collision rule, and
  "first-registered wins" silently mis-routes the others.
- **Two sources of truth.** Registry and file can disagree. What happens when a
  registered project's file changes its key — re-index, re-register, or error?
- **A copied repo is a copied identity.** Path-based identity distinguishes two
  checkouts for free; declared identity has to be told they differ.
- **Same trust gap.** A client can claim any project key just as easily as any
  path. Neither position authenticates anything.

## What is genuinely shared between them

Worth separating, because it can be decided independently and may collapse the
disagreement:

- **The wire contract can be identical.** "Every request carries a project
  identifier; unscoped requests are rejected" holds under both. The positions
  differ only in *how the client obtains* the identifier and *what the daemon
  does with it*. Fixing the wire contract first is low-regret.
- **Neither solves authentication.** Both stop accidental bleed; neither stops
  a hostile client. Real tenant isolation is issue #18 regardless.
- **`status` needs the same treatment as `search`,** or it keeps enumerating
  every project to every caller. Easy to overlook since it takes no arguments
  today.

## Sub-questions the winner has to answer

Under either position:

- What happens to a request that resolves to no project — hard error naming the
  remedy, or something softer?
- Does `status` scope to the caller's project, or stay machine-wide?
- Do the bench worktrees stay registered at all, or is `lore remove` (store-side
  `remove_project` exists; no CLI subcommand) the actual fix for result mixing?

Under B specifically:

- Stable key versus mutable display name — one field or two?
- Collision rule when two repos declare the same key.
- Does `lore add` write a `.lore.toml` when one is absent, and does that
  overturn D-0012's optionality?
- Is the key required, or does absence fall back to something?

## State of play

- Issue #4 (lexical conjunction) is fixed and closed; unrelated but it is why
  result mixing got looked at.
- Issue #18 tracks multi-tenancy and now records this leak as its concrete
  instance.
- `paths::is_within` and `Store::remove_project` already exist. `lore add`
  currently sends `name: None` and lets the daemon derive a name from the
  directory.
- Nothing has been implemented for scoping. No canon has been changed.

## The larger fork behind this one (Wrysk, 2026-08-16)

Scoping is one instance of a bigger split: **the CLI is a different thing when
it is a client to a shared instance than when it drives a local daemon.** Is a
given caller an administrator of the index or a user of it?

### The capability tiers already exist

The daemon's routes divide cleanly today:

| Tier | Routes | Effect |
| --- | --- | --- |
| Mutating | `POST /projects`, `POST /index` | Changes shared index state |
| Read | `/status`, `/search`, `/expand` | Observes it |

And `lore-mcp` **already withholds the mutating tier** — its server
instructions tell agents that registration and reindex are deliberately
unavailable and to ask the user to run `lore add` / `lore index` instead.

So a two-tier capability model is already in the product. It is enforced by
*omitting tools from one client's surface*, not by the API. Locally that is
sufficient, because the only caller is the machine's owner. Remotely it is not
a boundary at all: the HTTP endpoints are reachable directly, unauthenticated,
by anything that can route to the box.

Making that split explicit and API-enforced is a prerequisite for a shared
instance, and it is independent of how scoping resolves.

### D-0003's "single authoritative owner" acquires a second meaning

The constraint was written about *process* ownership — no multi-process
indexing free-for-all, the failure that crashed the machine. On a shared box it
also becomes a question of *authority*: if user A runs `lore index`, user B's
queries are affected. One owner of index state no longer implies one
beneficiary of it.

### The part that may dominate everything else: ingestion

Lore indexes by walking the filesystem. `lore add <path>` sends a path the
**daemon** must then walk. That is coherent only while both sit on one machine.

A daemon on a shared dev box has three options, and they are not small:

1. **Repos live on the shared box.** Development moves to the server; the
   walker is unchanged. Simplest for Lore, largest change to how Wrysk works.
2. **The daemon reaches the repos.** Network mount, or the daemon clones them
   itself. Keeps local development, but re-introduces path semantics across the
   wire and needs credentials to private remotes.
3. **Ingestion inverts.** The client pushes file content and the daemon never
   touches a filesystem it does not own. Cleanest tenancy story, and by far the
   biggest departure from the current design — watcher, `.loreignore`
   evaluation and incremental hashing all currently live daemon-side.

This is arguably a *larger* fork than the scoping question, and it may
constrain it: option 3 in particular makes path-based anything moot and makes
repo-declared identity close to forced.

**Recorded as an open question, not a proposal.** It touches D-0003 and D-0007
directly and belongs in issue #18's scope. Flagged here because deciding
scoping in isolation risks answering the small question in a way the big one
overturns.

## Recommendation offered, not taken

On the constraint as stated — the daemon moving off the user's machine — **B is
the only position that survives**, and A's advantages (zero configuration, no
collisions, one source of truth) are real but are advantages of a design that
stops working once the daemon is remote.

The honest counter is that A's objections to B are not answered by preferring B;
they are deferred. Collision handling and the D-0012 optionality question are
load-bearing and unresolved, and B is not safe to build until they are.

## Resolution (Wrysk, 2026-08-16): hold — decide the core, defer the mechanism

Reviewed with a synthesis that reframed A and B as answers to different
questions (B: what is a project's portable name; A: how does a local client
discover where it is standing; neither: who arbitrates the binding), and a
recommended amended-B (registry binds declared keys and rejects duplicates at
`lore add`; path resolution demoted to a local-only fallback). Wrysk's call was
narrower than the recommendation:

**Neither identity mechanism is adopted.** A, B, and the amended-B hybrid all
stay open until issue #18's ingestion fork is settled — the fork this brief
already flagged as capable of overturning a premature answer.

**What proceeds now** — the pieces compatible with both positions and all three
ingestion options:

1. **Wire contract.** Every request carries a project identifier; unscoped
   requests are rejected with a hard error naming the remedy. `status` scopes
   to the identified project like `search`; the machine-wide view becomes an
   explicitly admin-tier affordance rather than the default answer.
2. **`lore remove`.** A CLI subcommand over the existing store-side
   `remove_project` — the actual fix for bench-worktree result mixing.

**Interim identifier.** Until the mechanism is chosen, the identifier is the
daemon registry's existing project name. Locally the client may fill it by cwd
containment against the registry as an implementation convenience — without
prejudice to the eventual mechanism, and acceptable only because the daemon is
loopback-only today.

**Explicitly not decided:** how a client obtains the identifier once the daemon
is remote, any `.lore.toml` identity table, collision rules, and whether
`lore add` writes anything into a repo. No canon changed; no ledger entry. The
#18 comment recording an earlier leaning toward `.lore.toml` identity is
corrected there — that leaning is now formally open, not the plan of record.

### Addendum (Wrysk, 2026-08-16, follow-up): `lore add` names the project

One of the open items above is now decided: **`lore add` writes the project
name into `.lore.toml` when none exists** — the name is passable at add time,
defaulting to the local-most folder's name. Precedence: explicit argument > an
existing `.lore.toml` name > root basename; a name already registered to a
different root is a hard error at registration. This gives the *registry* a
committed, human-chosen name per project. It is deliberately **not** a ruling
on the identity *mechanism* (how a client names its project to a remote
daemon), which stays held on #18's ingestion fork — where Wrysk's stated
direction is a flavor of ingestion inversion (client pushes file content), with
the local walk/watch placement still open.

### Resolution addendum (2026-08-16, later same day): the hold is closed

The ingestion fork this hold was waiting on is decided — **D-0015** (snapshot-
manifest push, one observer per machine). With it, the identity mechanism is
decided as **D-0016**: the registry binds the declared `.lore.toml` name,
`lore add` rejects duplicate names, and cwd-containment is demoted to a local
discovery convenience. A declared name is never an authorization identity;
deployments serving more than one trusting party authorize callers in front of
the daemon (issue #18's scope). See [[../1_Architecture/1.2_Ingestion]].
