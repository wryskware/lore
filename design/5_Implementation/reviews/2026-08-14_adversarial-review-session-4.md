---
design_status: exploration
last_reviewed: 2026-08-14
---

# Adversarial Review — Session 4: Test-Suite Quality Audit

## Scope and verification

- **Implementation reviewed:** commit `3e791d2`. Repository `HEAD` was
  `31105b3`; `3e791d2..HEAD` adds only the adversarial-review brief. The
  existing modified brief and untracked Session 1–3 reports were preserved.
- Read the live bodies of GitHub issues #1–#9, all three prior reports, the
  binding decision ledger, every test target, all checked-in snapshots, and
  the test-support servers/harnesses.
- `cargo test --workspace --all-targets --quiet`: **217 passed, 0 failed**.
  `cargo test --workspace --all-targets -- --list` independently counted 217.
- Verdict: this is not a slop suite. The store churn test, exact-span
  invariants, HTTP rejection checks, shuffled embedding-response test, and
  readable structural snapshots are substantive. It is nevertheless much
  better at confirming the intended happy-path model than falsifying the
  concurrency, Windows, bounded-backlog, and stale-data seams. Every critical
  or major Session 1–3 defect survived all 217 tests, and several tests encode
  the defective behavior as the expected contract.

## Findings

### 1. The single-owner tests prove only sequential discovery, not mutual exclusion

- **Severity:** critical
- **Confidence:** high
- **Binding constraint:** D-0003 — exactly one authoritative owner of index state
- **Locations:** `crates/lore/tests/daemon_lifecycle.rs:135`,
  `crates/lore/tests/daemon_handshake.rs:172`,
  `crates/lore/tests/daemon_handshake.rs:180`,
  `crates/lore/tests/daemon_handshake.rs:205`,
  `crates/lore/src/daemon/handshake.rs:101`
- **Failure scenario:** Put two daemon starts behind a barrier on an empty data
  directory. Both can read `None` before either publishes, both open the store,
  and both run. The test named `a_second_daemon_on_the_same_data_dir_refuses_to_start`
  cannot see this: it waits for daemon A's published record before starting B.
  The clean-directory test separately establishes that `None` admits a starter,
  but no test composes those two facts concurrently.
- **Mutation result:** The current check-then-publish implementation, with no
  lifetime lock or atomic claim, passes every ownership test. Deleting the
  incumbent's record also still admits a second daemon; corrupting it is
  explicitly asserted to permit takeover.
- **Required test:** A barrier-controlled two-starter process test must assert
  that exactly one process acquires a crash-releasing ownership primitive and
  that the loser never opens a mutable store. Separate tests must delete and
  corrupt discovery metadata while a live owner holds that primitive and prove
  that neither event changes ownership.

### 2. The worker tests use tiny backlogs, so the bounded poison-window failure is invisible

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/tests/embed_worker.rs:136`,
  `crates/lore/tests/embed_worker.rs:148`,
  `crates/lore/tests/embed_worker.rs:167`,
  `crates/lore/src/embed/worker.rs:66`,
  `crates/lore/src/embed/worker.rs:269`
- **Failure scenario:** Poison the oldest 5,000 missing chunks and leave one
  good chunk at row 5,001. `next_batch` fetches at most `MAX_FETCH`, filters the
  whole page away, and reports an empty batch/`Idle`; the good row never
  reaches the endpoint. The existing poison test has three chunks, one poison,
  and `batch_max_items = 1`, so widening the fetch by one is sufficient and the
  cap interaction is never exercised.
- **Mutation result:** Changing `MAX_FETCH` to any value above the three-row
  fixture, removing pagination beyond the first fetched page, or stopping at
  the first all-poison page leaves the complete suite green.
- **Required test:** Seed `MAX_FETCH + 1` missing rows, pre-poison the first
  `MAX_FETCH`, and assert that a drain neither returns `Idle` nor leaves the
  last row unembedded.

### 3. A test claiming worker/store vector parity does not test the store's boundary

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/embed/worker.rs:385`,
  `crates/lore/src/embed/worker.rs:397`,
  `crates/lore/src/store/vector.rs:12`,
  `crates/lore/tests/embed_worker.rs:81`
- **Failure scenario:** The endpoint returns `[1e-10, 0.0]`. Worker `usable`
  accepts its positive finite squared norm, while store normalization rejects
  its norm as `<= f32::EPSILON`; the transaction fails and the row retries
  forever. `usable_rejects_what_the_store_would_reject` checks only empty,
  exactly-zero, NaN, and infinity inputs, so its name overclaims the property.
- **Mutation result:** Replacing the worker predicate with any positive-norm
  check continues to pass. No worker test scripts a successful HTTP response
  containing a tiny finite vector, mixed dimensions, or one malformed member
  among valid vectors.
- **Required test:** Table-test values immediately below, at, and above the
  store normalization threshold through the worker's real store-write path;
  assert that worker and store agree and that one rejected vector cannot block
  valid peers.

### 4. Ranking tests bypass candidate acquisition and certify the faulty collapse rule

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/daemon/search.rs:43`,
  `crates/lore/src/daemon/search.rs:123`,
  `crates/lore/src/daemon/search.rs:415`,
  `crates/lore/src/daemon/search.rs:498`,
  `crates/lore/src/daemon/search.rs:522`
- **Failure scenario A:** Put one chunk at rank 51 in both complete arms. It is
  the mathematically best RRF result, but `execute` fetches only 50 candidates
  per arm. Every RRF unit test calls `fuse` with already-materialized vectors,
  so candidate truncation is outside the tested surface.
- **Failure scenario B:** Return two C# overloads with the same structural path,
  or two Markdown sections with the same heading path. The current collapse
  hides one. The test named `section_windows_collapse_but_distinct_headings_do_not`
  uses different heading names for its "distinct" case and explicitly expects
  an unsuffixed whole section to collapse with `#w` windows; it never challenges
  equal-anchor non-window chunks.
- **Mutation result:** Removing any requirement that collapse be positively
  gated on `#w` membership is effectively the current behavior and passes.
  Changing `LEXICAL_CANDIDATES`/`VECTOR_CANDIDATES` to another fixed depth also
  passes every ranking test.
- **Required test:** Exercise `execute`, not only `fuse`, with a rank-51 shared
  candidate and with enough collapsed windows to require refill. Unit-test two
  overloads and two repeated headings whose anchors match but spans/IDs differ.

### 5. The watcher tests deliberately remove watcher startup, retry, overlap, and storm failure states

- **Severity:** major
- **Confidence:** high
- **Binding constraint:** D-0003 — Windows-native behavior
- **Locations:** `crates/lore/tests/daemon_watch.rs:102`,
  `crates/lore/tests/daemon_watch.rs:112`,
  `crates/lore/tests/daemon_watch.rs:131`,
  `crates/lore/tests/daemon_watch.rs:180`,
  `crates/lore/src/daemon/watch.rs:66`,
  `crates/lore/src/daemon/watch.rs:83`
- **Failure scenario:** Register `C:\repo` and `C:\repo\game`, then edit a
  file under the nested root; only the first containing project is queued.
  Alternatively, fail the initial watch arm once or deliver raw event batches
  faster than the unbounded callback channel drains. The suite has only two
  watcher tests, one project in each, no injectable watcher backend, and no
  status/retry assertion.
- **Why timing hides rather than tests startup:** `start_watcher` sleeps for
  exactly one debounce interval before making the first edit. It cannot prove
  that registration-to-arm events are retained, or even observe when the arm
  completed. It assumes away the gap.
- **Mutation result:** Route every event to only the first containing root,
  drop an arm error permanently, remove overflow recovery, or leave the raw
  callback channel unbounded; both watcher tests remain green.
- **Required test:** Extract an injectable event-routing/desired-watch seam.
  Test both nested-root registration orders, arm-fail-then-retry, backend error
  to rescan/re-arm, and callback overflow collapsing to one full scan.

### 6. `expand` tests distinguish stable versus missing files, but not stale live files

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/tests/daemon_http.rs:387`,
  `crates/lore/tests/daemon_http.rs:423`,
  `crates/lore/tests/daemon_http.rs:451`,
  `crates/lore/src/daemon/expand.rs:46`
- **Failure scenario:** Search a chunk at line 200, insert 100 lines above it
  without reindexing, then expand the old ID. The old line remains within EOF,
  so the handler returns unrelated current text. Existing tests cover an
  unchanged file, clamping at file bounds, and a deleted file that triggers
  stored-text fallback; none changes a still-readable file between search and
  expand.
- **Mutation result:** Removing any verification that disk text still matches
  the stored chunk survives because no current test creates a content/span
  disagreement.
- **Required test:** Shift an indexed chunk while keeping its old line range
  valid, expand the old ID, and assert stored text or an explicit stale result,
  never unrelated disk lines.

### 7. The Windows/path fixtures are ASCII-only and cannot establish Windows path semantics

- **Severity:** major
- **Confidence:** high
- **Binding constraint:** D-0003 — Windows-native behavior
- **Locations:** `crates/lore/src/chunk/mod.rs:191`,
  `crates/lore/tests/chunk_invariants.rs:64`,
  `crates/lore/tests/store_sqlite.rs:448`,
  `crates/lore/src/daemon/paths.rs:149`
- **Failure scenario:** Filter `données/parser.cs` with `données/`, or filter
  `Assets/Scripts/Foo.cs` with alternate Windows casing. The first fails from
  UTF-8 byte length versus SQLite character length; the second from a
  case-sensitive comparison. The path-ID tests vary only `/` versus `\`, and
  store filters use ASCII `design/`.
- **Mutation result:** ASCII-only case folding, byte-count prefix lengths, and
  case-sensitive SQL all pass. Likewise, no chunk fixture combines UTF-8 BOM
  and CRLF, so dropping frontmatter recognition in that common Windows encoding
  is invisible.
- **Required test:** Add accented/non-Latin prefixes, alternate-case Windows
  paths, non-ASCII case pairs, verbatim/UNC forms, and BOM+CRLF vault fixtures.

### 8. MCP goldens validate rendering while accepting any forwarded request body

- **Severity:** minor
- **Confidence:** high
- **Locations:** `crates/lore-mcp/tests/mcp_golden.rs:33`,
  `crates/lore-mcp/tests/mcp_golden.rs:35`,
  `crates/lore-mcp/tests/mcp_golden.rs:217`,
  `crates/lore-mcp/src/server.rs:137`,
  `crates/lore-mcp/src/daemon.rs:165`
- **Failure scenario:** A refactor drops `project`, `status`, and `limit` before
  sending `POST /v1/search`. The unit conversion test still passes in isolation,
  and the golden stub ignores the request body and returns the same canned
  response for every call. The agent-facing snapshot remains unchanged.
- **Mutation result:** Hard-code a default `SearchRequest` in the live tool
  handler or client call; the affected search golden still receives the same
  canned body, while the other goldens do not inspect that request. The harness
  also has no non-2xx API response, malformed successful JSON, wrong content
  type, or connection-close case.
- **Required test:** Record and assert method, route, and exact JSON at the stub
  for every tool, then add JSON/non-JSON 4xx/5xx and malformed-2xx cases and
  assert a visible MCP tool error.

### 9. Several asynchronous tests are bounded only after the operation that can hang

- **Severity:** minor
- **Confidence:** high
- **Locations:** `crates/lore/tests/daemon_lifecycle.rs:27`,
  `crates/lore/tests/daemon_lifecycle.rs:69`,
  `crates/lore/tests/daemon_lifecycle.rs:117`,
  `crates/lore/tests/daemon_watch.rs:112`,
  `crates/lore/tests/embed_client.rs:127`
- **Failure scenario:** A daemon accepts a test connection but never completes
  `/v1/status` or `/v1/search`. The lifecycle test's 30-second timeout wraps
  only the final daemon `JoinHandle`; the preceding `reqwest::Client::new()`
  calls have no client/request timeout, so the test can hang before reaching
  the bound. Separately, watcher arming uses a fixed sleep and `Retry-After`
  asserts a wall-clock upper bound, both vulnerable to a slow CI scheduler.
- **Required direction:** Put one timeout around each whole async scenario,
  expose readiness instead of sleeping for `DEBOUNCE`, and use Tokio paused
  time for retry/backoff policy tests.

## Mutation-lens scorecard

`COVERED` means the named subtle mutations are caught through an independent
observable. `PARTIALLY` means important branches are real tests but at least one
load-bearing mutation survives. `THEATER` means the tests' names/comments claim
the invariant while their arrangement cannot distinguish its violation.

| Invariant | Grade | Mutations caught | Mutation that survives |
|---|---|---|---|
| 1. Chunk-ID stability | **PARTIALLY** | Separator normalization removed; nondeterministic re-chunk order/IDs; content change reuses ID | Omit the structural anchor from the hash. Fixtures do not contain same-text distinct symbols, so uniqueness and re-chunk tests still pass. Windows case-equivalent spellings are absent. |
| 2. Replace atomicity + embedding survival | **PARTIALLY** | Delete a kept embedding; fail to evict a stale ID; fail to update a kept span | Move one write outside the transaction/remove rollback protection. No test induces an error after an early write and then inspects pre-state. |
| 3. FTS row lifecycle | **COVERED** | Drop insert/delete trigger, omit explicit pre-cascade chunk delete, or leave an orphan after churn | No meaningful supported-API lifecycle mutation found that survives the white-box FTS5 `integrity-check`; this is the suite's strongest storage test. |
| 4. FTS sanitization | **PARTIALLY** | Preserve hostile syntax, lose prefix `*`, remove the cap entirely | Change the cap from 64 to another finite value: `sanitize_caps_term_count` derives its expected count from production `MAX_TERMS`, so policy drift passes. |
| 5. Vector top-k/filter/dimensions | **PARTIALLY** | Reverse top-k ordering, ignore normalization, silently accept dimension mismatch | Apply project/language filters after a generous top-k. Current fixtures and limits return the same rows, so the claimed SQL pushdown is not distinguished. |
| 6. RRF/authority/collapse | **PARTIALLY** | Zero-vs-one-based rank error; reverse authority ordering; fail to collapse explicit windows | Change `RRF_K` while the exact-score test imports the same constant; truncate at any fixed candidate depth; collapse equal non-window anchors. The last is the current defect and is partly asserted as expected. |
| 7. Handshake matrix | **THEATER** for exclusivity | Fresh/stale boundary, stale live Lore responder, dead port, stranger body, owned withdrawal | Remove atomic/lifetime ownership entirely; two simultaneous clean starters and a live owner whose record vanished remain untested. Corrupt-record takeover is pinned as success. |
| 8. Indexer change detection | **PARTIALLY** | Remove content-hash short circuit, prune-by-diff, directory prune, ignored-file removal, or newly-skipped-file removal | Check cancellation only before the walk/pass. The cancellation test starts already cancelled and cannot prove mid-pass cooperative stop or ownership-safe shutdown. |
| 9. Embed worker | **PARTIALLY** | Fail fingerprint reconciliation, retry one poison forever, ignore the normal indexer pulse, or poison transient 5xx | Stop after an all-poison capped page; accept a tiny finite vector; lose health-observation ordering; ignore cancellation during an in-flight request. |
| 10. HTTP/MCP wire | **PARTIALLY** | Remove main HTTP error shaping, omit required MCP tools/schema, break normal rendering | Drop fields from the actual MCP HTTP request or mishandle daemon 4xx/invalid-2xx responses. The golden stub ignores bodies and returns only success payloads. |

### Skip-removal regression retrospective

The 2026-08-14 stale-chunk bug needed exactly the test now present as
`a_previously_indexed_file_that_becomes_binary_is_removed`
(`crates/lore/tests/daemon_index.rs:169`): index text, rewrite the same path to
a skipped class, rescan, and assert both `summary.removed == 1` and absence from
the store. That test would have failed before `3e791d2` and is meaningful
regression coverage now. All `FileChunks::Skipped` reasons currently converge
on the same removal branch, so there is no separate known equivalent gap in the
indexer. Input coverage still exercises only binary conversion; table cases for
oversize, invalid UTF-8, and machine-text conversion would guard a future split
of those paths cheaply.

## Tautologies and weak oracles

- `sanitize_caps_term_count` compares against the implementation's own
  `MAX_TERMS`; it proves a cap exists, not that the public policy remains 64.
- The RRF exact-score assertion imports `RRF_K`, and the exact authority tests
  import `AUTHORITY_*`. The independent ordering assertions are useful; the
  constant-equality assertions cannot catch tuning drift.
- Re-chunking the same fixture twice is a valid determinism test, but not an
  independent oracle for the ID formula. Omitting path or anchor inputs can
  survive because fixtures lack the necessary collision pair.
- `responses_are_reordered_by_index_not_by_arrival` computes expected vectors
  with the same `stub_vector` used by the server. This is acceptable for its
  intended pairing invariant because the response is reversed and inputs map
  to distinct vectors, but it establishes nothing about real-model geometry.
- `chunks_cover_almost_all_content` deliberately accepts 5% significant-byte
  loss. It is a useful smoke alarm, not an oracle for "no prose dropped"; a
  short parent rule can disappear while the ratio remains green.

## Snapshot quality

- The chunk snapshots are compact and reviewable: anchors, exact spans, byte
  sizes, first lines, and vault metadata are visible without freezing whole
  fixture bodies. They meaningfully pin structural behavior.
- The MCP `tools/list` snapshot is a strong agent-facing schema contract, and
  the search/status snapshots are readable enough for human review.
- Snapshot breadth is the issue, not snapshot form. No chunk snapshot contains
  C# overloads, repeated Markdown headings, BOM+CRLF frontmatter, a short parent
  introduction, or non-ASCII paths. MCP snapshots freeze output rendering but
  not the request sent to the daemon or daemon-error decoding.

## Timing audit

- Poll loops in `daemon_lifecycle.rs`, `daemon_watch.rs`, and
  `embed_support::until` have explicit 30s/10s deadlines and assert the awaited
  state; they do not pass vacuously merely because work never ran.
- `readers_racing_a_writer_never_observe_a_partial_record` explicitly asserts
  that readers executed, which avoids the usual vacuous race-test failure.
- The main weaknesses are the unbounded HTTP awaits before lifecycle cleanup,
  the fixed `sleep(DEBOUNCE)` used as watcher readiness, the five-second
  negative watcher sleep, and wall-clock `Retry-After` bounds. Paused virtual
  time is not used anywhere.
- No test controls the interleaving for simultaneous admission, shutdown versus
  blocking index work, stale health writes, or notify-during-drain. Scheduler
  luck is therefore avoided, but so are the races themselves.

## Negative-path coverage count

Classification is by test function, not raw `assert!` macro. Each of the 217
tests was assigned once according to its primary oracle; "negative" includes
rejection, fallback, cancellation, corrupt input, or no-op/error behavior.
Mixed scenario tests were assigned to their dominant purpose.

| Area / targets | Total | Negative-path | Happy/structural | Assessment |
|---|---:|---:|---:|---|
| Chunking + chunk IDs/types | 33 | 5 | 28 | Strong exact-span breadth; weak Windows encodings, collision pairs, and deliberate small-loss cases. |
| CLI rendering/config client | 14 | 4 | 10 | Friendly discovery/version errors covered; no live HTTP error decoding. |
| Config | 6 | 2 | 4 | Unknown fields and malformed TOML covered. |
| Handshake + daemon lifecycle | 15 | 7 | 8 | Many record states, but no adversarial ownership interleaving. |
| HTTP handlers | 18 | 7 | 11 | Good client-rejection shaping; no injected store/channel/internal failure. |
| Indexer + queue + watcher + path + ranking units | 38 | 12 | 26 | Good synchronous file churn; very thin asynchronous watcher/scheduler coverage. |
| Embedding client/worker/search/text/health | 45 | 14 | 31 | Good HTTP classification and simple recovery; weak bounded-backlog and concurrency boundaries. |
| Store/FTS/vector | 23 | 4 | 19 | Excellent lifecycle/integrity happy churn; no transaction fault injection and weak pushdown proof. |
| `lore-core` | 1 | 0 | 1 | Data-dir override only; discovery behavior is mostly exercised elsewhere. |
| MCP unit + golden | 24 | 6 | 18 | Strong presentation/schema checks; weak request and protocol-failure checks. |

The three worst-covered production failure paths are:

1. ownership during simultaneous start, missing/corrupt discovery metadata, and
   shutdown with residual work — zero controlled interleaving tests;
2. watcher arm failure/backend error/overflow/overlapping roots — zero tests;
3. embed worker poison-cap pagination, stale health observations, and in-flight
   cancellation — zero boundary/interleaving tests.

## Test-support honesty

- The embedding stub uses a real loopback socket, checks actual JSON inputs,
  can reverse indexed responses, and scripts status/retry behavior. Those are
  honest tests of HTTP and response pairing.
- Its synonym-vector geometry is intentionally artificial but suitable for the
  one claim it supports: a semantic arm can find a lexically invisible result.
  It is not evidence about a real model's ranking quality.
- The stub cannot delay a selected request, hold it behind a barrier, return
  malformed/partial/duplicate-index data, return tiny finite vectors, mix
  dimensions, or wrap a transport failure in a 400 body. Those omissions map
  directly to the untested worker/client races and issue #6.
- The MCP stub is materially weaker: it ignores request bodies, records
  nothing, returns only valid typed success bodies, and has no explicit
  shutdown. It is a rendering fixture attached to a real protocol transport,
  not an end-to-end proxy correctness oracle.
- `daemon_support::Fixture` is honest for synchronous indexing: real temp
  files, real SQLite, real chunking. It intentionally bypasses watcher timing
  and daemon ownership, so conclusions should remain within that boundary.

## Top 10 missing tests (risk × cheapness)

1. **`simultaneous_starters_have_exactly_one_store_owner`** — Launch two
   process-level daemon starters against one empty data directory behind a
   barrier. Assert exactly one acquires the lifetime lock/publishes/opens the
   store, the loser returns an ownership error, and the winner remains healthy.
   Guards D-0003 and Sessions 1–2's highest-risk failure.
2. **`live_owner_survives_deleted_or_corrupt_discovery_record`** — Start A,
   delete then corrupt `daemon.json`, and start B after each mutation. Assert B
   is refused by the ownership primitive and A remains the only writer.
3. **`poisoned_fetch_window_does_not_report_idle`** — Seed `MAX_FETCH + 1`
   missing chunks, poison the oldest `MAX_FETCH`, drain, and assert the final
   good chunk is fetched/stored and `Idle` is impossible before then.
4. **`collapse_preserves_overloads_and_repeated_headings`** — Feed `fuse` two
   same-anchor C# overloads and two repeated-heading Markdown sections with
   distinct IDs/spans. Assert all survive; separately assert explicit siblings
   in one generated `#w` family collapse.
5. **`shifted_live_file_never_expands_unrelated_lines`** — Index/search a
   chunk, insert lines above it while keeping the old range in bounds, expand
   the old ID, and assert stored/stale-safe content rather than the new text at
   the old line number.
6. **`rank_51_cross_arm_agreement_can_win_and_page_refills`** — Through
   `search::execute`, create disjoint top-50 arms plus one shared rank-51 hit,
   then a window-heavy variant. Assert the mathematically winning shared hit is
   returned and collapse does not underfill while unseen eligible hits exist.
7. **`vault_frontmatter_accepts_utf8_bom_and_crlf`** — Prefix a decided vault
   fixture with BOM and use CRLF. Assert parsed status/refs, no YAML body chunk,
   and byte spans relative to original bytes.
8. **`path_prefix_is_unicode_and_windows_case_correct`** — Store
   `données/parser.cs` and `Assets/Scripts/Foo.cs`; query `données/` and an
   alternate-case Windows prefix. Assert both lexical and vector arms return
   the intended rows and no sibling prefix leaks in.
9. **`watch_routes_to_every_containing_project_and_retries_arm`** — With an
   injectable watcher backend, register overlapping roots in both orders, fail
   one arm attempt, then deliver one nested edit. Assert desired watch state is
   visible, retry occurs, and both projects receive coalesced work.
10. **`mcp_forwards_exact_requests_and_surfaces_daemon_protocol_errors`** —
    Record search/expand JSON at the stub and assert every field. Then script a
    JSON 400, plain-text 500, and malformed JSON 200; assert each becomes a
    visible MCP tool error rather than a panic, success, or dropped detail.

## Lower-priority hardening tests

- Queue fairness: continuously requeue project 1 and prove project 2 is served
  within a fixed number of takes.
- Shutdown ownership: hold a controllable blocking index/store operation,
  cancel, and prove ownership metadata/lock remains until the operation exits.
- Health epochs: complete an older failed query after a newer successful probe
  and prove Ready is not overwritten.
- Transaction fault injection: force a late `replace_file_chunks` failure and
  prove file hash, chunks, FTS rows, and vectors all remain at pre-call state.
- Watch callback overflow: exceed the callback-side bound and assert detailed
  events collapse to one rescan request without unbounded retention.
