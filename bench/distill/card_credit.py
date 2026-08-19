#!/usr/bin/env python3
"""Card-credit scoring variant for the distilled-cards experiment.

Rewrites a recorded search run, replacing each `distilled/` card hit with the
card's frontmatter source paths in listed order (score.ps1's existing
first-occurrence dedup then applies). Models an agent that follows a card's
anchors in one hop — the optimistic bracket; the raw run is the pessimistic
one. Emitted as a synthetic run dir beside the input so score.ps1 needs no
changes and shows both as model rows.

  python card_credit.py --run-dir ..\\retrieval\\results\\<runset>-qwen3-4b \
      --corpus lore-bench-d --root C:\\Users\\perag\\bench-e2e\\lore-bench-d
"""

import argparse
import json
import re
from pathlib import Path


def card_sources(root: Path) -> dict[str, list[str]]:
    """card path (as the daemon reports it, forward slashes) -> source paths."""
    mapping = {}
    for card in sorted((root / "distilled").glob("*.md")):
        text = card.read_text(encoding="utf-8")
        m = re.match(r"---\n(.*?)\n---", text, flags=re.S)
        if not m:
            continue
        paths = re.findall(r"^\s*- path: (.+)$", m.group(1), flags=re.M)
        mapping[f"distilled/{card.name}"] = [p.strip() for p in paths]
    return mapping


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", required=True, type=Path)
    ap.add_argument("--corpus", default="lore-bench-d")
    ap.add_argument("--root", required=True, type=Path)
    args = ap.parse_args()

    cards = card_sources(args.root)
    searches_file = args.run_dir / "searches" / f"{args.corpus}.json"
    searches = json.loads(searches_file.read_text(encoding="utf-8"))

    total_hits = expanded = unmapped = 0
    for s in searches:
        out = []
        for r in s["results"]:
            path = r["path"].replace("\\", "/")
            if path.startswith("distilled/"):
                total_hits += 1
                sources = cards.get(path)
                if not sources:
                    unmapped += 1
                    out.append(r)
                    continue
                expanded += 1
                for src in sources:
                    out.append({**r, "path": src, "via_card": path})
            else:
                out.append(r)
        s["results"] = out

    run_json = json.loads((args.run_dir / "run.json").read_text(encoding="utf-8"))
    out_dir = args.run_dir.parent / (args.run_dir.name + "+cardcredit")
    (out_dir / "searches").mkdir(parents=True, exist_ok=True)
    (out_dir / "searches" / f"{args.corpus}.json").write_text(
        json.dumps(searches, indent=2) + "\n", encoding="utf-8")
    (out_dir / "run.json").write_text(json.dumps({
        "model": run_json["model"] + "+cardcredit",
        "run_set": run_json["run_set"],
        "note": f"synthetic: {args.corpus} card hits expanded to their anchor "
                "source paths (bench/distill/card_credit.py); other corpora "
                "omitted on purpose",
    }, indent=2) + "\n", encoding="utf-8")

    print(f"card hits in recorded top-K: {total_hits} "
          f"(expanded {expanded}, unmapped {unmapped})")
    print(f"-> {out_dir}")


if __name__ == "__main__":
    main()
