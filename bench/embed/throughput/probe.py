"""Throughput probe for an OpenAI-compatible /v1/embeddings server.

Answers one question: how fast can this server turn lore's real chunks into
vectors? It replays a fixture of actual chunk text (see export-fixture.py) as
batched POSTs at a fixed in-flight concurrency, and reports chunks/s, tokens/s
(server-reported prompt_tokens, not an estimate) and request latency.

    python probe.py --url http://127.0.0.1:8080 --model Qwen/Qwen3-Embedding-0.6B \
        --fixture fixture.jsonl --label tei --batch 32 --concurrency 1,2,4,8

Stdlib only on purpose: it has to run unchanged next to whichever server is
under test, without polluting that server's venv.

Note this measures the *server*, not lore's drain: no chunking, no SQLite
writes, no daemon pacing. That is the point — it isolates the embedding
backend so TEI / vLLM / llama-server are comparable on one GPU.
"""

import argparse
import json
import queue
import statistics
import subprocess
import threading
import time
import urllib.error
import urllib.request

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def post_embeddings(url, model, texts, timeout):
    payload = json.dumps({"model": model, "input": texts}).encode("utf-8")
    req = urllib.request.Request(
        url.rstrip("/") + "/v1/embeddings",
        payload,
        {"Content-Type": "application/json"},
    )
    with OPENER.open(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def gpu_sampler(stop_evt, out, interval=1.0):
    """Poll whole-GPU VRAM while the run is in flight. Best effort."""
    while not stop_evt.is_set():
        try:
            raw = subprocess.run(
                ["nvidia-smi", "--query-gpu=memory.used,utilization.gpu",
                 "--format=csv,noheader,nounits"],
                capture_output=True, text=True, timeout=5,
            ).stdout.strip().splitlines()[0]
            mib, util = (int(x) for x in raw.split(","))
            out.append((mib, util))
        except Exception:
            pass
        stop_evt.wait(interval)


def run_pass(url, model, batches, concurrency, timeout):
    work = queue.Queue()
    for b in batches:
        work.put(b)

    latencies, tokens, errors = [], [], []
    lock = threading.Lock()

    def worker():
        while True:
            try:
                batch = work.get_nowait()
            except queue.Empty:
                return
            t0 = time.perf_counter()
            try:
                body = post_embeddings(url, model, batch, timeout)
                dt = time.perf_counter() - t0
                n_tok = (body.get("usage") or {}).get("prompt_tokens")
                n_vec = len(body.get("data") or [])
                dims = len(body["data"][0]["embedding"]) if n_vec else 0
                with lock:
                    latencies.append(dt)
                    if n_tok:
                        tokens.append(n_tok)
                    if n_vec != len(batch):
                        errors.append(f"returned {n_vec} vectors for {len(batch)} inputs")
                    globals()["_dims"] = dims
            except urllib.error.HTTPError as e:
                with lock:
                    errors.append(f"HTTP {e.code}: {e.read()[:300].decode('utf-8', 'ignore')}")
            except Exception as e:  # noqa: BLE001 — surface anything the server does
                with lock:
                    errors.append(f"{type(e).__name__}: {e}")

    vram = []
    stop_evt = threading.Event()
    sampler = threading.Thread(target=gpu_sampler, args=(stop_evt, vram), daemon=True)
    sampler.start()

    threads = [threading.Thread(target=worker) for _ in range(concurrency)]
    t0 = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - t0

    stop_evt.set()
    sampler.join(timeout=3)
    return wall, latencies, tokens, errors, vram


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True, help="server base URL, e.g. http://127.0.0.1:8080")
    ap.add_argument("--model", required=True, help="model id sent in the request body")
    ap.add_argument("--fixture", required=True)
    ap.add_argument("--label", required=True, help="server name for the report, e.g. tei")
    ap.add_argument("--batch", type=int, default=32, help="chunks per request")
    ap.add_argument("--concurrency", default="8", help="comma-separated in-flight request counts")
    ap.add_argument("--limit", type=int, default=0, help="cap fixture size (0 = all)")
    ap.add_argument("--warmup", type=int, default=64, help="chunks to send before timing")
    ap.add_argument("--timeout", type=float, default=600.0)
    ap.add_argument("--out", help="append one JSON result object per pass to this file")
    args = ap.parse_args()

    chunks = [json.loads(line)["text"] for line in open(args.fixture, encoding="utf-8")]
    if args.limit:
        chunks = chunks[: args.limit]
    total_bytes = sum(len(c.encode("utf-8")) for c in chunks)
    print(
        f"fixture: {len(chunks)} chunks, {total_bytes / 1e6:.1f} MB, "
        f"mean {total_bytes // len(chunks)} B/chunk"
    )

    if args.warmup:
        print(f"warmup ({args.warmup} chunks)…", flush=True)
        post_embeddings(args.url, args.model, chunks[: args.warmup], args.timeout)

    batches = [chunks[i : i + args.batch] for i in range(0, len(chunks), args.batch)]

    print(f"\n{'conc':>5} {'chunks/s':>9} {'tok/s':>9} {'MB/s':>6} "
          f"{'p50 ms':>8} {'p95 ms':>8} {'wall s':>7} {'VRAM MiB':>9}")
    for conc in [int(c) for c in args.concurrency.split(",")]:
        wall, lat, tok, errors, vram = run_pass(
            args.url, args.model, batches, conc, args.timeout
        )
        if errors:
            print(f"{conc:>5}  {len(errors)} errors; first: {errors[0]}")
            continue
        lat.sort()
        n_tok = sum(tok)
        peak_vram = max((m for m, _ in vram), default=0)
        row = {
            "label": args.label,
            "model": args.model,
            "concurrency": conc,
            "batch": args.batch,
            "chunks": len(chunks),
            "bytes": total_bytes,
            "wall_s": round(wall, 2),
            "chunks_per_s": round(len(chunks) / wall, 1),
            "tokens_per_s": round(n_tok / wall) if n_tok else None,
            "prompt_tokens": n_tok or None,
            "mb_per_s": round(total_bytes / 1e6 / wall, 2),
            "p50_ms": round(statistics.median(lat) * 1000, 1),
            "p95_ms": round(lat[int(len(lat) * 0.95)] * 1000, 1),
            "peak_gpu_mib": peak_vram,
            "dims": globals().get("_dims"),
        }
        print(f"{conc:>5} {row['chunks_per_s']:>9} "
              f"{(row['tokens_per_s'] or '-'):>9} {row['mb_per_s']:>6} "
              f"{row['p50_ms']:>8} {row['p95_ms']:>8} {row['wall_s']:>7} {peak_vram:>9}")
        if args.out:
            with open(args.out, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(row) + "\n")


if __name__ == "__main__":
    main()
