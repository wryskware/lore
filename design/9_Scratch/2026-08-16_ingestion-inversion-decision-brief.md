---
design_status: exploration
last_reviewed: 2026-08-16
---

# Ingestion inversion — decision brief

**Status: unclassified exploration.** Nothing here is decided. This brief
proposes one design for the ingestion fork recorded in the project-scoping
brief (`2026-08-16_project-scoping-decision-brief.md`, "The part that may
dominate everything else") and in issue #18, states what that design refuses
to build and why, and lists the questions the decision session still has to
answer. It touches D-0003 and D-0007 directly; neither is amended by this
document.

## The fork, restated

Lore ingests by walking the filesystem: `lore add <path>` names a directory
the daemon walks, watches, hashes, and chunks. That is coherent only while
daemon and repo share a machine. The scoping brief named three exits:

1. **Repos move to where the daemon is.** Walker unchanged; development
   moves.
2. **The daemon reaches out to the repos.** Mounts or clones; path semantics
   and credentials cross the wire.
3. **Ingestion inverts.** The client pushes file content; the daemon never
   touches a filesystem it does not own.

This brief develops option 3, on the argument that it is the only exit with a
clean tenancy story, and that — with the design below — its concurrency
hazards can be made structurally impossible rather than carefully avoided.

## The reframe that bounds the lift

The indexer is already a diff machine. A walk produces a set of
`(path, content)` observations; everything downstream — hashing, chunk
comparison, embed queueing, prune — operates on the diff between observations
and stored state. Inversion does not replace that machine. It replaces the
*source of observations*: instead of "walk the disk," the observation set
arrives as a pushed snapshot.

Consequence: the daemon grows **one internal ingestion interface** — "here is
a snapshot of project X" — with two producers. The local walker/watcher
becomes the first producer, in-process. A network pusher becomes the second.
There is one ingestion pipeline, not two, and local mode's behavior is
unchanged in every way a user can observe.

## Proposed design: snapshot-manifest ingestion

### The push unit is a manifest, never an event stream

A push is a **full listing** of the project: `(path, content-hash, size)` per
file, filtered by ignore rules. Deletion is **absence from the manifest** —
there is no delete event to lose, reorder, or replay. Manifests are small
enough for this to hold at scale (~100 bytes/file; a 300k-file repo is a
~30MB listing, compressible), and a wire-level delta encoding against a
daemon-named base generation is available later *as pure transport
optimization*, kept safe by a full-manifest checksum that detects divergence
instead of accumulating it.

> [!IMPORTANT]
> **What this design refuses to build:** an incremental mutation protocol —
> clients streaming "file changed / file deleted" events. That is where the
> concurrency bugs live: ordering, lost deletes, interleaving between
> pushers. Every future proposal to add one "for performance" should be read
> as reopening that door. The manifest negotiation must stay fast enough
> that nobody wants to.

One guard: omission-means-delete converts a botched client-side ignore file
into a mass index wipe. A push that would delete more than a threshold
fraction of a project's files is rejected unless explicitly forced.

### Negotiation, staging, commit

1. Client sends the manifest.
2. Daemon diffs it against the project's committed state and answers with the
   list of paths whose content it needs.
3. Client uploads those files into a **per-push staging area**, keyed by push
   session.
4. Commit is **one transaction**: upsert changed files, delete omitted ones,
   advance the project's generation. This is today's index-pass apply/prune
   with a different observation source. Searches during a push see the old
   state until the flip; no reader ever sees a half-applied push.

A push session that dies leaves staged files and an unflipped generation —
the index never sees partial state. Staging cleanup is "delete the session
directory on commit or lease expiry": temp-dir hygiene, not garbage
collection.

> [!IMPORTANT]
> **Also refused: a content-addressed blob store with multi-generation
> retention and reference-counted GC.** An earlier sketch had one; it is
> over-designed. The daemon keeps **current generation only**, keyed by
> `(project, path)` — replaced on write, removed on omission, in the commit
> transaction. Nothing has more than one owner or one lifetime, so there is
> nothing to collect. The cost is losing cross-file dedup (not load-bearing)
> and blob-level resumption — but file-level resumption survives for free,
> because negotiation already skips any path whose hash matches committed
> *or* staged content.

### One pusher per project: leases with epoch fencing

The daemon grants a per-project **push lease** carrying a monotonically
increasing epoch; every push names its epoch. Leases heartbeat and expire on
a TTL, so a dead client blocks a successor for seconds, not until cleanup.

If two pushers contend — two local daemons on one machine, a bypassed
singleton, two checkouts of one project — the server resolves it: the second
acquirer either waits or takes over (bumping the epoch), and the stale
pusher's next push is **rejected with a named error**, not interleaved. The
server never trusts client-side singleton discipline. The worst sustained
failure is flapping between two internally-consistent snapshots — detectable
as epoch churn, reportable in status, and never corrupting, because each
generation is wholly one pusher's view. This is D-0003's single-owner
principle surviving the wire: races degrade to loud rejection.

### Topology: one filesystem observer per machine

The walker and watcher live in the **local daemon** and nowhere else. Agent
clients (MCP, CLI) keep exactly today's verbs; none of them ever pushes
content or triggers a walk — they were never given the verb, so independent
agents cannot race to reindex.

- **Local mode:** the walker feeds the snapshot interface in-process. The
  disk remains the content source; watcher events debounce into
  micro-manifests. Nothing is retained beyond today's store; `expand` reads
  from disk exactly as now (`daemon/expand.rs`).
- **Remote mode:** the local daemon is a forwarder — it observes the
  filesystem and pushes snapshots to a remote daemon that owns the index.
  The inversion happens on the daemon→server hop, not the agent→daemon hop.

### Storage in remote mode

The server needs file content transiently to chunk and embed — unavoidable.
Retention beyond that is bounded and smaller than it looks:

- What crosses the wire is the **ignore-filtered text subset**, not the repo.
  A 40GB checkout with binaries, artifacts, and VCS history indexes as
  single-digit GB of source text, on the *server's* disk, never the user's
  machine. Local mode retains nothing extra, full stop.
- The store **already retains chunk text** for FTS5 and search excerpts, so
  keeping compressed current-generation per-file text (to serve `expand`
  context, which currently reads the file from disk) is a bounded multiple
  of existing storage, not a new category. zstd on source text runs 3–4×.
- Even if lexical search were someday dropped (an open question Wrysk has
  raised, out of scope here), excerpts and `expand` still require text
  server-side — this retention does not hinge on FTS5's fate.

### Secrets: layered defaults, not documentation

Under inversion a bad `.loreignore` stops being a poisoned local index and
becomes exfiltration. Defense in depth, in priority order:

1. **Git-tracked-only manifests by default** for git repos (`git ls-files` ∩
   ignore rules). Untracked `.env`, keys, and credential files never enter
   the manifest. This one default removes most of the leak surface.
2. **Non-overridable client-side hard excludes** for credential patterns
   (`.env*`, `id_rsa*`, `*.pem`, cloud credential directories, high-entropy
   small files). `.loreignore` cannot override these; only explicit named
   configuration can.
3. **First remote push is a reviewable dry-run** of the manifest by default.
4. **Server-side scan-on-ingest as backstop**, quarantine-and-report rather
   than silent indexing.
5. **TLS mandatory on the remote path.** The deliberate no-TLS reqwest
   posture is loopback-era and ends at this boundary; at-rest encryption is
   a server-side requirement.

Non-git projects (Lexomancy's registered root is the live example — no VCS
governs it) skip (1) and rely on walker + (2); that asymmetry is a real
residual risk to weigh.

## Inventory: what currently assumes disk access

| Coupling | Today | Under inversion |
| --- | --- | --- |
| Walker/watcher | daemon-side, per registered root | local-daemon-side only; feeds snapshot interface |
| `.loreignore` evaluation | daemon-side during walk | pusher-side (decides what to send); server keeps hard-exclude backstop |
| `expand` context | reads file from disk at query time (`daemon/expand.rs:116`) | local: unchanged; remote: served from retained compressed text |
| Ledger / authority parsing | reads from `project.root` during indexing (`daemon/index.rs:381`) | parses pushed content; `.lore.toml` and `0_Canon` are just files in the snapshot |
| Chunking + `CHUNK_FORMAT_VERSION` | daemon-side | unchanged — clients send content, never chunks |
| `lore add <path>` | names a directory the daemon walks | becomes "create project + start pushing"; paths mean nothing to a remote daemon |
| Project identity | registry name; cwd-containment as local convenience | declared identity effectively forced (see below) |

## Interaction with held and existing decisions

- **Scoping hold resolves as predicted.** The scoping brief held the identity
  mechanism *because* this fork could overturn it. Inversion makes path-based
  identity moot on the remote hop: the pusher must name its project, so
  declared/registered identity is close to forced. The amended-B shape
  reviewed in the scoping brief (registry binds declared keys, rejects
  duplicates at `lore add`, containment as local-only convenience) is the
  natural fit.
- **D-0003** (single authoritative owner of index state) is preserved, not
  weakened: one filesystem observer per machine, one index owner per
  deployment, and the lease/epoch mechanism makes multi-owner violation a
  rejected request instead of a corruption. Its **local-only embeddings**
  clause, however, was written for the co-located world; a remote deployment
  embeds where the index lives. Whether that clause needs scoping or
  amendment is a session question, not assumed here.
- **D-0007** (loopback HTTP, thin MCP proxy) is per-deployment: the agent→
  local-daemon hop stays loopback; the daemon→server hop is new surface with
  its own transport requirements.

## Open questions for the decision session

1. **Lease conflict policy:** takeover-with-epoch-bump (matches the
   handshake publish-over posture) or refuse-while-held? Takeover favors
   liveness; refusal favors surprise-avoidance.
2. **Ignore/exclude enforcement locus:** trusted client evaluation with
   server backstop scanning, or duplicated server-side enforcement? The
   backstop-only shape is simpler; duplication protects multi-user servers
   from a hostile or broken pusher.
3. **Project identity:** does this session formally resolve the held scoping
   question (declared identity, amended-B shape), or does it stay held until
   the wire contract lands?
4. **Local mode plumbing:** in-process snapshot interface only (proposed), or
   does the local walker literally speak the push protocol over loopback?
   Wire-identical is better dogfooding of the remote path but adds
   serialization cost to every local index pass for no user-visible gain.
5. **Manifest wire format:** full listing only in v1 (proposed), with
   checksummed delta encoding deferred until a measured need?
6. **Mass-delete guard:** threshold, and the override UX (`--force` on which
   surface, reported how in status?).
7. **Remote `expand` context:** retain compressed current-generation text
   (proposed), or accept chunk-boundary-degraded context to retain nothing
   beyond the chunk store?
8. **Watch latency:** what debounce cadence for micro-manifests keeps the
   live-index feel that T5-style tasks depend on, and does cadence differ
   local vs. remote?
9. **Secrets floor for non-git projects:** is walker + hard-excludes enough,
   or do non-git roots require an explicit allowlist before their first
   remote push?
10. **Ledger touchpoints:** does D-0003's local-only-embeddings clause get
    scoped to local deployments, and does D-0007 get an amendment naming the
    daemon→server hop, or does the whole fork land as one new decision that
    partially supersedes both?
11. **Entropy/credential scanning cost:** acceptable false-positive rate for
    the hard-exclude heuristics, and what the override ritual looks like
    when a legitimate file trips it.
