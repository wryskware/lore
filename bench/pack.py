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
# `    project_key: lexomancy-bench  chunk_id: a51edc03203c`
CHUNK_RE = re.compile(r"chunk_id:\s*(?P<cid>[0-9a-f]+)")


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
                "chunk_id": None,
            }
            hits.append(current)
            continue
        if current is not None:
            if current["chunk_id"] is None:
                c = CHUNK_RE.search(line)
                if c:
                    current["chunk_id"] = c.group("cid")
            if current["symbol"] is None:
                sym = SYMBOL_RE.match(line)
                if sym:
                    current["symbol"] = sym.group("name")
    return hits


def later_tool_text(parts: list[dict], after: int) -> str:
    """Every tool input the agent issued after call index `after`, as one blob."""
    blob = []
    for part in parts[after + 1:]:
        raw = (part.get("state") or {}).get("input")
        if raw is not None:
            blob.append(json.dumps(raw, ensure_ascii=False))
    return "\n".join(blob).replace("\\\\", "/").replace("\\", "/")


SUITE_MARKERS = ("cargo test", "pytest", "cargo nextest")
SUITE_VERDICT_RE = re.compile(
    r"(test result: (?:ok|FAILED)[^\n]*|\d+ passed[^\n]*|\d+ failed[^\n]*|error\[E\d+\][^\n]*|"
    r"error: (?:test failed|could not compile)[^\n]*)"
)


def agent_suite_runs(parts: list[dict]) -> list[dict]:
    """Suite commands the AGENT chose to run, with how they came out.

    Self-reported and not a substitute for the harness's own run: the agent
    picks the command, so a green `cargo test -p lore --lib chunk::tests` says
    nothing about the workspace. Extracted because it is real evidence that was
    otherwise being discarded — and because round 2's four T5 cells had to be
    graded without a harness suite result at all, while three of the four had
    in fact run a full suite themselves.
    """
    runs = []
    for idx, part in enumerate(parts):
        if part.get("tool") != "bash":
            continue
        state = part.get("state") or {}
        cmd = ((state.get("input") or {}).get("command") or "").strip()
        if not any(marker in cmd for marker in SUITE_MARKERS):
            continue
        output = state.get("output") or ""
        verdicts = SUITE_VERDICT_RE.findall(output)
        failed = any(
            ("FAILED" in v) or ("error" in v) or re.match(r"\d+ failed", v) and not v.startswith("0 failed")
            for v in verdicts
        )
        runs.append({
            "position": idx + 1,
            "command": cmd.replace("\n", " ")[:200],
            "result_lines": verdicts[-6:],
            "looks_green": bool(verdicts) and not failed,
            "scope": "workspace" if ("--workspace" in cmd or re.search(r"pytest\s+-q\s*$", cmd)) else "partial",
        })
    return runs


def retrieval_report(parts: list[dict], answer: str) -> tuple[list[str], dict]:
    """The `### lore calls` section plus its machine-readable twin.

    `uptake` and `answer_overlap` are string containment over the returned
    paths, deliberately: a path is a long, distinctive token, so a later
    `read` of it or a citation of it in the answer is unambiguous, and
    nothing here needs a model's opinion.
    """
    answer_norm = (answer or "").replace("\\", "/")
    calls, lines = [], []

    # Where each chunk_id was later expanded. `lore_expand` on a chunk a search
    # just returned IS uptake -- the strongest kind, because the agent went
    # deeper without leaving the index -- and counting only `read` of a path
    # misses it entirely. Round-2's corpora barely used expand; on Lexomancy it
    # is 8 of 22 lore calls, which would have understated uptake by a third.
    expanded_at: dict[str, list[int]] = {}
    for idx, part in enumerate(parts):
        if part.get("tool") != "lore_expand":
            continue
        cid = ((part.get("state") or {}).get("input") or {}).get("chunk_id")
        if cid:
            expanded_at.setdefault(cid, []).append(idx)

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
        read_paths = [p for p in paths if p in after]
        expanded = [
            h["path"] for h in hits
            if h["chunk_id"] and any(j > idx for j in expanded_at.get(h["chunk_id"], []))
        ]
        opened = list(dict.fromkeys(read_paths + expanded))
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
            "chunk_ids": [h["chunk_id"] for h in hits if h["chunk_id"]],
            "opened_after": opened,
            "read_after": read_paths,
            "expanded_after": list(dict.fromkeys(expanded)),
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
            if expanded:
                lines.append(
                    f"    ...of which {len(expanded)} via lore_expand on a returned chunk: "
                    + ", ".join(expanded)
                )
            lines.append(
                f"    answer overlap: {len(cited)}/{len(paths)} returned path(s) appear in the final answer"
            )
        else:
            if tool == "lore_expand":
                cid = raw_in.get("chunk_id")
                src = next(
                    (c for c in reversed(calls[:-1])
                     if cid and cid in (c.get("chunk_ids") or [])),
                    None,
                )
                lines.append(
                    f"    expands a chunk returned by call {src['position']}"
                    if src else
                    "    chunk was NOT returned by an earlier call in this cell"
                )
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
        lines += ["", "### suite result (harness-run, authoritative)", ""]
        if suite.exists():
            lines.append(suite.read_text(encoding="utf-8").strip())
        else:
            lines.append(
                "NOT RUN. This cell predates the harness running suites itself, so there is "
                "no authoritative result. Grade any criterion that depends on it as "
                "`confidence: low` rather than assuming green."
            )

        runs = agent_suite_runs(parts)
        lines += ["", "### suite runs the agent made itself (self-reported)", ""]
        if not runs:
            lines.append("(none — the agent never ran a suite)")
        else:
            lines.append(
                "The agent chose these commands, so scope matters: a green partial run says "
                "nothing about the whole suite. Weigh accordingly; this is not a substitute "
                "for the harness-run result above."
            )
            lines.append("")
            for run in runs:
                flag = "looks green" if run["looks_green"] else "NOT green"
                lines.append(f"- call {run['position']} ({run['scope']}, {flag}): `{run['command']}`")
                for rl in run["result_lines"]:
                    lines.append(f"    {rl}")

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
