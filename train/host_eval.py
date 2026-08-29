#!/usr/bin/env python3
"""The agentic consumption test: a host agent answers held-out tasks.

scout_eval.py measured the scout answering questions *directly*. This measures
the configuration the scout exists for: a frontier-ish host agent answering the
same tasks, with retrieval provided three ways (one arm per invocation):

  * ``direct`` -- the status quo. The host gets the exact five-tool surface the
    scout was trained on (lore ``bundle``/``search`` plus read/grep/bash) and
    does its own retrieval.
  * ``scout``  -- the integration under test. The host gets a ``scout`` tool
    that runs the trained checkpoint's whole loop (scout_eval's scaffold,
    verbatim) and returns its answer with the citations mechanically verified:
    cited spans are re-rendered from the files on disk at answer time, and
    citations that do not resolve are flagged. read/grep/bash remain for the
    host to verify with.
  * ``floor``  -- no retrieval at all: read/grep/bash only.

Host model is z-ai/glm-5.3-flash via OpenRouter (the teacher family -- scoring
is mechanical against gold evidence, so no judge is needed and the family bias
question does not arise). Scoring mirrors scout_eval: file recall and span hit
rate against the task's evidence, plus host/scout token spend kept separate.

    python3 host_eval.py --arm scout \
        --tasks ~/bench/atlas/dataset/v1/tasks.jsonl \
        --repos-root ~/bench/atlas/repos \
        --out work/eval/host.scout.jsonl
"""
from __future__ import annotations

import argparse
import concurrent.futures
import http.client
import json
import os
import threading
import time
import urllib.error
import urllib.request

import common
import convert
import grade
import scout_eval

OPENROUTER_ENDPOINT = "https://openrouter.ai/api/v1"
HOST_MAX_TOOL_CALLS = 40
HOST_CHAR_GUARD = 200_000
SCOUT_CHAR_GUARD = 100_000
SCOUT_RESULT_CAP = 12000  # same elide cap the host's bundle results get
RENDER_MAX_SPANS = 12
RENDER_MAX_LINES = 80

HOST_PROMPT = (
    "You are a code analyst answering a question about this repository using "
    "read-only tools.\n"
    "\n"
    "{guidance}\n"
    "\n"
    "Finish with a direct answer that cites its evidence as "
    "repository-relative `path:start-end` line spans. Never cite a path you "
    "have not seen, and never write an absolute path."
)

GUIDANCE = {
    # The direct arm gets the same steering the scout was trained with, so the
    # comparison is against the status quo played well, not played naively.
    "direct": (
        "Always call `bundle` first: it returns a pre-assembled, mechanically "
        "verified context bundle from the local index, and it is cheaper and "
        "more reliable than exploring blind. Then explore with the remaining "
        "read-only tools until you can answer."),
    "scout": (
        "Always call `scout` first: it dispatches a trained local repository "
        "scout that researches your question and returns an answer whose "
        "cited spans have been re-rendered verbatim from the files on disk "
        "(unresolvable citations are flagged). Verify anything load-bearing "
        "with the other tools, and call `scout` again with a refined question "
        "if the first pass leaves gaps."),
    "floor": (
        "Explore with the tools until you can answer."),
}

SCOUT_TOOL = {"type": "function", "function": {
    "name": "scout",
    "description": "Dispatch a trained local repository scout to research a "
                   "question about this repository. Returns the scout's "
                   "answer plus its cited spans rendered verbatim from the "
                   "files on disk; citations that do not resolve to a real "
                   "file are flagged.",
    "parameters": {"type": "object", "properties": {
        "question": {"type": "string", "description":
                     "A specific, self-contained question about this "
                     "repository."}},
        "required": ["question"]}}}

_LOCAL = {t["function"]["name"]: t for t in common.TOOLS}
ARM_TOOLS = {
    "direct": common.TOOLS,
    "scout": [SCOUT_TOOL, _LOCAL["read"], _LOCAL["grep"], _LOCAL["bash"]],
    "floor": [_LOCAL["read"], _LOCAL["grep"], _LOCAL["bash"]],
}


# --------------------------------------------------------------------------- #
# chat plumbing
# --------------------------------------------------------------------------- #

class Http400(Exception):
    """The server rejected the request outright (context overflow on vLLM)."""


class Usage:
    def __init__(self):
        self.prompt = 0
        self.completion = 0

    def add(self, resp: dict):
        u = resp.get("usage") or {}
        self.prompt += u.get("prompt_tokens") or 0
        self.completion += u.get("completion_tokens") or 0


def make_chat(endpoint: str, model: str, usage: Usage, *,
              api_key: str = "", reasoning: dict | None = None,
              max_tokens: int = 4096):
    """A chat function bound to one endpoint/model, with retry on transient
    upstream failures. HTTP 400 raises Http400 for the caller to salvage."""
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"

    def chat_fn(messages: list, tools: list | None) -> dict:
        payload = {"model": model, "messages": messages,
                   "temperature": 0.0, "top_p": 1.0,
                   "max_tokens": max_tokens}
        if tools:
            payload["tools"] = tools
        if reasoning:
            payload["reasoning"] = reasoning
        body = json.dumps(payload).encode("utf-8")
        last = None
        for attempt in range(4):
            req = urllib.request.Request(
                endpoint.rstrip("/") + "/chat/completions",
                data=body, headers=headers)
            try:
                with urllib.request.urlopen(req, timeout=600) as resp:
                    out = json.loads(resp.read())
                if "choices" not in out or not out["choices"]:
                    # OpenRouter reports some upstream errors in a 200 body.
                    last = RuntimeError(str(out.get("error"))[:300])
                    time.sleep(2 * (attempt + 1))
                    continue
                usage.add(out)
                return out["choices"][0]["message"]
            except urllib.error.HTTPError as exc:
                if exc.code == 400:
                    raise Http400(exc.read()[:300].decode("utf-8", "replace"))
                last = exc
                if exc.code not in (408, 429, 500, 502, 503, 524, 529):
                    raise
            except (urllib.error.URLError, TimeoutError, ConnectionError,
                    http.client.HTTPException) as exc:
                last = exc
            time.sleep(2 * (attempt + 1))
        raise RuntimeError(f"chat failed after retries: {last}")

    return chat_fn


def tool_loop(chat_fn, system: str, question: str, tools: list,
              dispatch: dict, caps: dict, *, max_calls: int,
              char_guard: int) -> dict:
    """The generic loop both the host and the inner scout run: call tools
    until the model answers in text, forcing a text-only answer when the tool
    or context budget runs out. Returns answer + accounting."""
    messages = [{"role": "system", "content": system},
                {"role": "user", "content": question}]
    n_calls, tool_counts, first_tool = 0, {}, None
    answer, error, forced, nudges = "", None, False, 0

    def approx_chars() -> int:
        return sum(len(m.get("content") or "")
                   + 200 * len(m.get("tool_calls") or []) for m in messages)

    for _ in range(scout_eval.MAX_TURNS):
        if not forced and approx_chars() > char_guard:
            messages.append({
                "role": "user",
                "content": "Context budget exhausted. Answer now with what "
                           "you have, citing path:start-end spans."})
            forced = True
        try:
            msg = chat_fn(messages, None if forced else tools)
        except Http400:
            answer = next((m["content"] for m in reversed(messages)
                           if m["role"] == "assistant" and m["content"]), "")
            error = "context_overflow_salvaged"
            break
        calls = msg.get("tool_calls") or []
        entry = {"role": "assistant", "content": msg.get("content") or ""}
        if calls:
            entry["tool_calls"] = calls
        messages.append(entry)
        if not calls:
            answer = msg.get("content") or ""
            # A reasoning-heavy turn can spend the whole completion budget
            # thinking and return neither text nor calls; that is a stall,
            # not an answer.
            if not answer.strip() and nudges < 2:
                nudges += 1
                messages.append({
                    "role": "user",
                    "content": "You returned no answer. Answer now with what "
                               "you have, citing path:start-end spans."})
                continue
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
            fn = dispatch.get(name)
            try:
                out = fn(fargs) if fn else f"unknown tool {name}"
            except Exception as exc:  # noqa: BLE001
                out = f"tool error: {str(exc)[:300]}"
            messages.append({"role": "tool", "tool_call_id": call["id"],
                             "content": convert.elide(
                                 out, caps.get(name, 4000))})
        if n_calls >= max_calls and not forced:
            messages.append({
                "role": "user",
                "content": "Tool budget exhausted. Answer now with what you "
                           "have, citing path:start-end spans."})
            forced = True
    return {"answer": answer, "error": error, "n_calls": n_calls,
            "tool_counts": tool_counts, "first_tool": first_tool}


# --------------------------------------------------------------------------- #
# the scout as a tool: run the trained loop, then verify its citations
# --------------------------------------------------------------------------- #

def verify_citations(repo: str, answer: str) -> tuple[str, int]:
    """Render every resolvable cited span from the files on disk -- the
    bundle discipline applied to the scout's prose -- and flag the rest.
    Returns (rendered block, unresolvable count)."""
    paths, spans = grade.citations(answer)
    rendered, unresolvable, seen = [], [], set()
    for path, start, end in spans:
        if (path, start, end) in seen or len(rendered) >= RENDER_MAX_SPANS:
            continue
        seen.add((path, start, end))
        full = scout_eval._resolve(repo, path)
        if full is None or not os.path.isfile(full):
            unresolvable.append(f"{path}:{start}-{end}")
            continue
        with open(full, encoding="utf-8", errors="replace") as handle:
            lines = handle.read().splitlines()
        stop = min(end, start + RENDER_MAX_LINES - 1, len(lines))
        body = "\n".join(f"{n}: {lines[n - 1]}"
                         for n in range(max(1, start), stop + 1))
        rendered.append(f"{path}:{start}-{end}\n{body}")
    span_paths = {p for p, _, _ in spans}
    for path in sorted(paths - span_paths):
        full = scout_eval._resolve(repo, path)
        if full is None or not os.path.isfile(full):
            unresolvable.append(path)
    out = []
    if rendered:
        out.append("VERIFIED SPANS (rendered from the repository now):\n"
                   + "\n\n".join(rendered))
    if unresolvable:
        out.append("UNRESOLVABLE CITATIONS (these do not exist in the "
                   "repository -- do not trust or repeat them): "
                   + ", ".join(unresolvable))
    return "\n\n".join(out), len(unresolvable)


def make_scout_tool(repo: str, mcp, scout_chat, meta_sink: list):
    dispatch = {
        "bundle": lambda a: mcp.call("bundle", a),
        "search": lambda a: mcp.call("search", a),
        "read": lambda a: scout_eval.tool_read(repo, a),
        "grep": lambda a: scout_eval.tool_grep(repo, a),
        "bash": lambda a: scout_eval.tool_bash(repo, a),
    }
    caps = {"bundle": 12000}

    def scout_fn(args: dict) -> str:
        question = str(args.get("question") or "")
        started = time.monotonic()
        run = tool_loop(scout_chat, common.SCOUT_SYSTEM_PROMPT, question,
                        common.TOOLS, dispatch, caps,
                        max_calls=scout_eval.MAX_TOOL_CALLS,
                        char_guard=SCOUT_CHAR_GUARD)
        verified, n_bad = verify_citations(repo, run["answer"])
        meta_sink.append({
            "question": question, "n_calls": run["n_calls"],
            "tool_counts": run["tool_counts"], "error": run["error"],
            "n_unresolvable": n_bad,
            "wall_s": round(time.monotonic() - started, 1)})
        parts = [f"SCOUT ANSWER:\n{run['answer']}" if run["answer"]
                 else "SCOUT ANSWER: (the scout returned no answer)"]
        if verified:
            parts.append(verified)
        return "\n\n".join(parts)

    return scout_fn


# --------------------------------------------------------------------------- #
# one cell
# --------------------------------------------------------------------------- #

def run_cell(task: dict, args, api_key: str, reasoning: dict | None) -> dict:
    repo = os.path.realpath(os.path.join(args.repos_root, task["project"]))
    host_usage, scout_usage = Usage(), Usage()
    host_chat = make_chat(OPENROUTER_ENDPOINT, args.host_model, host_usage,
                          api_key=api_key, reasoning=reasoning,
                          max_tokens=12288)
    mcp = None
    scout_meta: list = []
    started = time.monotonic()
    try:
        dispatch = {
            "read": lambda a: scout_eval.tool_read(repo, a),
            "grep": lambda a: scout_eval.tool_grep(repo, a),
            "bash": lambda a: scout_eval.tool_bash(repo, a),
        }
        caps = {"bundle": 12000, "scout": SCOUT_RESULT_CAP}
        if args.arm in ("direct", "scout"):
            mcp = scout_eval.LoreMcp(os.path.expanduser(args.mcp_bin),
                                     task["project"],
                                     os.path.expanduser(args.data_dir))
        if args.arm == "direct":
            dispatch["bundle"] = lambda a: mcp.call("bundle", a)
            dispatch["search"] = lambda a: mcp.call("search", a)
        elif args.arm == "scout":
            scout_chat = make_chat(args.scout_endpoint, args.scout_model,
                                   scout_usage, max_tokens=4096)
            dispatch["scout"] = make_scout_tool(repo, mcp, scout_chat,
                                                scout_meta)
        run = tool_loop(
            host_chat, HOST_PROMPT.format(guidance=GUIDANCE[args.arm]),
            task["question"], ARM_TOOLS[args.arm], dispatch, caps,
            max_calls=args.max_calls, char_guard=HOST_CHAR_GUARD)
    except Exception as exc:  # noqa: BLE001
        run = {"answer": "", "error": str(exc)[:400], "n_calls": 0,
               "tool_counts": {}, "first_tool": None}
    finally:
        if mcp:
            mcp.close()
    wall = round(time.monotonic() - started, 1)
    answer = run["answer"]

    # ---- grade against the gold evidence, exactly as scout_eval does ------ #
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
    _rz = scout_eval._resolve
    unresolvable = [p for p in sorted(cand_paths)
                    if _rz(repo, p) is None
                    or not os.path.isfile(_rz(repo, p))]
    return {
        "task_id": task["task_id"], "project": task["project"],
        "arm": args.arm, "host_model": args.host_model,
        "answer": answer, "error": run["error"], "wall_s": wall,
        "n_tool_calls": run["n_calls"], "tool_counts": run["tool_counts"],
        "first_tool": run["first_tool"],
        "host_prompt_tokens": host_usage.prompt,
        "host_completion_tokens": host_usage.completion,
        "scout_prompt_tokens": scout_usage.prompt,
        "scout_completion_tokens": scout_usage.completion,
        "scout_meta": scout_meta,
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


# --------------------------------------------------------------------------- #

def load_api_key() -> str:
    key = os.environ.get("OPENROUTER_API_KEY", "")
    if key:
        return key
    auth = os.path.expanduser("~/.local/share/opencode/auth.json")
    with open(auth, encoding="utf-8") as handle:
        return json.load(handle)["openrouter"]["key"]


def probe_reasoning(api_key: str, model: str) -> dict | None:
    """The teacher ran at effort max through opencode; confirm the plain API
    takes the same knob, degrading to high, then to none."""
    for candidate in ({"effort": "max"}, {"effort": "high"}, None):
        chat = make_chat(OPENROUTER_ENDPOINT, model, Usage(),
                         api_key=api_key, reasoning=candidate, max_tokens=512)
        try:
            chat([{"role": "user", "content": "Say ok."}], None)
            return candidate
        except (Http400, RuntimeError):
            continue
    raise RuntimeError("no reasoning configuration accepted by the endpoint")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--arm", required=True, choices=sorted(ARM_TOOLS))
    ap.add_argument("--tasks", required=True)
    ap.add_argument("--repos-root", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--host-model", default="z-ai/glm-5.3-flash")
    ap.add_argument("--scout-endpoint", default="http://127.0.0.1:8123/v1")
    ap.add_argument("--scout-model", default="glm-scout-v2")
    ap.add_argument("--mcp-bin", default=os.path.expanduser(
        "~/lore/target/release/lore-mcp"))
    ap.add_argument("--data-dir", default=os.path.expanduser(
        "~/.local/share/lore"))
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--max-calls", type=int, default=HOST_MAX_TOOL_CALLS,
                    help="host tool-call budget; the default lets the host "
                         "grind, a small value models a busy host consuming "
                         "the scout mid-task")
    ap.add_argument("--concurrency", type=int, default=12)
    args = ap.parse_args()

    tasks = [json.loads(line) for line in open(os.path.expanduser(args.tasks))]
    if args.limit:
        tasks = tasks[:args.limit]
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    args.repos_root = os.path.expanduser(args.repos_root)

    api_key = load_api_key()
    reasoning = probe_reasoning(api_key, args.host_model)
    print(f"arm={args.arm} host={args.host_model} reasoning={reasoning} "
          f"tasks={len(tasks)}", flush=True)

    lock = threading.Lock()
    rows = []

    def one(task):
        row = run_cell(task, args, api_key, reasoning)
        with lock:
            rows.append(row)
            fr = row["file_recall"]
            print(f"[{len(rows)}/{len(tasks)}] {row['task_id']:<36} "
                  f"calls={row['n_tool_calls']:<3} recall="
                  f"{'-' if fr is None else fr} wall={row['wall_s']}s "
                  f"host_tok={row['host_prompt_tokens'] // 1000}k"
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
        hp = sum(r["host_prompt_tokens"] for r in rows) / len(rows)
        hc = sum(r["host_completion_tokens"] for r in rows) / len(rows)
        print(f"\narm={args.arm}: {len(rows)} tasks, file_recall {fr:.3f}, "
              f"span_hit {sum(sp)/len(sp):.3f} over {len(sp)}, "
              f"answered {sum(r['answered'] for r in rows)}/{len(rows)}, "
              f"host tokens {hp/1000:.0f}k in / {hc/1000:.1f}k out per task")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
