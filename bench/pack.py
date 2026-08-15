"""Reduce each result cell's raw event stream to a compact grading packet.

Deterministic extraction only — no model in the loop. Output:
results/<cell>/packet.md plus results/packets-all.md (everything a grader
thread needs, concatenated). Grading itself happens in a Fable thread that
receives the answer key + packets inline and never touches the repos.
"""

import json
import sys
from pathlib import Path

RESULTS = Path(__file__).parent / "results"
PROMPTS = json.loads((Path(__file__).parent / "prompts.json").read_text(encoding="utf-8"))
TRAIL_INPUT_LIMIT = 160
DIFF_LINE_LIMIT = 400


def tool_trail(events_path: Path) -> list[str]:
    trail = []
    for line in events_path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") != "tool_use":
            continue
        part = ev.get("part", {})
        name = part.get("tool", "?")
        state = part.get("state") or {}
        raw = state.get("input")
        arg = json.dumps(raw, ensure_ascii=False) if raw is not None else ""
        if len(arg) > TRAIL_INPUT_LIMIT:
            arg = arg[:TRAIL_INPUT_LIMIT] + "…"
        trail.append(f"{name} {arg}".rstrip())
    return trail


def pack_cell(cell_dir: Path) -> str | None:
    metrics_path = cell_dir / "metrics.json"
    if not metrics_path.exists():
        return None
    m = json.loads(metrics_path.read_text(encoding="utf-8"))
    repo, task = m["repo"], m["task"]
    prompt = PROMPTS.get(repo, {}).get(task, "(prompt missing)")
    answer = (cell_dir / "answer.md").read_text(encoding="utf-8", errors="replace").strip() \
        if (cell_dir / "answer.md").exists() else "(no answer captured)"
    trail = tool_trail(cell_dir / "events.jsonl") if (cell_dir / "events.jsonl").exists() else []

    lines = [
        f"## {m['cell']}",
        "",
        f"- model {m['model']}  arm {m['arm']}  repo {repo}  task {task}",
        f"- wall {m['wall_ms'] / 1000:.0f}s  tokens in/out {m['tokens']['input']}/{m['tokens']['output']}"
        f"  tools {m['tool_calls']} (lore {m['lore_calls']})  exit {m['exit_code']}",
        f"- prompt: {prompt}",
        "",
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
    packets = []
    for cell_dir in sorted(RESULTS.iterdir()):
        if not cell_dir.is_dir() or not cell_dir.name[:1].isdigit():
            continue
        p = pack_cell(cell_dir)
        if p:
            packets.append(p)
    out = RESULTS / "packets-all.md"
    out.write_text("\n---\n\n".join(packets), encoding="utf-8")
    total = sum(len(p) for p in packets)
    print(f"{len(packets)} packets, {total / 1024:.0f} KB total -> {out}")


if __name__ == "__main__":
    sys.exit(main())
