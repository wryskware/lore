---
name: lore-search
description: Answer "where is X", "how does Y work", or "what was decided about Z" in this repository using Lore's local index — the `bundle`, `search`, `expand` and `status` MCP tools, or the `lore search` CLI. Use this before grepping or walking the tree, whenever a question is about this codebase or its design history, when Lore returns nothing useful, when documents disagree about what is true, or when the user says "ask lore", "search the index", or "/lore-search".
---

# Using Lore

Lore has already read this repository — code and design vault together — and
indexed it locally. That buys you two things grep cannot give you: hits for a
concept whose *name* you do not know, and design documents that say what was
decided and whether that decision still stands.

It costs almost nothing to ask first. One call routinely replaces a chain of
exploratory greps and directory reads, and it is the cheapest opening move even
when it turns out you also need to read the tree.

There are two ways in, and the default is **`bundle`**:

- **`bundle` — you want the answer.** One call returns a verdict line and the
  verified source spans themselves, read from disk at the moment you asked.
  You may quote and edit straight from it. This is the whole loop in one step:
  **bundle → read the header → read the spans → cite.**
- **`search` — you want to steer.** Ranked pointers with truncated excerpts,
  plus the filters bundle does not expose (path, language, status), for
  narrowing passes and surveys of *how many* places mention something. Its
  loop is longer: **search → read the header lines → expand → read the file →
  cite**, and a hit you have not expanded is never quotable.

Everything below is those loops in detail.

## 1. Ask for the answer first: `bundle`

Call `bundle` with the whole question, in your own words, before your first
`grep`, `glob`, or directory listing, unless you already know the exact file
and line. Write the query as you would brief a colleague — `bundle` runs the
same hybrid retrieval as `search`, then verifies and renders the spans for you.

Read its header before its code, because the header is the honest part:

- `VERDICT: found` — the query's terms are covered by the spans below. `weak`
  and `none` mean they are not, and what follows may be nearest misses rather
  than the answer. The verdict is a claim about term coverage, not about
  correctness: a `found` bundle can still miss the piece you need.
- `NO MATCH FOR:` — query terms nothing in the bundle covers. Those are yours
  to go and find; do not paper over them.
- `FURTHER READING:` — verified paths that did not fit the token budget. Open
  them yourself if the rendered spans fall short.
- `DROPPED` — hits the verifier refused; `stale` means the index pointer no
  longer matches the file on disk.

`budget_tokens` widens or tightens how much source it may carry. If the verdict
is `weak`/`none`, or the spans answer a neighbouring question rather than
yours, that is when you drop to `search` — not before.

## 2. Steer it yourself: `search`

Call `search` (MCP) or run `lore search "<query>"` when you want ranked
pointers rather than a finished answer: filtered retrieval, several narrowing
passes, or a picture of how widely something is referenced.

Query in natural language or with literal identifiers — both are matched, and
the search is hybrid lexical + semantic. Ask for the *thing you want to know*,
not the filename you are guessing at:

- good: `how does the daemon decide a project needs reindexing`
- good: `chunk id derivation`, `SearchRequest`
- weak: `daemon.rs`, `the reindex file`

If the first query comes back thin, **re-query with different words** before
falling back to walking the tree. Semantic search rewards a rephrase, and a
second query is far cheaper than a tree sweep. Inconsistent naming — the code
calls it one thing, the design docs another — is exactly the case where a
literal grep silently loses half the answer and this search does not.

Filters exist; reach for them second, not first:

- `path_prefix` — project-relative, forward slashes (`design/`, `crates/lore/`)
- `language` — lowercase tag (`rust`, `csharp`, `markdown`)
- `status` — `exploration` | `leaning` | `decided` | `deprecated` |
  `unclassified`, filtering what a document *declares* (see §4)
- `limit` — the daemon clamps it

Filtering on the first call is how you miss the hit that would have told you
your framing was wrong. Search broad, then narrow.

## 3. Read the header lines — they are the answer's provenance

A hit looks like this:

```
[1] lore  design/4_Interfaces/4.1_MCP_Surface.md:15-18  score 0.874  [markdown]
    heading: MCP Tool Surface > v0.1 tools
    status: decided  refs: D-0007, D-0008
    project_key: lore  chunk_id: 9f3a1c2b7e4d
- **`search`** - one unified hybrid query. Filters: project, path glob, …
    (excerpt truncated - expand project_key="lore" chunk_id="9f3a1c2b7e4d" …)
```

- `path:line-line` — where it came from. **Paths are relative to the project
  root, which is your working directory.** Join them to it to get the path to
  open; do not resolve them against any other directory.
- `symbol:` / `heading:` — where the hit sits structurally. Use it to judge
  whether this is the definition or an incidental mention.
- `status:` / `refs:` / `authority:` — see the next section.
- `project_key` / `chunk_id` — the handle you pass to `expand`.
- The excerpt is **truncated**, and only the top few hits carry one at all —
  the rest are header-only pointers. Every hit is a pointer, not a source;
  `expand` reads any of them in full.

## 4. Authority: what Lore assigns beats what the document claims

`status:` is what a document *declares about itself*. It is not evidence.

An `authority:` line appears **only when Lore disagreed with that
declaration** — for example `authority: deprecated - 99_Scratch path cap`.
Where it appears, Lore's reading wins:

- `decided` is honoured only when the document cites a decision that is still
  active in the project's ledger. A document that calls itself decided and
  cites nothing has not earned it, and the note says so.
- Scratch and research paths are capped whatever they declare. Research is
  evidence; scratch is thinking out loud. Neither is canon.
- `refs: D-00xx` are provenance — a pointer into the ledger — not authority in
  themselves.

**When sources disagree, prefer the one whose effective authority is `decided`,
and tell the user they disagree.** Do not let confident prose outrank a
validated decision, and do not quietly promote a leaning, a proposal or an
example into a requirement — if the vault only leans, your answer leans.

## 5. Expand before you quote or edit

`expand` takes the hit's `chunk_id` (the printed short id is enough — any
prefix of 8+ hex characters resolves) plus its `project_key`, and returns the
chunk in full with surrounding context; `context_lines` widens it.

**Never quote, attach a line number to, or edit code you have seen only as a
search excerpt.** Excerpts are truncated, and their boundaries are chunk
boundaries, not meaning boundaries.

Bundle spans are the exception, and the only one: they were read from the file
on disk and mechanically checked, so quoting and editing from a bundle needs no
expand step.

## 6. Verify by reading the hit — not by re-deriving it

This is where the win usually gets thrown away. The measured failure mode is an
agent that receives the right hit and then globs the whole tree anyway to
convince itself, paying full price for an answer it already had.

Verification means **opening what Lore pointed at** and confirming the file
says what the excerpt implied. That is `expand`, or reading the cited file
around the cited lines. It does not mean rediscovering the file by hand.

Go to grep/glob when you have an actual reason:

- an **exhaustive literal sweep** — every occurrence of an exact string, where
  missing one is a bug (renames, deleting a symbol, counting call sites);
- you already know the **exact file**;
- the hits pointed at a region and you now need neighbouring code the index did
  not chunk together;
- the index is unavailable or stale (§7).

Ranked relevance is not exhaustiveness. For "change every caller", search to
understand and grep to enumerate.

## 7. When search comes back empty, stale, or lexical-only

Call `status`. It reports the daemon, the index generation, this project's
file/chunk/embedding coverage, and the embedding endpoint's state.

- **A `lexical-only` header, or a note that embeddings are unavailable** — you
  are getting keyword matches only, and every semantic match is silently
  missing. Say so rather than concluding the concept does not exist, and lean
  on literal identifiers meanwhile.
- **Coverage far below the repo's real size** — ignore rules may be excluding
  authored content. That is the `/lore-ignore` skill's job.
- **Recent work missing** — the index may not have caught up with the edits.

You cannot register a project or force a reindex; that is deliberate. Ask the
user to run `lore add <path>` or `lore index`, and carry on with file tools in
the meantime rather than stalling.

Lore is scoped to **this project only**. Other projects on this machine are not
reachable, so do not ask for them and do not assume a hit came from one.

## 8. Cite what you actually read

Cite as `path:line-line`, from the file you opened — not from a search header
alone. If the only evidence for a claim is a truncated excerpt, expand it or
say the evidence is thin.

If the repository genuinely does not support an answer, say that. A cited guess
is worse than an uncited "not in this repo".

---

Without the MCP tools the same surface is a CLI: `lore search "<query>"
[--path-prefix …] [--language …] [--status …] [--limit …]` prints the same
view, and `lore status` the same health. There is no `lore bundle` — the
one-call evidence bundle is an MCP tool only, so on the CLI the loop is
search → read the file.
