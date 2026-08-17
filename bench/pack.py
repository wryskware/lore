"""Reduce each result cell's raw event stream to a compact grading packet.

Deterministic extraction only — no model in the loop. Output:
results/<cell>/packet.md plus results/packets-all.md (everything a grader
thread needs, concatenated), and results/<cell>/retrieval.json for the
retrieval-behaviour numbers. Grading itself happens in grader threads that
receive the answer key + packets inline and never touch the repos.

Two things beyond the answer are extracted, because a round-2 finding made
them load-bearing: most on-arm cells made a single `lore_search` call and then
reverted to grep-and-read. "The agent did not adopt retrieval" and "retrieval
answered badly so the agent gave up" are different failures with different
fixes, and telling them apart needs the search's *results* — which the event
stream carries and this script used to discard.

  - `### lore calls` — every lore tool call with its query AND the hits it
    returned, so a grader can judge whether the response was relevant.
  - `uptake` / `answer overlap` — computed, not judged: did the agent go on to
    open a path retrieval handed it, and did any returned path survive into the
    final answer. A relevant hit set the agent ignored looks nothing like an
    irrelevant one it was right to ignore, and this separates them without
    asking a model to adjudicate reasoning.

Usage:
    python pack.py                      # every cell in results/
    python pack.py --cells 20260817-*   # one round
    python pack.py --cells 20260817-* --batch repo-task
        # ...and write results/batches/<repo>-<task>.md, one bundle per
        # grading thread (both arms of one task together, so the thread reads
        # one key section once). See design/6_Evaluation/grading-protocol.md.
"""

import argparse
import fnmatch
import json
import re
import sys
from pathlib import Path

RESULTS = Path(__file__).parent / "results"
PROMPTS = json.loads((Path(__file__).parent / "prompts.json").read_text(encoding="utf-8"))
TRAIL_INPUT_LIMIT = 160
DIFF_LINE_LIMIT = 400
QUERY_LIMIT = 300
LORE_TOOLS = ("lore_search", "lore_expand", "lore_status")

# `[3] lore-bench  crates/lore/src/chunk/markdown.rs:80-87  score 0.032  [rust]`
HIT_RE = re.compile(
    r"^\[(?P<rank>\d+)\]\s+(?P<project>\S+)\s+(?P<path>[^\s:]+):(?P<start>\d+)-(?P<end>\d+)"
    r"\s+score\s+(?P<score>[\d.]+)"
)
SYMBOL_RE = re.compile(r"^\s+(?:symbol|heading):\s*(?P<name>.+?)\s*$")


def events(events_path: Path):
    """Yield parsed `tool_use` parts in call order."""
    if not events_path.exists():
        return
    for line in events_path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") != "tool_use":
            continue
        part = ev.get("part") or {}
        if part.get("type") == "tool":
            yield part


def tool_trail(parts: list[dict]) -> list[str]:
    trail = []
    for part in parts:
        name = part.get("tool", "?")
        raw = (part.get("state") or {}).get("input")
        arg = json.dumps(raw, ensure_ascii=False) if raw is not None else ""
        if len(arg) > TRAIL_INPUT_LIMIT:
            arg = arg[:TRAIL_INPUT_LIMIT] + "…"
        trail.append(f"{name} {arg}".rstrip())
    return trail


def parse_hits(output: str) -> list[dict]:
    """Pull (path, span, score, symbol) out of a rendered lore search result."""
    hits, current = [], None
    for line in (output or "").splitlines():
        m = HIT_RE.match(line)
        if m:
            current = {
                "rank": int(m.group("rank")),
                "path": m.group("path").replace("\\", "/"),
                "span": f"{m.group('start')}-{m.group('end')}",
                "score": float(m.group("score")),
                "symbol": None,
            }
            hits.append(current)
            continue
        if current is not None and current["symbol"] is None:
            s = SYMBOL_RE.match(line)
            if s:
                current["symbol"] = s.group("name")
    return hits


def later_tool_text(parts: list[dict], after: int) -> str:
    """Every tool input the agent issued after call index `after`, as one blob."""
    blob = []
    for part in parts[after + 1:]:
        raw = (part.get("state") or {}).get("input")
        if raw is not None:
            blob.append(json.dumps(raw, ensure_ascii=False))
    return "\n".join(blob).replace("\\\\", "/").replace("\\", "/")


def retrieval_report(parts: list[dict], answer: str) -> tuple[list[str], dict]:
    """The `### lore calls` section plus its machine-readable twin.

    `uptake` and `answer_overlap` are string containment over the returned
    paths, deliberately: a path is a long, distinctive token, so a later
    `read` of it or a citation of it in the answer is unambiguous, and
    nothing here needs a model's opinion.
    """
    answer_norm = (answer or "").replace("\\", "/")
    calls, lines = [], []

    for idx, part in enumerate(parts):
        tool = part.get("tool", "")
        if tool not in LORE_TOOLS:
            continue
        state = part.get("state") or {}
        raw_in = state.get("input") or {}
        output = state.get("output") or ""
        hits = parse_hits(output) if tool == "lore_search" else []
        paths = list(dict.fromkeys(h["path"] for h in hits))

        after = later_tool_text(parts, idx)
        opened = [p for p in paths if p in after]
        cited = [p for p in paths if p in answer_norm]

        query = raw_in.get("query") or json.dumps(raw_in, ensure_ascii=False)
        if len(query) > QUERY_LIMIT:
            query = query[:QUERY_LIMIT] + "…"

        call = {
            "position": idx + 1,
            "of": len(parts),
            "tool": tool,
            "query": query,
            "args": {k: v for k, v in raw_in.items() if k != "query"},
            "hit_count": len(hits),
            "paths": paths,
            "opened_after": opened,
            "cited_in_answer": cited,
            "top_score": hits[0]["score"] if hits else None,
        }
        calls.append(call)

        lines.append(f"{len(calls)}. **{tool}** — call {idx + 1} of {len(parts)}")
        if call["args"]:
            lines.append(f"    args: {json.dumps(call['args'], ensure_ascii=False)}")
        lines.append(f"    query: {query}")
        if tool == "lore_search":
            lines.append(f"    returned {len(hits)} hit(s){' — NO HITS' if not hits else ''}:")
            for h in hits:
                sym = f"  {h['symbol']}" if h["symbol"] else ""
                lines.append(f"      [{h['rank']}] {h['path']}:{h['span']}  score {h['score']}{sym}")
            lines.append(
                f"    uptake: agent later opened {len(opened)}/{len(paths)} returned path(s)"
                + (f" — {', '.join(opened)}" if opened else "")
            )
            lines.append(
                f"    answer overlap: {len(cited)}/{len(paths)} returned path(s) appear in the final answer"
            )
        else:
            head = output.strip().splitlines()[:6]
            lines.append("    output (first lines):")
            lines += [f"      {h}" for h in head]
        lines.append("")

    summary = {
        "lore_calls": len(calls),
        "first_call_position": calls[0]["position"] if calls else None,
        "last_call_position": calls[-1]["position"] if calls else None,
        "total_tool_calls": len(parts),
        "calls": calls,
    }

    if not calls:
        header = ["(no lore calls — retrieval-off arm, or an on-arm cell that never called it)", ""]
    else:
        tail = len(parts) - calls[-1]["position"]
        header = [
            f"{len(calls)} lore call(s) out of {len(parts)} tool calls. "
            f"First at position {calls[0]['position']}, last at {calls[-1]['position']}, "
            f"then {tail} further non-lore call(s).",
            "",
        ]
    return header + lines, summary


def pack_cell(cell_dir: Path) -> str | None:
    metrics_path = cell_dir / "metrics.json"
    if not metrics_path.exists():
        return None
    m = json.loads(metrics_path.read_text(encoding="utf-8"))
    repo, task = m["repo"], m["task"]
    prompt = PROMPTS.get(repo, {}).get(task, "(prompt missing)")
    answer = (cell_dir / "answer.md").read_text(encoding="utf-8", errors="replace").strip() \
        if (cell_dir / "answer.md").exists() else "(no answer captured)"

    parts = list(events(cell_dir / "events.jsonl"))
    trail = tool_trail(parts)
    lore_lines, retrieval = retrieval_report(parts, answer)
    (cell_dir / "retrieval.json").write_text(
        json.dumps(retrieval, indent=2, ensure_ascii=False), encoding="utf-8"
    )

    lines = [
        f"## {m['cell']}",
        "",
        f"- model {m['model']}  arm {m['arm']}  repo {repo}  task {task}",
        f"- wall {m['wall_ms'] / 1000:.0f}s  tokens in/out {m['tokens']['input']}/{m['tokens']['output']}"
        f"  tools {m['tool_calls']} (lore {m['lore_calls']})  exit {m['exit_code']}",
        f"- prompt: {prompt}",
        "",
        "### lore calls",
        "",
        *lore_lines,
        f"### tool trail ({len(trail)} calls)",
        "",
        *[f"{i + 1:>3}. {t}" for i, t in enumerate(trail)],
        "",
        "### final answer",
        "",
        answer,
    ]

    diff_path = cell_dir / "diff.patch"
    if diff_path.exists() and diff_path.stat().st_size:
        diff = diff_path.read_text(encoding="utf-8", errors="replace").splitlines()
        shown = diff[:DIFF_LINE_LIMIT]
        lines += ["", f"### diff ({len(diff)} lines{', truncated' if len(diff) > DIFF_LINE_LIMIT else ''})", "", "```diff", *shown, "```"]
        suite = cell_dir / "suite-result.txt"
        if suite.exists():
            lines += ["", f"### suite result", "", suite.read_text(encoding='utf-8').strip()]
        else:
            lines += ["", "### suite result", "", "(not yet run — mechanical step pending)"]

    packet = "\n".join(lines) + "\n"
    (cell_dir / "packet.md").write_text(packet, encoding="utf-8")
    return packet


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cells", default="*", help="glob over result directory names (default: all)")
    ap.add_argument("--batch", choices=("repo-task",), help="also write one bundle per grading thread")
    args = ap.parse_args()

    packets, batches = [], {}
    for cell_dir in sorted(RESULTS.iterdir()):
        if not cell_dir.is_dir() or not cell_dir.name[:1].isdigit():
            continue
        if not fnmatch.fnmatch(cell_dir.name, args.cells):
            continue
        p = pack_cell(cell_dir)
        if not p:
            continue
        packets.append(p)
        m = json.loads((cell_dir / "metrics.json").read_text(encoding="utf-8"))
        batches.setdefault(f"{m['repo']}-{m['task']}", []).append(p)

    out = RESULTS / "packets-all.md"
    out.write_text("\n---\n\n".join(packets), encoding="utf-8")
    total = sum(len(p) for p in packets)
    print(f"{len(packets)} packets, {total / 1024:.0f} KB total -> {out}")

    if args.batch:
        batch_dir = RESULTS / "batches"
        batch_dir.mkdir(exist_ok=True)
        for name, group in sorted(batches.items()):
            path = batch_dir / f"{name}.md"
            path.write_text("\n---\n\n".join(group), encoding="utf-8")
            print(f"  batch {name}: {len(group)} cell(s), {len(''.join(group)) / 1024:.0f} KB -> {path}")


if __name__ == "__main__":
    sys.exit(main())
