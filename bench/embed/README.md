# Embedding-model retrieval bench

Decides whether swapping lore's embedding model (currently `nomic-embed-text`
via Ollama — a convenience default, never a canon decision) is worth it.
Candidates and calibration expectations come from
`design/7_Research/raw/C_embeddings.md` (Brief C); this harness produces the
"measure on the target repository" numbers that report asks for.

## Isolation guarantees

Nothing here touches the dogfooding daemon or Ollama:

- Each model run spawns its **own** lore daemon with `LORE_DATA_DIR` pointed
  at `data/<model>/` — own SQLite, own config, own ephemeral port (read from
  its `daemon.json` handshake). `run.ps1` hard-refuses if the data dir or port
  ever resolves to the main daemon's.
- Embeddings are served by a **standalone llama.cpp `llama-server`**
  (CUDA build, downloaded by `setup.ps1` into `tools/`), not Ollama. Chosen
  per Brief C: explicit `--pooling`, real batching, `/metrics` token counters,
  and no hidden model templating.
- Corpora are registered read-only (watchers only read); registering the same
  roots as the main daemon is safe.

The one shared resource is the GPU: **do not run while the e2e matrix is
running.** `run.ps1` refuses if `opencode` is alive (override with `-Force`).

## Layout

- `models.json` — candidate matrix (GGUF repo/file, dims, pooling, exact
  model-card prefixes; all card facts verified 2026-08-15). `ctx` is the
  server-total context, split across `--parallel 4` slots.
- `corpora.json` — corpus name → root → answer key. lore-bench (Rust + design
  vault), terrarium-bench (TS/WebGPU), lexomancy (C#/Unity, the flagship
  target — 81k chunks, also the realistic throughput test).
- `queries/*.json` — answer keys: queries with verified-relevant paths.
  Kinds: `semantic` (phrased to avoid the target's identifiers — the kind
  that actually differentiates embedders, since BM25 owns lexical matches),
  `lexical`, `symbol`, `design`/`docs`. Regenerate/re-verify if a corpus
  moves off its pin (lexomancy: cs:134).
- `setup.ps1` — downloads llama.cpp (highest CUDA build; Blackwell needs
  ≥ 12.8) and GGUFs (~16 GB total, resumable). Network/disk only.
- `run.ps1` — the bench itself (see header comment for details).
- `score.ps1` — hit@5/10, MRR@10, nDCG@10 (binary), per-kind breakdown, the
  cost table (drain time, chunks/s, exact prompt tokens from llama-server
  `/metrics`, per-process VRAM peak), and daemon-side latency percentiles
  (`/v1/status` `latency` field: `search`/`search_embed` global,
  `search_store:<name>` via `?project=`) → `results/summary.md`.

## Protocol

```powershell
.\setup.ps1                      # once; resumable
.\run.ps1                        # all downloaded models + lexical control
.\run.ps1 -Models lexical,nomic-v1.5,qwen3-4b -Corpora lore-bench   # subset
.\score.ps1                      # latest run per model -> results/summary.md
```

## Reading the results

- The `lexical` arm is the floor: it shows what BM25 alone does. A model
  earns its VRAM/latency by beating it — mostly on `semantic` queries.
- Scores measure the **fused system** (BM25 + vector + RRF + authority), not
  the embedder in isolation — deliberately, since that is what lore ships.
  There is no vector-only search mode; the semantic-kind breakdown is the
  isolation proxy.
- Fingerprinting means every model change forces a full re-embed; the cost
  table is therefore also the migration cost of actually switching.
- Fairness caveats: jina-code's nl2code prefixes are code-flavored while the
  corpora include markdown; nomic-v2-moe truncates at 512 tokens/chunk.
  Qwen3 runs a code+docs-tailored instruction (card-sanctioned, +1–5%).
