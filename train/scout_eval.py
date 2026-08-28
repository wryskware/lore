#!/usr/bin/env python3
"""Evaluate a scout checkpoint on held-out tasks, under the trained contract.

The student was trained on SCOUT_SYSTEM_PROMPT + the bare question + the
five-tool schema in common.TOOLS -- opencode's framing was deliberately
replaced at convert time (Decision 3) -- so this runner IS the deployment
scaffold: a plain OpenAI tool loop against a locally served checkpoint.

  * `bundle` / `search` go through a real `lore-mcp` process per cell
    (newline-delimited JSON-RPC over stdio), scoped by LORE_PROJECT and
    pinned to the real daemon by LORE_DATA_DIR -- the same path the teacher's
    recordings came through.
  * `read` / `grep` reproduce the result formats the model was conditioned
    on (sampled from the training rows), and `bash` runs inside a userns
    with the repo bind-mounted read-only and no network.
  * Tool results are elided with convert.elide at the training caps, so the
    model sees the same truncation shapes it was trained with.

Tasks: the SWE-Atlas corpus (bench/rcb/atlas), whose repos were never in
training. Scoring mirrors grade.py: file recall and span hit rate against the
task's gold evidence spans, plus the unresolvable-citation count.

    python3 scout_eval.py --tasks ~/bench/atlas/dataset/v1/tasks.jsonl \
        --repos-root ~/bench/atlas/repos --endpoint http://127.0.0.1:8123/v1 \
        --model glm-scout-v1 --out work/eval/atlas.glm-scout-v1.jsonl
"""
from __future__ import annotations

import argparse
import concurrent.futures
import fnmatch
import json
import os
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request

import common
import convert
import grade

MAX_TOOL_CALLS = 40
MAX_TURNS = 60
BASH_TIMEOUT_S = 30
GREP_MAX_MATCHES = 100
READ_DEFAULT_SPAN = 250


# --------------------------------------------------------------------------- #
# lore-mcp over stdio
# --------------------------------------------------------------------------- #

class LoreMcp:
    """One lore-mcp process, newline-delimited JSON-RPC over stdio."""

    def __init__(self, mcp_bin: str, project_key: str, data_dir: str):
        env = dict(os.environ)
        env["LORE_PROJECT"] = project_key
        env["LORE_DATA_DIR"] = data_dir
        self.proc = subprocess.Popen(
            [mcp_bin], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, env=env, text=True, bufsize=1)
        self._id = 0
        self._rpc("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "scout-eval", "version": "1"}})
        self._notify("notifications/initialized")

    def _send(self, msg: dict):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _rpc(self, method: str, params: dict) -> dict:
        self._id += 1
        self._send({"jsonrpc": "2.0", "id": self._id,
                    "method": method, "params": params})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("lore-mcp closed its stdout")
            msg = json.loads(line)
            if msg.get("id") == self._id:
                if "error" in msg:
                    raise RuntimeError(f"mcp error: {msg['error']}")
                return msg.get("result") or {}

    def _notify(self, method: str):
        self._send({"jsonrpc": "2.0", "method": method})

    def call(self, tool: str, arguments: dict) -> str:
        result = self._rpc("tools/call", {"name": tool, "arguments": arguments})
        parts = result.get("content") or []
        text = "\n".join(p.get("text", "") for p in parts
                         if p.get("type") == "text")
        if result.get("isError"):
            raise RuntimeError(text[:400] or "mcp tool error")
        return text

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.terminate()
        except Exception:
            pass


# --------------------------------------------------------------------------- #
# local tools, in the training result formats
# --------------------------------------------------------------------------- #

def _resolve(repo: str, path: str) -> str | None:
    full = os.path.realpath(os.path.join(repo, path.lstrip("/")))
    if full != repo and not full.startswith(repo + os.sep):
        return None
    return full


def tool_read(repo: str, args: dict) -> str:
    path = str(args.get("path") or "")
    full = _resolve(repo, path)
    if full is None or not os.path.isfile(full):
        return f"<path>{path}</path>\n<type>error</type>\nfile not found"
    with open(full, encoding="utf-8", errors="replace") as handle:
        lines = handle.read().splitlines()
    start = max(1, int(args.get("start") or 1))
    end = int(args.get("end") or 0) or min(len(lines),
                                           start + READ_DEFAULT_SPAN - 1)
    body = "\n".join(f"{n}: {lines[n - 1]}"
                     for n in range(start, min(end, len(lines)) + 1))
    return f"<path>{path}</path>\n<type>file</type>\n<content>\n{body}"


def tool_grep(repo: str, args: dict) -> str:
    pattern = str(args.get("pattern") or "")
    glob = str(args.get("glob") or "")
    try:
        rx = re.compile(pattern)
    except re.error as exc:
        return f"invalid pattern: {exc}"
    hits: list[tuple[str, int, str]] = []
    for dirpath, dirnames, filenames in os.walk(repo):
        dirnames[:] = [d for d in dirnames if d != ".git"]
        for name in filenames:
            rel = os.path.relpath(os.path.join(dirpath, name), repo)
            rel = rel.replace(os.sep, "/")
            if glob and not (fnmatch.fnmatch(rel, glob)
                             or fnmatch.fnmatch(name, glob)):
                continue
            try:
                with open(os.path.join(dirpath, name), encoding="utf-8",
                          errors="replace") as handle:
                    for n, line in enumerate(handle, 1):
                        if rx.search(line):
                            hits.append((rel, n, line.rstrip("\n")))
                            if len(hits) > GREP_MAX_MATCHES:
                                break
            except OSError:
                continue
            if len(hits) > GREP_MAX_MATCHES:
                break
        if len(hits) > GREP_MAX_MATCHES:
            break
    truncated = len(hits) > GREP_MAX_MATCHES
    hits = hits[:GREP_MAX_MATCHES]
    if not hits:
        return "Found 0 matches"
    out = [f"Found {len(hits)}{'+' if truncated else ''} matches"]
    last = None
    for rel, n, line in hits:
        if rel != last:
            out.append(f"{rel}:")
            last = rel
        out.append(f"  Line {n}: {line}")
    return "\n".join(out)


def tool_bash(repo: str, args: dict) -> str:
    """Run inside a user namespace: repo bind-mounted read-only, no network."""
    command = str(args.get("command") or "")
    inner = ("mount --bind \"$1\" \"$1\" && "
             "mount -o remount,bind,ro \"$1\" \"$1\" && "
             "cd \"$1\" && exec timeout {t} bash -c \"$2\"").format(
                 t=BASH_TIMEOUT_S)
    try:
        proc = subprocess.run(
            ["unshare", "-rmn", "bash", "-c", inner, "_", repo, command],
            capture_output=True, text=True, timeout=BASH_TIMEOUT_S + 10)
    except subprocess.TimeoutExpired:
        return "(command timed out)"
    out = (proc.stdout or "") + (proc.stderr or "")
    if proc.returncode != 0:
        out += f"\n(exit {proc.returncode})"
    return out.strip() or "(no output)"


# --------------------------------------------------------------------------- #
# the loop
# --------------------------------------------------------------------------- #

def chat(endpoint: str, model: str, messages: list,
         tools: list | None) -> dict:
    payload = {
        "model": model, "messages": messages,
        "temperature": 0.0, "top_p": 1.0, "max_tokens": 4096,
    }
    if tools:
        payload["tools"] = tools
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        endpoint.rstrip("/") + "/chat/completions", data=body,
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as resp:
        return json.loads(resp.read())


def run_cell(task: dict, args, mcp_bin: str, data_dir: str) -> dict:
    repo = os.path.realpath(os.path.join(args.repos_root, task["project"]))
    messages = [
        {"role": "system", "content": common.SCOUT_SYSTEM_PROMPT},
        {"role": "user", "content": task["question"]},
    ]
    mcp = LoreMcp(mcp_bin, task["project"], data_dir)
    n_calls, tool_counts, first_tool = 0, {}, None
    answer, error = "", None
    forced = False
    started = time.monotonic()

    def approx_chars() -> int:
        return sum(len(m.get("content") or "")
                   + 200 * len(m.get("tool_calls") or []) for m in messages)

    try:
        for _ in range(MAX_TURNS):
            # A grind-happy model can outrun the 32k window before it outruns
            # the tool budget. Force a text-only answer while there is still
            # room for one, and salvage the last narration if it overflows
            # anyway (the server answers 400, not a truncated completion).
            if not forced and approx_chars() > 100_000:
                messages.append({
                    "role": "user",
                    "content": "Context budget exhausted. Answer now with "
                               "what you have, citing path:start-end spans."})
                forced = True
            try:
                resp = chat(args.endpoint, args.model, messages,
                            None if forced else common.TOOLS)
            except urllib.error.HTTPError as exc:
                if exc.code == 400:
                    answer = next(
                        (m["content"] for m in reversed(messages)
                         if m["role"] == "assistant" and m["content"]), "")
                    error = "context_overflow_salvaged"
                    break
                raise
            msg = resp["choices"][0]["message"]
            calls = msg.get("tool_calls") or []
            entry = {"role": "assistant",
                     "content": msg.get("content") or ""}
            if calls:
                entry["tool_calls"] = calls
            messages.append(entry)
            if not calls:
                answer = msg.get("content") or ""
                break
            for call in calls:
                name = call["function"]["name"]
                try:
                    fargs = json.loads(call["function"]["arguments"] or "{}")
                except json.JSONDecodeError:
                    fargs = {}
                n_calls += 1
                tool_counts[name] = tool_counts.get(name, 0) + 1
                first_tool = first_tool or name
                cap = 12000 if name == "bundle" else 4000
                try:
                    if name in ("bundle", "search"):
                        out = mcp.call(name, fargs)
                    elif name == "read":
                        out = tool_read(repo, fargs)
                    elif name == "grep":
                        out = tool_grep(repo, fargs)
                    elif name == "bash":
                        out = tool_bash(repo, fargs)
                    else:
                        out = f"unknown tool {name}"
                except Exception as exc:  # noqa: BLE001
                    out = f"tool error: {str(exc)[:300]}"
                messages.append({"role": "tool",
                                 "tool_call_id": call["id"],
                                 "content": convert.elide(out, cap)})
            if n_calls >= MAX_TOOL_CALLS:
                messages.append({
                    "role": "user",
                    "content": "Tool budget exhausted. Answer now with what "
                               "you have, citing path:start-end spans."})
    except Exception as exc:  # noqa: BLE001
        error = str(exc)[:400]
    finally:
        mcp.close()
    wall = round(time.monotonic() - started, 1)

    # ---- grade against the gold evidence ---------------------------------- #
    ref_paths = {e["path"] for e in task["evidence"]}
    ref_spans = [(e["path"], e["start_line"], e["end_line"])
                 for e in task["evidence"]
                 if e.get("start_line") and e.get("end_line")]
    cand_paths, cand_spans = grade.citations(answer)
    matched = {p for p in ref_paths
               if any(grade.same_file(p, c) for c in cand_paths)}
    tol = 20
    hits = sum(1 for p, s, e in ref_spans
               if any(grade.same_file(p, cp) and cs <= e + tol and ce >= s - tol
                      for cp, cs, ce in cand_spans))
    unresolvable = [p for p in sorted(cand_paths)
                    if _resolve(repo, p) is None
                    or not os.path.isfile(_resolve(repo, p))]
    return {
        "task_id": task["task_id"], "project": task["project"],
        "model": args.model, "answer": answer, "error": error,
        "wall_s": wall, "n_tool_calls": n_calls,
        "tool_counts": tool_counts, "first_tool": first_tool,
        "answered": bool(answer.strip()),
        "file_recall": round(len(matched) / len(ref_paths), 3)
                       if ref_paths else None,
        "span_hit_rate": round(hits / len(ref_spans), 3)
                         if ref_spans else None,
        "ref_files": len(ref_paths), "ref_spans": len(ref_spans),
        "cand_files": len(cand_paths), "cand_spans": len(cand_spans),
        "unresolvable": unresolvable[:5],
        "n_unresolvable": len(unresolvable),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--tasks", required=True)
    ap.add_argument("--repos-root", required=True)
    ap.add_argument("--endpoint", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--mcp-bin", default=os.path.expanduser(
        "~/lore/target/release/lore-mcp"))
    ap.add_argument("--data-dir", default=os.path.expanduser(
        "~/.local/share/lore"))
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--concurrency", type=int, default=6)
    args = ap.parse_args()

    tasks = [json.loads(line) for line in open(os.path.expanduser(args.tasks))]
    if args.limit:
        tasks = tasks[:args.limit]
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)

    lock = threading.Lock()
    rows = []

    def one(task):
        row = run_cell(task, args, os.path.expanduser(args.mcp_bin),
                       os.path.expanduser(args.data_dir))
        with lock:
            rows.append(row)
            fr = row["file_recall"]
            print(f"[{len(rows)}/{len(tasks)}] {row['task_id']:<36} "
                  f"calls={row['n_tool_calls']:<3} recall="
                  f"{'-' if fr is None else fr} wall={row['wall_s']}s"
                  + (f"  ERR {row['error'][:60]}" if row["error"] else ""),
                  flush=True)
        return row

    with concurrent.futures.ThreadPoolExecutor(args.concurrency) as pool:
        list(pool.map(one, tasks))

    rows.sort(key=lambda r: r["task_id"])
    with open(args.out, "w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, ensure_ascii=False) + "\n")

    graded = [r for r in rows if r["file_recall"] is not None]
    if graded:
        fr = sum(r["file_recall"] for r in graded) / len(graded)
        sp = [r["span_hit_rate"] for r in graded
              if r["span_hit_rate"] is not None]
        bundle_first = sum(1 for r in rows if r["first_tool"] == "bundle")
        print(f"\n{args.model}: {len(rows)} tasks, file_recall {fr:.3f}, "
              f"span_hit {sum(sp)/len(sp):.3f} over {len(sp)}, "
              f"bundle-first {bundle_first}/{len(rows)}, "
              f"answered {sum(r['answered'] for r in rows)}/{len(rows)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
