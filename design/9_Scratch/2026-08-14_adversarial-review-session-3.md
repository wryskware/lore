---
design_status: exploration
last_reviewed: 2026-08-14
---

# Adversarial Review — Session 3: Data-Path Correctness

## Scope and verification

- **Implementation reviewed:** commit `3e791d2`. Repository `HEAD` was
  `31105b3`; `3e791d2..HEAD` changes only the adversarial-review brief, not
  production or test code. The pre-existing modified brief and untracked
  Session 1–2 reports were preserved.
- Read the live bodies of GitHub issues #1–#9 and both prior session reports.
  The findings below are distinct from the known FTS5 AND behavior, embedding
  cold-load flap, transient-400 poisoning, fingerprint `dimensions: 0`, and
  the earlier authority/identity/lifecycle findings.
- `cargo test --workspace --all-targets --quiet`: **217 passed, 0 failed**.
  `cargo test --workspace --all-targets -- --list` independently counted 217
  tests. The suite does not exercise the failures below.
- Spot-checked the C# chunker against three files in the accessible Lexomancy
  Unity project: `AxiomEffectBehaviorCatalog.cs` (generic constraint and
  expression-bodied members), `GameController.cs` (`#region`-heavy large
  class), and generated partial `PlayerInput.cs` (nested structs/interfaces and
  expression-bodied properties). All emitted texts exactly matched their byte
  spans. The first two had no overlapping spans. `PlayerInput.cs` had six
  overlap pairs, all expected oversized `#w` windows; its partial-class header,
  nested structs, and nested interfaces each had one owning chunk at sampled
  lines.

## Findings

### 1. Window collapse also collapses C# overloads and repeated Markdown sections

- **Severity:** major
- **Confidence:** high
- **Binding constraint:** D-0003 — C#/Unity is the flagship target
- **Locations:** `crates/lore/src/daemon/search.rs:214`,
  `crates/lore/src/daemon/search.rs:223`, `crates/lore/src/daemon/search.rs:230`,
  `crates/lore/src/daemon/search.rs:249`, `crates/lore/src/chunk/common.rs:249`
- **Failure scenario A:** A C# file declares `Parse(string)` and `Parse(Stream)`.
  The chunker gives both the structural path `Parser.Parse`; their differing
  text gives them distinct chunk IDs, so the `#d` collision pass does not mark
  either one. Both survive RRF as separate candidates. Window collapse then
  keys only on `(project, path, "code:Parser.Parse")`, so the higher-scoring
  overload suppresses the other. An agent searching for terms common to both
  can never receive both overloads in one response even when the result limit
  has room.
- **Failure scenario B:** Two sibling Markdown sections use the same heading,
  such as two `## Notes` sections with different bodies. They have the same
  `heading_path` but distinct IDs. The same collapse key suppresses one as if
  it were an overlapping window.
- **Cause:** The comment says only `#w` windows are folded, but chunks that
  never contained `#w` are also folded whenever their base structural anchor
  happens to be equal. Overloads and repeated headings are normal distinct
  content, not duplicate views of one span.
- **Required direction:** Collapse only candidates positively identified as
  members of the same generated window family. Preserve unsplit chunks with an
  equal anchor as separate results. Add flagship C# overload and repeated-
  heading tests.

### 2. Five thousand poisoned old rows permanently hide every later embedding candidate

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/embed/worker.rs:66`,
  `crates/lore/src/embed/worker.rs:70`, `crates/lore/src/embed/worker.rs:220`,
  `crates/lore/src/embed/worker.rs:273`, `crates/lore/src/embed/worker.rs:276`,
  `crates/lore/src/embed/worker.rs:289`, `crates/lore/src/store/mod.rs:618`,
  `crates/lore/src/store/mod.rs:625`
- **Failure scenario:** An 82,000-chunk corpus is draining in insertion order.
  Input-specific permanent rejections accumulate in the process-local poison
  set; because one bad input rejects and poisons its whole batch, 5,000 poisoned
  rows do not require 5,000 independently bad documents. After the oldest
  5,000 missing rows are poisoned, `next_batch` computes a fetch larger than
  5,000 but clamps it to `MAX_FETCH = 5_000`. The store returns exactly those
  oldest rows, filtering removes every one, and `next_batch` returns an empty
  vector. `drain` reports `Idle` even though tens of thousands of unpoisoned
  missing rows exist immediately beyond the SQL window. Every future tick
  repeats the same query; the advertised `MAX_POISONED = 10_000` is unreachable
  as a progress bound.
- **User-visible consequence:** The endpoint can be Ready while embedding
  coverage stalls forever at an incomplete count. New chunks receive larger
  rowids and are also starved. This is distinct from issue #6: legitimate
  permanent input rejections are enough; no transient HTTP misclassification
  is required.
- **Required direction:** Exclude poison keys in the store query, page by rowid
  until a non-poison batch is filled, or persist a terminal skipped state so
  rejected rows stop satisfying “missing.” An empty filtered page must not
  mean idle unless the underlying query reached the end.

### 3. `expand` can return unrelated current lines under a stale chunk identity

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/daemon/expand.rs:36`,
  `crates/lore/src/daemon/expand.rs:42`, `crates/lore/src/daemon/expand.rs:55`,
  `crates/lore/src/daemon/expand.rs:59`, `crates/lore/src/daemon/expand.rs:63`,
  `crates/lore-core/src/lib.rs:179`
- **Failure scenario:** Search returns a chunk at lines 200–220. Before the
  watcher reindexes the file, an editor inserts 100 lines above the chunk. The
  stored `line_start = 200` is still within the now-longer file, so the only
  staleness guard passes. `expand` reads current lines around 200 while the
  requested code is now around 300. The response still carries the old chunk's
  path and identity indirectly through the request, but the text can be wholly
  unrelated. `file_lines` is current, which makes the payload look fresher and
  more trustworthy rather than exposing the mismatch.
- **Why the fallback misses it:** Stored text is used only when the file is
  unreadable/non-UTF-8 or the old start is past EOF. A shifted-but-long-enough
  file is the ordinary edit race and satisfies none of those conditions. The
  wire has no content generation, hash, or stale/source flag.
- **Required direction:** Verify that the stored chunk still occurs at its
  recorded span before serving disk text. If it moved, locate the exact stored
  text safely or serve the stored snapshot with an explicit stale indicator.
  Never label unrelated current lines as expansion of the requested hit.

### 4. The fixed candidate pool can omit the mathematically best RRF result and underfill pages

- **Severity:** major
- **Confidence:** high
- **Locations:** `crates/lore/src/daemon/search.rs:43`,
  `crates/lore/src/daemon/search.rs:49`, `crates/lore/src/daemon/search.rs:123`,
  `crates/lore/src/daemon/search.rs:127`, `crates/lore/src/daemon/search.rs:134`,
  `crates/lore/src/daemon/search.rs:175`, `crates/lore/src/daemon/search.rs:181`
- **Failure scenario A:** With the default result limit 20, make the first 50
  lexical candidates and first 50 vector candidates disjoint, then place chunk
  X at rank 51 in both complete lists. X's neutral RRF score would be
  `2 / (60 + 51) = 0.018018`; a rank-1 singleton scores only
  `1 / (60 + 1) = 0.016393`. X is therefore the correct fused rank 1, yet
  neither arm fetches it and it is absent from the response.
- **Failure scenario B:** A large symbol containing a repeated query term can
  occupy the first 50 positions with its overlapping windows. Post-fusion
  collapse keeps one window, but no rank-51 candidate was fetched to refill
  the page, so a request for 20 results can return one despite many distinct
  matching chunks below the window family.
- **Required direction:** Treat per-arm depth as an explicit approximation or
  fetch adaptively until the maximum possible score of unseen candidates
  (including authority weight and collapse refill) cannot cross the current
  result cutoff. At minimum, refill after collapse and test a rank-51
  cross-arm agreement case.

### 5. A UTF-8 BOM disables Markdown frontmatter parsing on the flagship platform

- **Severity:** major
- **Confidence:** high
- **Binding constraint:** D-0004 — `design_status`/D-NNNN awareness is
  first-class in the indexer
- **Locations:** `crates/lore/src/chunk/mod.rs:88`,
  `crates/lore/src/chunk/mod.rs:98`, `crates/lore/src/chunk/markdown.rs:119`,
  `crates/lore/src/chunk/markdown.rs:121`, `crates/lore/src/chunk/markdown.rs:125`
- **Failure scenario:** Save a valid vault document as UTF-8 with BOM, a normal
  Windows-editor encoding: bytes `EF BB BF` followed by `---`,
  `design_status: decided`, and `decision_refs`. UTF-8 decoding succeeds and
  leaves U+FEFF at the start of the first line. `trim_end()` does not remove
  that leading character, so the delimiter comparison fails. The whole file
  is treated as frontmatter-free Markdown: `design_status` becomes `None`,
  file-level refs disappear, and the frontmatter block itself is indexed as
  ordinary text.
- **User-visible consequence:** The same canonical document ranks and renders
  differently solely because a Windows editor emitted a BOM. Search reports
  it as unclassified; body-ref scanning may then partially and misleadingly
  reconstruct citations from the YAML text.
- **Required direction:** Recognize one leading UTF-8 BOM while preserving
  byte spans relative to the original file. Add BOM + CRLF fixtures for every
  supported frontmatter spelling.

### 6. Non-ASCII path prefixes use byte length where SQLite counts characters

- **Severity:** major
- **Confidence:** high
- **Binding constraint:** D-0003 — Windows-native behavior
- **Locations:** `crates/lore/src/store/query.rs:71`,
  `crates/lore/src/store/query.rs:82`, `crates/lore/src/store/query.rs:84`,
  `crates/lore/src/daemon/search.rs:109`
- **Failure scenario A:** A stored path is `données/parser.cs` and the request
  uses `path_prefix: "données/"`. Rust's `prefix.len()` is 9 UTF-8 bytes, but
  SQLite `substr(TEXT, 1, N)` counts Unicode characters. SQLite therefore
  extracts nine characters (`données/p`) and compares them with the eight-
  character prefix; every file below the directory is filtered out.
- **Failure scenario B:** On Windows, store `Assets/Scripts/Foo.cs` and request
  prefix `assets/scripts/`. The same equality is case-sensitive even though
  both spellings identify the same Windows path, so an otherwise valid scoped
  search returns no results.
- **Required direction:** Use character-count semantics for SQLite text and a
  platform-correct normalized path comparison. Tests need accented/non-Latin
  directory names and Windows case variants; ASCII-only path fixtures cannot
  establish this contract.

### 7. Short Markdown parent introductions are deliberately dropped from every index

- **Severity:** minor
- **Confidence:** high
- **Locations:** `crates/lore/src/chunk/markdown.rs:18`,
  `crates/lore/src/chunk/markdown.rs:43`, `crates/lore/src/chunk/markdown.rs:44`,
  `crates/lore/src/chunk/markdown.rs:45`
- **Failure scenario:** A document contains `# Safety`, then the standalone
  sentence `Never upload.`, then `## Details`. Because the parent has a child
  and its introduction is under 24 bytes, no parent chunk is emitted. The
  child's heading metadata retains the word “Safety,” but `Never upload.` is
  in no chunk, no FTS row, and no embedding. Rewording the same rule past 23
  bytes suddenly makes it searchable.
- **Required direction:** Attach a short parent introduction to the first child
  or emit it despite its size. Tiny-chunk merging is already available; a size
  heuristic should not delete human-authored prose.

### 8. Worker and store disagree on which finite vectors are usable, wedging a batch forever

- **Severity:** minor
- **Confidence:** high
- **Locations:** `crates/lore/src/embed/worker.rs:303`,
  `crates/lore/src/embed/worker.rs:310`, `crates/lore/src/embed/worker.rs:328`,
  `crates/lore/src/embed/worker.rs:387`, `crates/lore/src/store/vector.rs:12`,
  `crates/lore/src/store/vector.rs:17`, `crates/lore/src/store/mod.rs:658`
- **Failure scenario:** A broken local endpoint returns a nonzero finite vector
  with norm below `f32::EPSILON`, for example `[1e-10, 0, ...]`. Worker
  validation accepts it because the squared norm is finite and greater than
  zero. Store normalization rejects it because the square root is at most
  `f32::EPSILON`, aborting the entire embedding transaction. The worker does
  not poison the bad candidate or mark health unreachable; the same lowest-
  rowid batch is retried after each idle tick and all later chunks remain
  unembedded.
- **Required direction:** Share one validation/normalization implementation
  between worker and store. Isolate and mark a rejected vector rather than
  repeatedly aborting its whole batch while status remains Ready.

## Smells, debts, and hardening ideas

- Markdown ATX parsing strips every trailing `#` without requiring the
  CommonMark separator space, so `# Learning C#` reports heading `Learning C`.
  It also recognizes four-space-indented `#` lines and `#` lines inside HTML
  blocks as headings. These do not normally remove body text, but they corrupt
  `heading_path`, embedding headers, and the collapse key.
- The discriminator namespace is not escaped. A legitimate Markdown title
  exactly `#w0` or JavaScript private name `#w0` is stripped as window
  bookkeeping by embedding headers and collapse. Fixing Finding 1 should make
  generated window membership explicit rather than infer it from user-authored
  strings.
- `expand` reconstructs disk text with `str::lines().join("\n")`, silently
  normalizing CRLF and dropping the file's terminal newline. That is reasonable
  for a reading view only if the wire says it is a rendering, not an exact file
  slice agents can quote or use for edits.
- A first-line Markdown thematic rule followed later by `---` inside a fenced
  example can be consumed as frontmatter because the minimal parser neither
  validates YAML nor tracks fences. If the frontmatter convention remains
  heuristic, expose parse failure rather than silently deleting the preamble.
- The store transaction and FTS trigger set survived insert/update/delete/file-
  removal churn and the existing FTS5 integrity check. Kept-chunk metadata
  updates are consistent with ID inputs: status/span changes preserve vectors,
  while path/anchor/text changes necessarily change the ID. No partial-commit
  path was found in `replace_file_chunks`.
- Query and stored vectors are both normalized by the store; vector magnitude
  does not affect ranking. Dimension mismatch degrades the whole vector arm as
  documented. The already-tracked `dimensions: 0` fingerprint weakness remains
  the route by which a same-name model swap can silently mix dimensions.
- FTS sanitization remained syntactically safe for empty/operator-only,
  combining-mark, RTL, ZWJ, and prefix inputs by construction. Unicode
  tokenization quality may vary with SQLite's Unicode tables, but no reachable
  syntax error was found.
- CRLF↔LF checkout conversion changes chunk text bytes and therefore chunk IDs.
  This is churn, not a violation of the stated “identical bytes” guarantee.

## Silent data-loss and suppression table

| Input class | What is lost or suppressed | Visible anywhere? |
|---|---|---|
| C# overloads / repeated Markdown heading paths | All but the best result sharing the base anchor | No; response simply omits them (Finding 1). |
| More than 5,000 poisoned low-rowid chunks | Semantic vectors for every later and newly inserted chunk | Aggregate coverage stalls, but worker falsely idles and endpoint can show Ready (Finding 2). |
| File shifted before watcher reindex | Requested chunk context is replaced by unrelated current lines | No stale/hash marker; payload appears current (Finding 3). |
| Cross-arm agreement below candidate rank 50 | A result whose exact RRF score belongs in the returned page | No; scoring is correct only for the truncated lists (Finding 4). |
| UTF-8-BOM vault Markdown | Parsed `design_status` and file-level refs; YAML becomes body text instead | Only indirectly through wrong/missing metadata on hits (Finding 5). |
| Non-ASCII or case-varied Windows path prefix | Every otherwise matching hit below the prefix | No; returns an ordinary empty/partial result set (Finding 6). |
| Markdown parent intro shorter than 24 bytes | The intro prose itself | Nowhere in chunks, FTS, vectors, status, or logs (Finding 7). |
| One tiny nonzero model vector | The whole batch's vectors and progress behind it | Coverage stalls; health remains Ready (Finding 8). |
| CRLF/trailing newline through `expand` | Original line-ending bytes and final newline | No representation/freshness flag. |
| Embedding input beyond the 8 KiB payload ceiling | Suffix from the semantic representation only | Stored text and FTS remain complete; truncation is not surfaced per chunk. |
| Binary/invalid-UTF-8/oversize/machine-text file | Entire file from retrieval | Debug/pass logging only; status has no skipped-file inventory. This is intentional policy, but user-facing absence is silent. |

