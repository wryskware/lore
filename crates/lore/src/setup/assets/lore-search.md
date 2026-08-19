---
name: lore-search
description: Answer "where is X", "how does Y work", or "what was decided about Z" in this repository using Lore's local index — the `search`, `expand` and `status` MCP tools, or the `lore search` CLI. Use this before grepping or walking the tree, whenever a question is about this codebase or its design history, when Lore returns nothing useful, when documents disagree about what is true, or when the user says "ask lore", "search the index", or "/lore-search".
---

# Using Lore

Lore has already read this repository — code and design vault together — and
indexed it locally. That buys you two things grep cannot give you: hits for a
concept whose *name* you do not know, and design documents that say what was
decided and whether that decision still stands.

It costs almost nothing to ask first. One search routinely replaces a chain of
exploratory greps and directory reads, and it is the cheapest opening move even
when it turns out you also need to read the tree.

The loop is: **search → read the header lines → expand → read the file →
cite.** Everything below is that loop in detail.

## 1. Search first, and search by concept

Call `search` (MCP) or run `lore search "<query>"` before your first `grep`,
`glob`, or directory listing, unless you already know the exact file and line.

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
  `unclassified`, filtering what a document *declares* (see §3)
- `limit` — the daemon clamps it

Filtering on the first call is how you miss the hit that would have told you
your framing was wrong. Search broad, then narrow.

## 2. Read the header lines — they are the answer's provenance

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
- The excerpt is **truncated**. It is a pointer, not a source.

## 3. Authority: what Lore assigns beats what the document claims

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

## 4. Expand before you quote or edit

`expand` takes the hit's `chunk_id` (the printed short id is enough — any
prefix of 8+ hex characters resolves) plus its `project_key`, and returns the
chunk in full with surrounding context; `context_lines` widens it.

**Never quote, attach a line number to, or edit code you have seen only as a
search excerpt.** Excerpts are truncated, and their boundaries are chunk
boundaries, not meaning boundaries.

## 5. Verify by reading the hit — not by re-deriving it

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
- the index is unavailable or stale (§6).

Ranked relevance is not exhaustiveness. For "change every caller", search to
understand and grep to enumerate.

## 6. When search comes back empty, stale, or lexical-only

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

## 7. Cite what you actually read

Cite as `path:line-line`, from the file you opened — not from a search header
alone. If the only evidence for a claim is a truncated excerpt, expand it or
say the evidence is thin.

If the repository genuinely does not support an answer, say that. A cited guess
is worse than an uncited "not in this repo".

---

Without the MCP tools the same surface is a CLI: `lore search "<query>"
[--path-prefix …] [--language …] [--status …] [--limit …]` prints the same
view, and `lore status` the same health.
