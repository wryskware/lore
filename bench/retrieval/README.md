# Retrieval bench

Measures **how good lore's search results are**, two ways that answer
different questions and must not be confused:

- **Recall**, against hand-verified answer keys (`queries/*.json`): did the
  known-correct file come back, and at what rank? hit@5, hit@10, MRR@10,
  nDCG@10.
- **Precision**, against a labelled result pool (`judgments/*.json`): of the
  files that *did* come back, how many were worth reading? P@5, P@10, graded
  nDCG, and the noise rate that is their complement.

The keys cannot produce precision on their own — they average 1.2 relevant
paths per query, so they name *the* answer, not every acceptable one. That is
what the judged pool is for. Rationale and metric definitions:
`design/6_Evaluation/2026-08-17_relevance-bench-proposal.md`.

This folder was `bench/embed/` and began as an embedding-model comparison
(D-0012 came out of it). That matrix still works and still lives here — see
§ Model matrix — but it is now one caller of the machinery, not the point of
it.

## Isolation guarantees

Nothing here touches the dogfooding daemon or Ollama:

- A model-matrix run spawns its **own** lore daemon with `LORE_DATA_DIR`
  pointed at `data/<model>/` — own SQLite, own config, own ephemeral port
  (read from its `daemon.json` handshake). `run.ps1` hard-refuses if the data
  dir or port ever resolves to the main daemon's.
- Embeddings are served by a **standalone llama.cpp `llama-server`**
  (CUDA build, downloaded by `setup.ps1` into `tools/`), not Ollama. Chosen
  per Brief C: explicit `--pooling`, real batching, `/metrics` token counters,
  and no hidden model templating.
- Corpora are registered read-only (watchers only read); registering the same
  roots as the main daemon is safe.
- `query.ps1` alone is safe against *any* daemon, including the dogfooding
  one: it only issues searches.

The one shared resource is the GPU: **do not run the matrix while the e2e
matrix is running.** `run.ps1` refuses if `opencode` is alive (override with
`-Force`). The lexical arm and `query.ps1` need no GPU at all.

## Layout

- `queries/*.json` — answer keys: queries with verified-relevant paths.
  Kinds: `semantic` (phrased to avoid the target's identifiers — the kind
  that actually differentiates embedders, since BM25 owns lexical matches),
  `lexical`, `symbol`, `design`/`docs`, `multi-file`. Regenerate/re-verify if
  a corpus moves off its `frozen_at` pin.
- `judgments/*.json` — the labelled pool: `(query id, path, file sha256) ->
  2 | 1 | 0`. Written by `judge.ps1`, committed, and reused by every later
  run. Keying on content hash means a re-pinned corpus invalidates only the
  files that actually changed.
- `corpora.json` — corpus name → root → answer key. lore-bench (Rust + design
  vault), terrarium-bench (TS/WebGPU), lexomancy (C#/Unity, the flagship
  target).
- `query.ps1` — issue a query set at a running daemon, record top-K.
- `judge.ps1` — pool the recorded results and label them (luna via opencode,
  batched; `opencode-judge.jsonc` disables the lore MCP so the judge cannot
  retrieve its own second opinion).
- `score.ps1` — all metrics → `results/summary.md`.
- `run.ps1` / `setup.ps1` / `models.json` — the model matrix (§ below).

## Protocol

```powershell
# score a running daemon (bench or dogfooding — this only searches)
.\query.ps1 -Api http://127.0.0.1:PORT/v1 -OutDir results\20260817-shipped
.\judge.ps1 -DryRun          # how many new labels that would cost
.\judge.ps1                  # label everything unjudged; cached, resumable
.\score.ps1                  # -> results\summary.md
```

`judge.ps1 -MaxItems N` caps a session; re-running picks up where it stopped.
`-Corpora`, `-RunSet` and `-JudgeK` narrow the pool. The judge refuses a
corpus whose git pin has moved (`-Force` overrides); a cm pin (`cs:NNN`) is
reported and trusted rather than interrogated.

## Reading the results

- **Recall and precision fail differently.** hit@10 = 0.9 with P@10 = 0.4
  means lore finds the answer and buries it in noise; the reverse means it is
  tidy and missing things. One number cannot say which.
- **`judged` is the coverage column.** Precision computed over a 3%-labelled
  result set is a sample, not a measurement. Judge more before quoting it.
- **The calibration line is load-bearing.** The judge never sees the answer
  key; `score.ps1` then checks what it said about the key entries, which are
  known 2s. Low agreement invalidates every precision number above it — the
  instrument failed, not the retriever.
- **The `lexical` arm is the floor**: what BM25 alone does. A model earns its
  VRAM and latency by beating it, mostly on `semantic` queries. Measured
  2026-08-17 (see the caveat below): lore-bench 0.92 hit@10, terrarium 0.80,
  lexomancy 0.63 — and lore-bench's number is carried entirely by the
  `lexical`/`symbol`/`docs` kinds, where BM25 scores 1.00 against 0.23 on
  `semantic`.
- Scores measure the **fused system** (BM25 + vector + RRF + authority), not
  the embedder in isolation — deliberately, since that is what lore ships.
  There is no vector-only search mode; the semantic-kind breakdown is the
  isolation proxy.
- Judged pools are **incomplete by construction**: a file no arm ever returned
  has no label, so recall stays bounded by the key while precision is honest
  about what was actually shown.

### Caveat on the recorded model runs (read before comparing arms)

The embedding-model results under `results/2026081[56]-*` and the lexical
floor above **were produced by different binaries**, and the lexomancy corpus
has changed size since (17.9k chunks on 2026-08-17 against the ~81k the
throughput notes recorded — the D-0020 ignore stack landed in between). Cross-
dating those numbers is indicative, not controlled. The 2026-08-15/16 lexical
runs recorded **zero results for all 80 queries** in every run and were not a
floor at all; the arm reproduces correctly on the current binary. Re-run the
matrix before leaning on any margin between an embedder and the floor.

## Model matrix

Still here, still works, still needs the GPU:

```powershell
.\setup.ps1                      # once; resumable. llama.cpp + ~16 GB of GGUFs
.\run.ps1                        # all downloaded models + lexical control
.\run.ps1 -Models lexical -Corpora lore-bench     # subset; lexical needs no GPU
.\score.ps1
```

- `models.json` — candidate matrix (GGUF repo/file, dims, pooling, exact
  model-card prefixes; all card facts verified 2026-08-15). `ctx` is the
  server-total context, split across `--parallel` slots (per-model; 16 for
  the causal models so forward passes fill the 8192-token ubatch, 2-4 for
  the small non-causal nomics).
- The cost table doubles as the **migration cost**: fingerprinting means every
  model change forces a full re-embed.
- Fairness caveats: jina-code's nl2code prefixes are code-flavored while the
  corpora include markdown; nomic-v2-moe truncates at 512 tokens/chunk.
  Qwen3 runs a code+docs-tailored instruction (card-sanctioned, +1–5%).
