---
design_status: exploration
last_reviewed: 2026-08-14
---

# Adversarial Review — Session 2: Concurrency, Lifecycle, and Windows Behavior

## Scope and verification

- **Implementation reviewed:** commit `3e791d2`. Repository `HEAD` was
  `31105b3`; `3e791d2..HEAD` changes only the adversarial-review brief, not
  production or test code. The pre-existing modified brief and untracked
  Session 1 report were preserved.
- Read the live bodies of GitHub issues #1–#9 and the Session 1 report. The
  simultaneous two-starter check-then-publish race is not repeated here.
- `cargo test --workspace --all-targets --quiet`: **217 passed, 0 failed**.
  `cargo test --workspace --all-targets -- --list` independently counted 217
  tests. The current suite therefore does not exercise the failures below.
- Dependency semantics used below were checked against the locked sources:
  `tokio-util` 0.7.19 documents that dropping `TaskTracker` does not abort its
  tasks, and Tokio 1.53.1 documents that a started `spawn_blocking` task cannot
  be cancelled and normal runtime shutdown waits indefinitely for it.

## Findings

### 1. The shutdown deadline withdraws ownership while uncancellable work can still mutate the index

- **Severity:** critical
- **Confidence:** high
- **Binding constraint:** D-0003 — exactly one authoritative owner of index state
- **Locations:** `crates/lore/src/daemon/mod.rs:198`,
  `crates/lore/src/daemon/mod.rs:200`, `crates/lore/src/daemon/mod.rs:212`,
  `crates/lore/src/daemon/index.rs:435`, `crates/lore/src/daemon/index.rs:437`,
  `crates/lore/src/main.rs:61`
- **Failure scenario:** A full scan is walking a large/slow Windows or UNC
  project, or is inside the read/hash/chunk/SQLite work for one file, when
  shutdown is requested. Cancellation is checked only between files and a
  started `spawn_blocking` pass cannot be aborted. Ten seconds later,
  `timeout(tracker.wait())` expires. The code logs “exiting anyway,” withdraws
  `daemon.json`, and returns even though `TaskTracker` has not stopped the
  index task. A new daemon now sees no handshake and opens the same SQLite
  database while the old blocking pass can still call
  `replace_file_chunks`, `remove_file`, or `bump_generation`.
- **Two consequences:** When `run` is hosted by a longer-lived runtime (as it
  is in tests or could be as a library), the detached old task can continue
  after `run` reports success. In the shipped binary, dropping the runtime
  after `block_on` waits indefinitely for started blocking work, so the
  advertised ten-second exit bound is false even when no second process is
  started. A half-sent HTTP request creates the analogous async case: axum
  graceful shutdown waits for the connection, the tracker timeout does not
  abort it, and its handler can resume after ownership was withdrawn.
- **Required direction:** Do not remove the ownership token until every task
  capable of touching the store is known stopped. Make passes cooperatively
  cancellable inside walks/files and explicitly abort/close residual async
  work after grace. The process-lifetime ownership primitive recommended in
  Session 1 must outlive every store handle, not merely `run`'s wait future.

### 2. Deleting or corrupting `daemon.json` lets a live incumbent be replaced without any liveness probe

- **Severity:** critical
- **Confidence:** high
- **Binding constraint:** D-0003 — exactly one authoritative owner of index state
- **Locations:** `crates/lore/src/daemon/handshake.rs:101`,
  `crates/lore/src/daemon/handshake.rs:104`,
  `crates/lore/src/daemon/handshake.rs:105`,
  `crates/lore/src/daemon/handshake.rs:120`,
  `crates/lore/src/daemon/mod.rs:229`, `crates/lore/src/daemon/mod.rs:232`
- **Failure scenario A:** Daemon A is healthy. A user, cleanup tool, sync
  client, or antivirus removes `daemon.json` just after a heartbeat. Before
  the next 15-second heartbeat recreates it, daemon B starts. `preflight`
  returns `Ok` immediately on `None`, never probes A, and B becomes a second
  SQLite owner.
- **Failure scenario B:** The record becomes unreadable/corrupt while A is
  live. `preflight` logs and treats it as stale without probing. This creates
  the same second owner. Atomic normal heartbeat replacement prevents Lore's
  own partial writes, but it does not make deletion, ACL changes, external
  edits, disk faults, or corrupt bytes an ownership proof.
- **Distinct from Session 1:** This does not require simultaneous starters to
  pass the same initial check. An already-established incumbent loses
  exclusivity because the discovery record disappears or cannot be parsed.
  A lifetime-held OS lock would close both variants; a heartbeat file cannot.

### 3. Overlapping project roots deterministically leave one project's index stale

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/daemon/watch.rs:73`,
  `crates/lore/src/daemon/watch.rs:83`, `crates/lore/src/daemon/watch.rs:86`,
  `crates/lore/src/daemon/watch.rs:150`, `crates/lore/src/daemon/watch.rs:152`,
  `crates/lore/src/daemon/watch.rs:165`
- **Failure scenario:** Register `C:\repo` and then
  `C:\repo\packages\game` as separate projects. Both get an initial scan and
  recursive watch. Edit `packages\game\Player.cs`. For every delivered path,
  `find_map` chooses only the first watched root that contains it and queues
  work for only that project. If the outer root was registered first, the
  nested project remains stale; reverse registration order and the outer
  project remains stale. Duplicate notifications from the two recursive
  watches do not help because each identical absolute path makes the same
  first-match choice.
- **Required direction:** Either reject overlapping registrations explicitly
  or route each event to every containing project (then coalesce per project).
  If overlap is supported, prefer a test with both registration orders and a
  real Windows watcher.

### 4. Strict lowest-project-ID scheduling can starve every later project indefinitely

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/daemon/queue.rs:94`,
  `crates/lore/src/daemon/queue.rs:98`, `crates/lore/src/daemon/queue.rs:99`,
  `crates/lore/src/daemon/index.rs:415`, `crates/lore/src/daemon/index.rs:435`,
  `crates/lore/src/daemon/index.rs:437`
- **Failure scenario:** Project 1 is a large Unity tree receiving filesystem
  churn continuously; project 2 has one pending manual full scan. The indexer
  removes project 1's work and runs it. Events arriving during that pass create
  a new project-1 entry. When the pass ends, the map contains projects 1 and 2,
  and `keys().next()` selects 1 again. As long as each project-1 pass overlaps
  at least one new event, project 2 is never selected. Coalescing bounds project
  1's pending memory but provides no service fairness.
- **Required direction:** Maintain a ready FIFO/round-robin of project IDs or
  rotate the last-served key. Add a deterministic test that requeues the low ID
  after each take and proves a higher ID is served within a fixed number of
  passes.

### 5. The bounded `IndexQueue` sits behind an unbounded watcher event channel

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/daemon/watch.rs:66`,
  `crates/lore/src/daemon/watch.rs:69`, `crates/lore/src/daemon/watch.rs:70`,
  `crates/lore/src/daemon/watch.rs:96`, `crates/lore/src/daemon/watch.rs:97`,
  `crates/lore/src/daemon/watch.rs:129`
- **Failure scenario:** A Windows branch switch, Unity import, or generated-tree
  rewrite produces debounced batches faster than the single async watcher pump
  can iterate and normalize every path in the preceding batch. The callback
  sends whole `DebounceEventResult` values into `mpsc::unbounded_channel`, so
  queued batches retain every `PathBuf` and event attribute. The 4,096-path
  collapse happens only later, after `handle_batch` receives and translates a
  batch. Memory can therefore grow with raw events despite the module's claim
  that the downstream queue bounds it.
- **Required direction:** Put backpressure or an explicit byte/event bound at
  the callback boundary. On overflow, discard detailed batches and atomically
  set a rescan-needed bit; the next pump iteration can request one full scan per
  affected project.

### 6. A transient watch-arm failure permanently disables live indexing without surfacing it in status

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/daemon/watch.rs:80`,
  `crates/lore/src/daemon/watch.rs:83`, `crates/lore/src/daemon/watch.rs:88`,
  `crates/lore/src/daemon/watch.rs:90`, `crates/lore/src/daemon/mod.rs:176`,
  `crates/lore/src/daemon/mod.rs:183`, `crates/lore/src/daemon/http.rs:179`
- **Failure scenario:** On daemon startup, a registered project is briefly
  unavailable because a removable/network volume is reconnecting or Windows
  returns a transient watch error. `debouncer.watch` fails once. Lore logs the
  error, does not add the project to `watched`, and never retries. The startup
  scan may succeed after the volume returns, giving an apparently healthy
  index, but every later edit is missed until another registration request
  happens to resend `Watch`. `/v1/status` exposes counts and embedding health,
  not watcher coverage, so the degraded state is invisible.
- **Required direction:** Retain desired watches separately from successfully
  armed watches, retry with bounded backoff, and surface per-project watcher
  state. A backend error that invalidates an existing watch should move it back
  to the retry set after scheduling the safety rescan.

### 7. A backward clock step turns the known 45-second restart delay into an arbitrary outage

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore-core/src/discovery.rs:105`,
  `crates/lore-core/src/discovery.rs:106`,
  `crates/lore/src/daemon/handshake.rs:111`,
  `crates/lore/src/daemon/handshake.rs:112`,
  `crates/lore/src/daemon/handshake.rs:120`
- **Failure scenario:** A daemon writes a heartbeat at wall-clock 12:00. Windows
  time synchronization, VM restore, or a manual correction moves the clock
  back to 11:00, and the daemon then crashes. Every restart sees a negative
  heartbeat age, classifies it as fresh, and refuses startup without probing
  the dead port. The daemon remains unavailable until wall time passes the old
  timestamp plus 45 seconds — roughly an hour here and potentially much longer
  after snapshot restore.
- **Evidence and overlap:** The existing
  `a_heartbeat_from_the_future_counts_as_fresh` test deliberately pins this
  behavior. This is not issue #8's ordinary at-most-45-second hard-kill window;
  clock rollback removes that bound.
- **Required direction:** A future heartbeat should be “indeterminate,” not
  unconditionally live: probe the advertised endpoint, and use a
  process-lifetime lock as the actual safety decision. Wall time is suitable
  for diagnostics, not elapsed-liveness authority.

### 8. A stale query completion can overwrite a newer successful probe and force 60 seconds of false degradation

- **Severity:** minor
- **Confidence:** high
- **Binding constraint:** D-0007 — embedding degradation must be visibly and accurately surfaced
- **Locations:** `crates/lore/src/embed/mod.rs:169`,
  `crates/lore/src/embed/mod.rs:173`, `crates/lore/src/embed/mod.rs:182`,
  `crates/lore/src/embed/worker.rs:132`,
  `crates/lore/src/embed/worker.rs:146`,
  `crates/lore/src/embed/worker.rs:150`, `crates/lore/src/embed/worker.rs:153`
- **Failure scenario:** Query Q starts while health is Ready and its embedding
  request stalls. A worker batch fails, marks health Unreachable, and the worker
  immediately probes; the probe succeeds and publishes Ready. Q's older request
  then times out and overwrites that newer Ready state with Unreachable. If the
  worker's current drain reaches `Idle`, line 146 does not take the immediate
  re-probe path (it is gated on `Drained::Interrupted`) and the worker sleeps
  until a notify or the 60-second tick. During that interval status lies and
  every search is forced lexical-only despite the newer successful probe.
- **Distinct from issue #5:** #5 records repeated model cold-load timeout
  flapping. This is a last-completer-wins race: an older request invalidates a
  newer health observation, and the idle branch extends the bad state for a
  full tick.
- **Required direction:** Version health observations (or compare request
  epochs) so an older completion cannot replace a newer one. Independently,
  any `Idle` drain that observes non-Ready health should re-enter the probe arm
  rather than sleep.

## Smells, debts, and hardening ideas

- The “store guard cannot be held across an `.await`” claim is true for the
  closures passed to `StoreHandle::with`, but not a compile-time property of
  the handle: public `StoreHandle::blocking` can be called from any async task.
  Current production call sites use it only inside the indexer's
  `spawn_blocking` pass; narrow the visibility or require an explicit blocking
  context before a future call site violates the convention.
- “One lock per file” is not synonymous with a short critical section.
  `replace_file_chunks` performs the complete SQLite/FTS transaction for up to
  a 2 MiB file; `vector_search` scans every candidate vector; and a poison-heavy
  embed worker hydrates up to 5,000 complete chunk rows (roughly 20 MiB at the
  4 KiB chunk ceiling) under one store acquisition. Instrument lock wait/hold
  duration and consider moving policy-neutral hydration outside the mutex.
- The liveness probe has a one-second total timeout, but `/v1/status` itself
  waits for the same store mutex as brute-force vector search and indexing.
  Once a heartbeat is stale because writes failed, a temporarily busy store can
  therefore turn a live daemon into a false takeover. The lifetime lock is the
  correctness fix; a lock-free minimal liveness endpoint would still improve
  diagnosis.
- `watch::channel` for commands is also unbounded. Repeated local registration
  requests can accumulate duplicate `Project` values even though the consumer
  later treats a project ID as idempotent. A bounded desired-state channel or a
  shared set would match the event-side hardening.
- Windows containment uses ASCII-only case folding. That handles drive letters
  and ordinary source trees, but it is not Windows' full Unicode
  case-insensitive path comparison. Add non-ASCII case and long/verbatim UNC
  fixtures before claiming general Windows path identity.
- Heartbeat publication performs synchronous file write and rename on a Tokio
  worker. Usually tiny, but an unhealthy disk, filter driver, or redirected
  data directory can block a runtime thread. Move it to blocking IO and expose
  consecutive heartbeat failures in status/log escalation.
- Store-mutex and health-lock poisoning recovery assumes the protected state is
  valid after an arbitrary panic. SQLite transactions limit database damage,
  but not every store method is transactional and a panic can leave in-memory
  protocol state inconsistent. Treat poison as a daemon-fatal invariant breach
  unless the exact panicking operation proves recovery safe.

## Minimal interleaving traces

### R1 — shutdown releases ownership before the old writer stops (Finding 1)

1. **Indexer A:** `spawn_blocking` begins a full scan and enters slow file or
   store work.
2. **Shutdown task:** cancels the token; A cannot observe it until the current
   blocking work returns.
3. **Shutdown task:** waits ten seconds; `tracker.wait()` times out. Dropping the
   wait/tracker does not abort A.
4. **Shutdown task:** removes A's `daemon.json` and returns from `run`.
5. **Daemon B:** sees no record, opens the same `lore.db`, publishes itself.
6. **Indexer A:** resumes and mutates the same database under its old
   `StoreHandle`; two authoritative owners now exist.

### R2 — incumbent record deletion creates takeover (Finding 2)

1. **Daemon A heartbeat:** publishes a fresh record, then sleeps for 15 seconds.
2. **External actor:** deletes or corrupts `daemon.json`; A remains healthy.
3. **Daemon B:** calls `preflight`; `read` returns `None` or `Err`.
4. **Daemon B:** returns `Ok` without probing A and opens `lore.db`.
5. **Daemon A heartbeat:** later republishes A, racing B's record while both
   watcher/indexer stacks remain alive.

### R3 — stale query failure overwrites newer Ready (Finding 8)

1. **Query Q:** reads Ready and starts an embedding request.
2. **Worker W:** observes a batch failure, writes Unreachable, and starts a
   probe.
3. **Worker W:** probe succeeds and writes Ready; its next drain finds no work.
4. **Query Q:** its older request times out and writes Unreachable.
5. **Worker W:** returns `Drained::Idle`, skips the interrupted-only immediate
   probe condition, and waits up to 60 seconds while status remains stale.
