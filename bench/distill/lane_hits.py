#!/usr/bin/env python3
"""Lane-quality analysis for the distilled-cards experiment.

With the lane shipped, cards never appear in the ranked page, so the question
is no longer displacement — it is whether the <=3-card lane offers a useful
router. For each query: did the lane contain a card whose frontmatter anchors
include a key path ("routing hit")? Reported per query kind, plus whether the
page itself already had the key in its top 10 (a lane hit matters most where
the page missed).

  python lane_hits.py --run-dir ..\\retrieval\\results\\<runset>-<model> \
      --corpus lore-bench-d2 --root C:\\Users\\perag\\bench-e2e\\lore-bench-d2 \
      --keys ..\\retrieval\\queries\\lore-bench.json
"""

import argparse
import json
from pathlib import Path

from card_credit import card_sources


def norm(p: str) -> str:
    return p.replace("\\", "/").lower()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", required=True, type=Path)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--root", required=True, type=Path)
    ap.add_argument("--keys", required=True, type=Path)
    args = ap.parse_args()

    cards = {norm(k): [norm(s) for s in v]
             for k, v in card_sources(args.root).items()}
    key = json.loads(args.keys.read_text(encoding="utf-8"))
    rel = {q["id"]: {norm(r["path"]) for r in q["relevant"]} for q in key["queries"]}
    kinds = {q["id"]: q["kind"] for q in key["queries"]}

    searches = json.loads(
        (args.run_dir / "searches" / f"{args.corpus}.json").read_text(encoding="utf-8"))

    rows, by_kind = [], {}
    for s in searches:
        qid = s["id"]
        lane = [norm(r["path"]) for r in s.get("distilled") or []]
        page = []
        seen = set()
        for r in s["results"]:
            p = norm(r["path"])
            if p not in seen:
                seen.add(p)
                page.append(p)
        page_hit = any(p in rel[qid] for p in page[:10])
        routing_hit = any(rel[qid] & set(cards.get(c, [])) for c in lane)
        rows.append((qid, kinds[qid], len(lane), page_hit, routing_hit))
        k = by_kind.setdefault(kinds[qid], {"n": 0, "lane": 0, "route": 0,
                                            "rescue": 0, "page": 0})
        k["n"] += 1
        k["lane"] += bool(lane)
        k["route"] += routing_hit
        k["page"] += page_hit
        k["rescue"] += routing_hit and not page_hit

    print(f"{args.corpus}  ({args.run_dir.name})")
    print(f"{'id':7} {'kind':9} lane routing page@10")
    for qid, kind, n_lane, page_hit, routing_hit in rows:
        mark = "  <-- rescue" if routing_hit and not page_hit else ""
        print(f"{qid:7} {kind:9} {n_lane:4} {str(routing_hit):7} {str(page_hit):7}{mark}")
    print("\nper kind: n / lane shown / routing hit / page hit / rescues")
    for kind, k in sorted(by_kind.items()):
        print(f"  {kind:9} {k['n']:2} / {k['lane']:2} / {k['route']:2} "
              f"/ {k['page']:2} / {k['rescue']}")


if __name__ == "__main__":
    main()
