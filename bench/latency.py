"""Reconstruct client-observed tool latencies from bench event streams.

Every opencode tool_use part carries state.time.{start,end} (ms). For lore_*
tools this is daemon latency plus the lore-mcp stdio hop and opencode
plumbing — an upper bound on server time. Prints percentiles per tool.
"""

import json
import glob
from pathlib import Path

RESULTS = Path(__file__).parent / "results"


def pct(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return float("nan")
    k = min(len(sorted_vals) - 1, max(0, round(p / 100 * (len(sorted_vals) - 1))))
    return sorted_vals[k]


def main() -> None:
    by_tool: dict[str, list[float]] = {}
    for f in glob.glob(str(RESULTS / "2026*" / "events.jsonl")):
        for line in open(f, encoding="utf-8", errors="replace"):
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("type") != "tool_use":
                continue
            state = ev["part"].get("state") or {}
            t = state.get("time") or {}
            if "start" in t and "end" in t:
                tool = ev["part"].get("tool", "?")
                key = tool
                if tool.startswith("lore_"):
                    project = (state.get("input") or {}).get("project") or "all"
                    key = f"{tool}[{project}]"
                by_tool.setdefault(key, []).append(t["end"] - t["start"])

    print(f"{'tool':16} {'n':>5} {'mean':>8} {'p50':>7} {'p90':>7} {'p95':>7} {'p99':>7} {'max':>8}")
    for tool, vals in sorted(by_tool.items(), key=lambda kv: -len(kv[1])):
        vals.sort()
        mean = sum(vals) / len(vals)
        print(f"{tool:16} {len(vals):>5} {mean:>7.0f}ms {pct(vals, 50):>6.0f}ms "
              f"{pct(vals, 90):>6.0f}ms {pct(vals, 95):>6.0f}ms {pct(vals, 99):>6.0f}ms {vals[-1]:>7.0f}ms")


if __name__ == "__main__":
    main()
