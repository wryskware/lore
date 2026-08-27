#!/usr/bin/env python3
"""Stage 2 -- raw teacher trajectory in, TRL conversational rows out.

Three jobs, in this order:

  1. **Reshape.** opencode's event stream is grouped by `messageID` into
     assistant messages (prose plus zero or more tool calls) each followed by
     its `tool` results. The teacher's own prompt is discarded and replaced by
     the frozen scouter system prompt and the bare question, so harness
     scaffolding never becomes conditioning the student will not see at
     inference.
  2. **Normalise.** Every snapshot root is rewritten out of every string, and a
     trajectory whose *supervised* tokens still carry an absolute path is
     rejected rather than repaired. This is the defect that spoiled 92.8% of
     the SWE-QA-Pro conversion's tool calls; it is cheaper to catch here than
     to notice in the weights.
  3. **Validate.** The output is handed to `validate_dataset.py` -- the same
     validator the SWE-QA-Pro corpus was checked with, not a second copy.

Structurally impossible rows are dropped here with a reason. Quality filtering
is grade.py's job, not this stage's.

    python3 convert.py --batch pilot-01 [--config config.toml] [--no-validate]
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import sys

import common
from common import Config

# --------------------------------------------------------------------------- #
# opencode tool surface -> the scouter's five tools
#
# The teacher's tools are close to the student's but not identical: lore's MCP
# server prefixes its own, and opencode's `read`/`grep` use different argument
# names. Renaming here rather than at training time keeps exactly one spelling
# in the dataset -- the one the student will be served with.
# --------------------------------------------------------------------------- #

FORBIDDEN = "forbidden_tool"


def map_read(args: dict) -> dict:
    out = {"path": args.get("filePath") or args.get("path") or ""}
    offset = args.get("offset")
    limit = args.get("limit")
    if offset:                      # opencode counts lines from 0; we cite from 1
        out["start"] = int(offset) + 1
    if limit:
        out["end"] = out.get("start", 1) + int(limit) - 1
    return out


def map_grep(args: dict) -> dict:
    out = {"pattern": args.get("pattern") or ""}
    glob = args.get("include")
    if not glob:
        # `rstrip` only: a leading slash is left on deliberately. It lets the
        # normaliser rewrite the snapshot root out, and when the root does not
        # match, the absolute-path gate catches it instead of a silently
        # relative-looking `mnt/c/.../**` sailing through.
        scope = (args.get("path") or "").rstrip("/")
        if scope and scope != ".":
            glob = f"{scope}/**"
    if glob:
        out["glob"] = glob
    return out


MAPPERS = {
    "lore_bundle": ("bundle", lambda a: {k: v for k, v in
                                         (("query", a.get("query")),
                                          ("limit", a.get("limit"))) if v}),
    "bundle": ("bundle", lambda a: {k: v for k, v in
                                    (("query", a.get("query")),
                                     ("limit", a.get("limit"))) if v}),
    "lore_search": ("search", lambda a: {k: v for k, v in
                                         (("query", a.get("query")),
                                          ("limit", a.get("limit"))) if v}),
    "search": ("search", lambda a: {k: v for k, v in
                                    (("query", a.get("query")),
                                     ("limit", a.get("limit"))) if v}),
    "grep": ("grep", map_grep),
    "read": ("read", map_read),
    "bash": ("bash", lambda a: {"command": a.get("command") or ""}),
}


def map_call(tool: str, args: dict) -> tuple[str, dict, list[str]]:
    """Return (student tool name, student arguments, argument keys dropped)."""
    if tool not in MAPPERS:
        raise KeyError(tool)
    name, fn = MAPPERS[tool]
    mapped = {k: v for k, v in fn(args or {}).items() if v not in (None, "")}
    consumed = {"query", "limit", "filePath", "path", "offset", "pattern",
                "include", "command"}
    dropped = sorted(k for k in (args or {}) if k not in consumed)
    return name, mapped, dropped


# --------------------------------------------------------------------------- #
# Event stream -> messages
# --------------------------------------------------------------------------- #

def parse_events(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    out = []
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass          # a torn last line is normal on a killed cell
    return out


def group_messages(events: list[dict]) -> list[dict]:
    """opencode parts -> [{"text": str, "calls": [(callID, tool, input, output)]}].

    `messageID` is the grouping key and first-appearance order is the wire
    order, which is what makes parallel tool calls fall out for free: several
    `tool_use` parts sharing one `messageID` are one assistant turn with several
    calls, exactly as the chat template renders them.
    """
    order: list[str] = []
    by_id: dict[str, dict] = {}
    for event in events:
        kind = event.get("type")
        if kind not in ("text", "tool_use"):
            continue
        part = event.get("part") or {}
        mid = part.get("messageID")
        if not mid:
            continue
        if mid not in by_id:
            by_id[mid] = {"text": [], "calls": []}
            order.append(mid)
        if kind == "text":
            by_id[mid]["text"].append(part.get("text") or "")
        else:
            state = part.get("state") or {}
            by_id[mid]["calls"].append({
                "id": part.get("callID") or f"call_{len(by_id[mid]['calls'])}",
                "tool": part.get("tool") or "",
                "input": state.get("input") or {},
                "output": state.get("output") if isinstance(
                    state.get("output"), str) else json.dumps(
                        state.get("output"), ensure_ascii=False),
                "status": state.get("status"),
            })
    return [by_id[mid] for mid in order]


def elide(text: str, cap: int) -> str:
    """Cap a masked tool result, keeping both ends and saying what was cut.

    Tool results are never supervised, so trimming them costs no training
    signal -- but it does change the context the next supervised turn was
    actually produced from, which is why the marker is explicit and the cap is
    a configured knob rather than a constant.
    """
    if cap <= 0 or len(text) <= cap:
        return text
    head = cap * 2 // 3
    tail = cap - head
    return (text[:head] + f"\n... [{len(text) - cap} characters elided] ...\n"
            + text[-tail:])


# --------------------------------------------------------------------------- #
# One trajectory
# --------------------------------------------------------------------------- #

def convert_one(cfg: Config, meta: dict, events: list[dict],
                roots: list[str]) -> tuple[dict | None, list[str]]:
    """Return (row, reasons). `row is None` means the trajectory was dropped."""
    emit = cfg["emit"]
    drop_tools = set(emit["drop_tools"])
    reasons: list[str] = []

    groups = group_messages(events)
    if not groups:
        return None, ["no_events"]

    messages: list[dict] = [
        {"role": "system", "content": common.SCOUT_SYSTEM_PROMPT},
        {"role": "user", "content": meta["question"]},
    ]
    supervised_blobs: list[str] = []      # what the model is trained to emit
    masked_blobs: list[str] = []          # conditioning only
    n_calls = 0
    dropped_args: collections.Counter = collections.Counter()
    dropped_calls = 0
    first_tool = None
    norm_hits = 0

    for group in groups:
        calls, results = [], []
        for call in group["calls"]:
            if call["tool"] in drop_tools:
                dropped_calls += 1
                continue
            try:
                name, args, dropped = map_call(call["tool"], call["input"])
            except KeyError:
                return None, [f"{FORBIDDEN}:{call['tool']}"]
            for key in dropped:
                dropped_args[f"{call['tool']}.{key}"] += 1

            args_json, hits = common.normalize_paths(
                json.dumps(args, ensure_ascii=False, sort_keys=True), roots)
            norm_hits += hits
            supervised_blobs.append(args_json)
            first_tool = first_tool or name
            n_calls += 1
            calls.append({"id": call["id"], "type": "function",
                          "function": {"name": name, "arguments": args_json}})

            cap = (emit["max_bundle_chars"] if name == "bundle"
                   else emit["max_tool_chars"])
            output, hits = common.normalize_paths(call["output"] or "", roots)
            norm_hits += hits
            output = elide(output, cap)
            masked_blobs.append(output)
            results.append({"role": "tool", "tool_call_id": call["id"],
                            "name": name, "content": output})

        content, hits = common.normalize_paths("".join(group["text"]).strip(), roots)
        norm_hits += hits
        if not content and not calls:
            continue                       # an empty step is not a turn
        supervised_blobs.append(content)
        messages.append({"role": "assistant", "content": content,
                         **({"tool_calls": calls} if calls else {})})
        messages.extend(results)

    # ---- structural gates -------------------------------------------------- #
    if n_calls == 0:
        reasons.append("no_tool_calls")
    if messages[-1]["role"] != "assistant" or messages[-1].get("tool_calls"):
        reasons.append("ends_on_tool_call")
    elif not messages[-1]["content"].strip():
        reasons.append("empty_answer")
    if first_tool and first_tool != "bundle":
        reasons.append(f"bundle_not_first:{first_tool}")
    if n_calls > emit["max_tool_calls"]:
        reasons.append(f"too_many_tool_calls:{n_calls}")

    # A `bundle` query that is the question pasted back verbatim is rejected for
    # two reasons that happen to coincide. It demonstrates no query formulation,
    # which is the single behaviour this corpus most exists to teach; and it puts
    # the user's own words inside a supervised tool call, which the shared
    # validator flags as the user turn leaking into the loss. The threshold is
    # the validator's own -- an 80-character prefix -- so the two can never
    # disagree about the same row.
    qhead = (meta.get("question") or "").strip()[:80]
    if qhead and not emit.get("allow_question_echo") and any(
            qhead in blob for blob in supervised_blobs):
        reasons.append("question_echoed_verbatim")

    leaks = sorted({leak for blob in supervised_blobs
                    for leak in common.absolute_leaks(blob)})
    if leaks:
        reasons.append("abs_path_leak:" + ",".join(leaks[:3]))
    if reasons:
        return None, reasons

    masked_leaks = sum(len(common.absolute_leaks(b)) for b in masked_blobs)
    row_meta = dict(meta)
    row_meta.pop("question", None)          # it is messages[1]; do not duplicate
    row_meta.update({
        "id": f"lore-scout/{meta['qid']}",
        "n_tool_calls": n_calls,
        "n_messages": len(messages),
        "path_rewrites": norm_hits,
        "dropped_tool_calls": dropped_calls,
        "dropped_arg_keys": dict(dropped_args) or None,
        "masked_abs_fragments": masked_leaks,
        "system_prompt_sha12": common.sha12(common.SCOUT_SYSTEM_PROMPT),
        "graded": False,
    })
    return {"meta": row_meta, "tools": common.TOOLS, "messages": messages}, []


# --------------------------------------------------------------------------- #

def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--config", default=None)
    ap.add_argument("--batch", default=None)
    ap.add_argument("--no-validate", action="store_true")
    args = ap.parse_args(argv)

    cfg = common.load_config(args.config)
    batch = args.batch or cfg["batch"]["name"]
    workspace = cfg.get_path("paths", "workspace")
    raw_dir = os.path.join(workspace, "raw", batch)
    if not os.path.isdir(raw_dir):
        raise SystemExit(f"no raw trajectories at {raw_dir}; run generate.py first")

    manifest = common.read_manifest(workspace, batch)
    pins = common.pins_by_repo(manifest)
    snap_root = cfg.get_path("paths", "snapshots")
    # Every snapshot root, not just this row's: a `bash` command in one cell can
    # name a sibling checkout, and a root that is not rewritten is a leak.
    roots = [os.path.join(snap_root, p.snapshot) for p in pins.values()]
    roots.append(snap_root)

    rows, rejects = [], []
    reason_counts: collections.Counter = collections.Counter()
    for qid in sorted(os.listdir(raw_dir)):
        cell = os.path.join(raw_dir, qid)
        meta_path = os.path.join(cell, "meta.json")
        if not os.path.exists(meta_path):
            continue
        with open(meta_path, encoding="utf-8") as handle:
            meta = json.load(handle)
        events = parse_events(os.path.join(cell, "agent.ndjson"))
        row, reasons = convert_one(cfg, meta, events, roots)
        if row is None:
            rejects.append({"qid": qid, "stage": "convert", "reasons": reasons})
            for reason in reasons:
                reason_counts[reason.split(":", 1)[0]] += 1
            print(f"  DROP {qid:<24} {', '.join(reasons)}")
        else:
            rows.append(row)

    out = os.path.join(workspace, "data", f"{batch}.converted.jsonl")
    common.write_jsonl(out, rows)
    rej = os.path.join(workspace, "data", f"{batch}.convert-rejects.jsonl")
    common.write_jsonl(rej, rejects)

    total = len(rows) + len(rejects)
    print(f"\nconverted {len(rows)}/{total} trajectories -> {out}")
    if reason_counts:
        print("dropped:", ", ".join(f"{k}={v}" for k, v in
                                    reason_counts.most_common()))
    if not rows:
        print("nothing to validate")
        return 1

    calls = [r["meta"]["n_tool_calls"] for r in rows]
    rewrites = sum(r["meta"]["path_rewrites"] for r in rows)
    print(f"tool calls: min {min(calls)} median {sorted(calls)[len(calls)//2]} "
          f"max {max(calls)};  {rewrites} snapshot-root rewrites")

    code = 0 if args.no_validate else common.run_validator(cfg, out)
    print(f"\nnext:  python3 grade.py --batch {batch}")
    return code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
