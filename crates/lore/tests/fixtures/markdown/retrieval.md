---
design_status: leaning
last_reviewed: 2026-08-14
decision_refs:
  - D-0004
  - D-0005
---

Preamble text that sits before any heading, citing D-0009 for benchmarks.

# Retrieval

Intro paragraph for the container heading. It is long enough to be worth
indexing on its own, and it mentions D-0004.

## Chunking

### Code

Symbol-level chunks with leading comments attached. No call extraction
per D-0005.

```markdown
# not a heading — this line is inside a fence
## also not a heading
```

### Markdown

Heading-tree leaves carrying `heading_path` provenance.

## Ranking

BM25 and cosine fused with RRF, then a vault-status modifier.

# Open questions

Whether `search` groups by corpus is undecided; see D-0004 and D-0004 again
(the duplicate must be recorded once).
