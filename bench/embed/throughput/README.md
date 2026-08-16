# Embedding-server throughput probe

Measures how fast a given embedding **server** turns lore's real chunks into
vectors. Complements `../run.ps1`, which measures retrieval quality and a full
daemon drain; this one isolates the backend so different servers are comparable
on one GPU.

Not a canon decision, not a retrieval-quality result. Throughput only.

## What it does

`export-fixture.py` pulls real chunk text out of a bench daemon's SQLite,
clipped to `max_embed_bytes` exactly as the daemon clips it. `probe.py` replays
that fixture as batched `POST /v1/embeddings` at a fixed in-flight concurrency
and reports chunks/s, tokens/s (from the server's own `usage.prompt_tokens`,
not an estimate), latency percentiles and peak whole-GPU VRAM.

`probe.py` is stdlib-only so it can run next to any server without polluting
that server's venv.

## Protocol

Build a fixture, start one server, replay the fixture against it. Run the probe
on the same host as the server so the numbers measure the GPU and not the LAN.

```bash
python export-fixture.py ../data/qwen3-0.6b/lore.db fixture.jsonl -n 4000

# TEI (Docker; needs nvidia-container-toolkit)
docker run -d --gpus all -p 8080:80 -v "$PWD/hf:/data" \
    ghcr.io/huggingface/text-embeddings-inference:86-latest \
    --model-id Qwen/Qwen3-Embedding-0.6B --dtype float16 \
    --max-batch-tokens 32768 --max-client-batch-size 64 --auto-truncate

# vLLM (any venv with vllm installed)
vllm serve Qwen/Qwen3-Embedding-0.6B --runner pooling --dtype float16 \
    --max-model-len 8192 --gpu-memory-utilization 0.85 --port 8000

python3 probe.py --url http://127.0.0.1:8080 --model Qwen/Qwen3-Embedding-0.6B \
    --fixture fixture.jsonl --label tei --batch 32 --concurrency 1,2,4,8,16 \
    --out results.jsonl
```

Run one server at a time: vLLM preallocates its KV pool to
`--gpu-memory-utilization` and will starve anything sharing the GPU.

## Run: RTX 3070 8 GB, 2026-08-16

Host: Ubuntu (kernel 7.0), RTX 3070 8 GB, driver 595.84, 16 GB RAM.
Model: `Qwen/Qwen3-Embedding-0.6B`, float16, last-token pooling, 1024 dims.
Fixture: 4000 chunks / 4.0 MB / 1,257,137 prompt tokens, sampled at stride 10
from the qwen3-0.6b bench DB (lexomancy + lore-bench + terrarium-bench),
mean 990 B/chunk. Raw rows: `runs/rtx3070-20260816.jsonl`.

Peak per server (full sweep in the raw file):

| server | model | tok/s | chunks/s | wall s | p50 ms | p95 ms | GPU MiB | plateaus at |
|---|---|---|---|---|---|---|---|---|
| vLLM 0.27.1 | 0.6B fp16 | **32,884** | **104.6** | 38.2 | 613 | 763 | 6459\* | conc 2 |
| TEI 1.9.3 | 0.6B fp16 | 30,122 | 95.8 | 41.7 | 5378 | 6452 | 3079 | conc 2 |
| vLLM 0.27.1 | 4B FP8 (W8A16) | 5,313 | 16.9 | 236.6 | 3839 | 4673 | 7637\* | conc 2 |

\* vLLM's figure is its preallocated KV pool (`--gpu-memory-utilization 0.85`),
not working set. TEI's 3.1 GB is the real number; vLLM's would shrink if told to.

Findings:

- **vLLM is ~9% faster** (32.9k vs 30.1k tok/s). Real but not decisive.
- **Both saturate the GPU at concurrency 2.** Beyond that, throughput is flat
  and latency scales linearly with concurrency — both servers do their own
  continuous batching, so client-side concurrency buys nothing. Client
  concurrency 2 is the whole tuning story on this box.
- **TEI is not batch-starved**: re-running at `--max-batch-tokens 65536`
  (`tei-64k` rows) changed nothing, confirming compute-bound.
- **The two servers produce identical vectors** — cosine 1.0 across 20 matched
  chunks, both L2-normalized, both reporting the same 1,257,137 prompt tokens
  for the fixture. Either is a drop-in for the other; this is a pure
  throughput/ops choice, not a quality one.
- TEI uses **half the VRAM** and starts in ~25 s from a warm cache vs vLLM's
  ~50 s (plus a torch.compile pass on first run).

### 4B on this card

`DCC-BS/Qwen3-Embedding-4B-FP8-Dynamic` (2560 dims), which vLLM loads as
compressed-tensors **W8A16-FP8** — 8-bit weights, the Q8-equivalent data point.
It runs at **5,313 tok/s / 16.9 chunks/s: 6.2x slower than the 0.6B**. Straight
extrapolation, 100k chunks is ~16 min at 0.6B vs ~99 min at 4B.

Read that as a number for *this card*, not a clean model-vs-model comparison:
after 4B weights the 3070 has only a **16,544-token KV cache** left, so part of
the gap is that squeeze rather than model cost. A card that fits 4B comfortably
would close some of it.

**TEI cannot serve a quantized model at all**, so there is no TEI arm here:

- `--dtype` accepts only `float16 | float32`; the binary has no quantization
  flag and no GGUF loader (so the `models.json` Q8_0 GGUFs are not usable).
- compressed-tensors W4A16 fails concretely with `cannot find tensor
  model.layers.0.self_attn.q_proj.weight` — candle wants a dense `.weight`,
  the checkpoint stores `weight_packed` + `weight_scale`.
- The FP8 checkpoint downloads all 4.4 GB and then hangs without logging. Moot
  regardless: the 3070 is SM86 and has no FP8 tensor cores.
- Unquantized 4B is out too — fp16 weights alone are ~8 GB on an 8 GB card.

Quantized checkpoints also tend to drop `1_Pooling/`, so TEI needs an explicit
`--pooling last-token` or it refuses to start.

## Run: A40 48 GB (rented pod), 2026-08-16

The arm the 3070 could not host: TEI 1.9.3 serving **unquantized
`Qwen/Qwen3-Embedding-4B` in float16**, 2560 dims, `last_token` pooling,
`max_client_batch_size` 32, `max_batch_tokens` 16384. ~10.2 GB VRAM at 100%
GPU utilization under load. Same fixture as above. Raw rows:
`runs/runpod-a40-20260816.jsonl`.

| conc | chunks/s | tok/s | wall s | p50 ms | p95 ms |
|---|---|---|---|---|---|
| 1 | 12.8 | 4,033 | 311.7 | 2,666 | 3,290 |
| 2 | 25.0 | 7,862 | 159.9 | 2,601 | 3,527 |
| 4 | 31.7 | 9,957 | 126.3 | 4,070 | 5,035 |
| 8 | 32.9 | 10,340 | 121.6 | 7,631 | 9,375 |
| 16 | **33.0** | **10,386** | 121.0 | 15,177 | 16,804 |

This one was measured **over the internet**, not on the box — the pod's SSH
proxy has no sftp subsystem and ignores exec commands, so the fixture could
not be copied over for a loopback run. The concurrency sweep is what separates
network from GPU, and it does so cleanly: 1→2 scales 1.95x with p50 latency
flat (round-trip-bound, GPU idle), while 4→8→16 is flat at ~33 chunks/s with
the pod pegged at 100% GPU (compute-bound). **Read the plateau, not the
concurrency-1 row** — the low end of this table is measuring the Atlantic.

Note the shape differs from the local arms, which saturate at concurrency 2.
Here concurrency 4+ is needed just to keep the GPU fed across the WAN, which is
what a remote embedder costs in practice.

Against the 3070's 4B FP8 (5,313 tok/s), the A40 at 10,386 tok/s is ~2.0x —
close to the raw compute ratio, and it gets there while running the model
*unquantized* and unconstrained by KV cache.

Setup gotchas, both one-time:

- vLLM's Triton needs a C compiler; without `build-essential` the engine dies
  in `profile_run` with `Failed to find C compiler`.
- Docker GPU access needs `nvidia-container-toolkit` from NVIDIA's own apt
  repo — it is not in the Ubuntu archive.
- vLLM's quantized path JIT-compiles kernels and needs both `libnvrtc-builtins.so.13.0`
  on `LD_LIBRARY_PATH` (it ships inside the venv at
  `site-packages/nvidia/cu13/lib` but is not on the loader path) and the `ninja`
  binary on `PATH` (present in the venv, but only if the venv's `bin` is on
  `PATH` — launching vLLM by absolute path is not enough).

Two probe behaviours worth knowing before trusting a column:

- VRAM sampling shells out to `nvidia-smi` **on the client**, so it is only
  started when the target is loopback. A remote arm reports `null` rather than
  a plausible-looking reading of your own GPU.
- `--metrics-url` records TEI counter deltas as evidence but derives no rate
  from them. `te_embed_inference_duration_sum` bills every input the full
  duration of the batch it rode in, so it over-counts GPU time by roughly the
  batch size and cannot be corrected from the counters alone. Sweep concurrency
  to separate network from GPU instead.

Caveat: the local runs measure the server on an idle GPU over loopback. This excludes
chunking, SQLite writes, daemon pacing, and (if the daemon ran on another
machine) the network. It is an upper bound on what a lore drain could reach,
not a drain measurement.
