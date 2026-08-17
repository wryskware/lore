"""Export real lore chunks from a bench daemon's SQLite as a throughput fixture.

The throughput probe must feed servers the *actual* text lore embeds — chunk
length distribution drives batching behaviour far more than raw byte count, and
a synthetic uniform-length corpus flatters every server equally wrongly.

    python export-fixture.py ../data/qwen3-0.6b/lore.db fixture.jsonl -n 4000

Rows are sampled at a fixed stride over chunk_id so all three bench corpora
(lexomancy / lore-bench / terrarium-bench) stay represented in proportion, and
re-running with the same -n reproduces the same fixture. Text is clipped to
--max-bytes on a UTF-8 boundary, mirroring the daemon's max_embed_bytes.
"""

import argparse
import json
import sqlite3


def clip(text: str, max_bytes: int) -> str:
    raw = text.encode("utf-8")
    if len(raw) <= max_bytes:
        return text
    return raw[:max_bytes].decode("utf-8", "ignore")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("out")
    ap.add_argument("-n", type=int, default=4000, help="target chunk count")
    ap.add_argument("--max-bytes", type=int, default=3584, help="daemon max_embed_bytes")
    args = ap.parse_args()

    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    rows = con.execute(
        "SELECT ch.text, p.name, ch.path FROM chunks ch"
        " JOIN projects p ON p.id = ch.project_id ORDER BY ch.chunk_id"
    ).fetchall()

    stride = max(1, len(rows) // args.n)
    picked = rows[::stride][: args.n]

    total = 0
    with open(args.out, "w", encoding="utf-8") as fh:
        for text, project, path in picked:
            t = clip(text, args.max_bytes)
            total += len(t.encode("utf-8"))
            fh.write(json.dumps({"text": t, "project": project, "path": path}) + "\n")

    print(
        f"{len(picked)} chunks from {len(rows)} (stride {stride}), "
        f"{total / 1e6:.1f} MB, mean {total // len(picked)} B -> {args.out}"
    )


if __name__ == "__main__":
    main()
