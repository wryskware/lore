#!/usr/bin/env python3
"""Score lore `bundle` alone on the held-out tasks -- no agent anywhere.

The comparison that judges the scouter lane: scout_eval measured the trained
model's final-answer citations against gold evidence; this measures what the
mechanical assembler returns for the *same* question under the *same* metric
(grade.same_file, +/-20 line tolerance). If the scout is not demonstrably
better than one daemon-side bundle call, the lane has not earned query-time
model latency.

Two tiers per config, because a bundle is two things: `rendered` scores only
the spans the budget actually rendered (what an agent reads), and `+fr` adds
the further-reading refs (what the retrieval *found* -- the fair ceiling when
comparing against a scout that also only names its evidence).

    python3 bundle_eval.py --tasks ~/bench/atlas/dataset/v1/tasks.jsonl \
        --out work/eval/bundle.atlas.jsonl
"""
from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import threading
import time
import urllib.request

import grade

CONFIGS = [
    {"name": "b4k", "budget_tokens": 4000},
    {"name": "b12k", "budget_tokens": 12000},
    {"name": "b12k-l48", "budget_tokens": 12000, "limit": 48},
]


def daemon_port() -> int:
    handshake = os.path.expanduser("~/.local/share/lore/daemon.json")
    with open(handshake, encoding="utf-8") as handle:
        return json.load(handle)["port"]


def call_bundle(port: int, project: str, query: str, cfg: dict) -> dict:
    payload = {"query": query, "project": project,
               "budget_tokens": cfg["budget_tokens"]}
    if cfg.get("limit"):
        payload["limit"] = cfg["limit"]
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/bundle",
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        return json.loads(resp.read())


def score(task: dict, paths: set, spans: list) -> tuple[float | None,
                                                        float | None]:
    ref_paths = {e["path"] for e in task["evidence"]}
    ref_spans = [(e["path"], e["start_line"], e["end_line"])
                 for e in task["evidence"]
                 if e.get("start_line") and e.get("end_line")]
    matched = {p for p in ref_paths
               if any(grade.same_file(p, c) for c in paths)}
    tol = 20
    hits = sum(1 for p, s, e in ref_spans
               if any(grade.same_file(p, cp) and cs <= e + tol and ce >= s - tol
                      for cp, cs, ce in spans))
    return (round(len(matched) / len(ref_paths), 3) if ref_paths else None,
            round(hits / len(ref_spans), 3) if ref_spans else None)


def run_cell(task: dict, port: int) -> dict:
    row = {"task_id": task["task_id"], "project": task["project"],
           "configs": {}}
    for cfg in CONFIGS:
        started = time.monotonic()
        try:
            resp = call_bundle(port, task["project"], task["question"], cfg)
        except Exception as exc:  # noqa: BLE001
            row["configs"][cfg["name"]] = {"error": str(exc)[:200]}
            continue
        rendered = [(s["path"], s["line_start"], s["line_end"])
                    for s in resp["spans"]]
        fr = [(s["path"], s["line_start"], s["line_end"])
              for s in resp["further_reading"]]
        fr_r, sp_r = score(task, {p for p, _, _ in rendered}, rendered)
        fr_a, sp_a = score(task, {p for p, _, _ in rendered + fr},
                           rendered + fr)
        row["configs"][cfg["name"]] = {
            "verdict": resp["verdict"],
            "n_rendered": len(rendered), "n_fr": len(fr),
            "file_recall_rendered": fr_r, "span_hit_rendered": sp_r,
            "file_recall_fr": fr_a, "span_hit_fr": sp_a,
            "wall_s": round(time.monotonic() - started, 2),
            "rendered": [f"{p}:{s}-{e}" for p, s, e in rendered],
            "further_reading": [f"{p}:{s}-{e}" for p, s, e in fr],
        }
    return row


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--tasks", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--concurrency", type=int, default=8)
    args = ap.parse_args()

    port = args.port or daemon_port()
    tasks = [json.loads(line) for line in open(os.path.expanduser(args.tasks))]
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)

    # Refuse to run against a dead daemon rather than recording 47x3 errors.
    try:
        call_bundle(port, tasks[0]["project"], "preflight", CONFIGS[0])
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"preflight bundle call failed -- daemon down? "
                         f"{str(exc)[:200]}")

    lock = threading.Lock()
    rows = []

    def one(task):
        row = run_cell(task, port)
        with lock:
            rows.append(row)
            print(f"[{len(rows)}/{len(tasks)}] {row['task_id']}", flush=True)
        return row

    with concurrent.futures.ThreadPoolExecutor(args.concurrency) as pool:
        list(pool.map(one, tasks))

    rows.sort(key=lambda r: r["task_id"])
    with open(args.out, "w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, ensure_ascii=False) + "\n")

    for cfg in CONFIGS:
        name = cfg["name"]
        cells = [r["configs"][name] for r in rows
                 if "error" not in r["configs"][name]]
        errs = len(rows) - len(cells)
        if not cells:
            print(f"{name}: all {errs} cells errored")
            continue

        def avg(key):
            vals = [c[key] for c in cells if c[key] is not None]
            return sum(vals) / len(vals) if vals else 0.0

        print(f"{name}: rendered recall {avg('file_recall_rendered'):.3f} "
              f"span {avg('span_hit_rendered'):.3f} | +fr recall "
              f"{avg('file_recall_fr'):.3f} span {avg('span_hit_fr'):.3f} | "
              f"spans {avg('n_rendered'):.1f}+{avg('n_fr'):.1f}fr, "
              f"wall {avg('wall_s'):.2f}s"
              + (f", {errs} errors" if errs else ""))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
