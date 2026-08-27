#!/usr/bin/env python3
"""Stage 3 -- keep the trajectories whose citations land where the answer key does.

SWE-QA-Pro-Bench ships a human-validated reference answer per question, and
those answers cite `path` and `lines N-M`. That is an automatic grading signal
and it is the reason this question source was chosen: a trajectory can be
scored without a judge model and without a human.

The reference text is **only ever read as an answer key**. It is never emitted
into a training row, never shown to the teacher, and never quoted in `meta`
beyond the derived scores -- otherwise the corpus would be teaching the student
to reproduce the key rather than to find the evidence.

Two signals, in order of how much they can be trusted:

  * **file recall** -- what fraction of the reference's cited files the
    trajectory's answer also cites. Robust: a right answer has to land on the
    right files, and file paths survive both refactoring noise and citation-style
    differences.
  * **span hit rate** -- what fraction of the reference's cited line ranges are
    covered by an overlapping range in the answer, within a tolerance. Sharper,
    but noisier: line numbers drift with how much surrounding context an author
    chose to include, which is exactly why it carries the lower bar.

    python3 grade.py --batch pilot-01 [--config config.toml] [--report-only]
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import sys

import common
from common import Config

# A path-shaped token, then -- within a short window after it -- an optional
# line reference. Two regexes rather than one because the reference answers write
# "src/x.py: line 12-30", "src/x.py lines 12-30" and "src/x.py (lines 12-30)",
# while our scouter writes "src/x.py:12-30". One alternation covering all four
# is unreadable and silently mis-binds when two paths are adjacent.
PATH_RE = re.compile(
    r"(?:[\w.\-]+/)*[\w.\-]+"
    r"\.(?:py|pyx|pyi|pxd|c|h|cc|cpp|hpp|rs|go|js|jsx|ts|tsx|java|rb|cs|"
    r"toml|cfg|ini|txt|md|rst|yaml|yml|json|sh)\b")
# No `^` anchor: this is used with `.match(text, pos)`, and `^` would still bind
# to position 0 there rather than to `pos` -- which silently yields zero spans.
LINES_RE = re.compile(
    r"[\s:,(\[]*(?:lines?|L)?[\s:#]*(\d+)(?:\s*(?:[-–—]|to)\s*(\d+))?")


def citations(text: str) -> tuple[set[str], list[tuple[str, int, int]]]:
    """(cited paths, cited spans). A path with no line reference has no span."""
    paths: set[str] = set()
    spans: list[tuple[str, int, int]] = []
    for match in PATH_RE.finditer(text or ""):
        path = match.group(0).lstrip("./")
        paths.add(path)
        tail = LINES_RE.match(text, match.end())
        if tail:
            start = int(tail.group(1))
            end = int(tail.group(2)) if tail.group(2) else start
            spans.append((path, min(start, end), max(start, end)))
    return paths, spans


def same_file(a: str, b: str) -> bool:
    """Suffix match, on path components.

    The reference answers are inconsistent about how much of the path they
    write (`src/qibo/models/circuit.py` in one answer, `qibo/models/circuit.py`
    in another for the same file), so requiring string equality would reject
    correct citations. Comparing whole components stops `b/x.py` matching
    `lib/x.py`.
    """
    pa, pb = a.split("/"), b.split("/")
    n = min(len(pa), len(pb))
    return n > 0 and pa[-n:] == pb[-n:]


def grade_row(row: dict, reference: str, cfg: Config,
              snapshot: str | None) -> tuple[dict, list[str]]:
    """Return (scores, reject reasons)."""
    gc = cfg["grade"]
    answer = row["messages"][-1]["content"]
    ref_paths, ref_spans = citations(reference)
    cand_paths, cand_spans = citations(answer)

    reasons: list[str] = []
    if row["meta"]["n_tool_calls"] < gc["min_tool_calls"]:
        reasons.append(f"few_tool_calls:{row['meta']['n_tool_calls']}")
    if not cand_paths:
        reasons.append("no_citations")

    # Every path the answer claims must exist at the pinned commit. Cheap, and
    # it is the only check here that catches a confidently hallucinated file.
    unresolvable = []
    if snapshot and os.path.isdir(snapshot):
        for path in sorted(cand_paths):
            if not os.path.exists(os.path.join(snapshot, path)):
                unresolvable.append(path)
        if unresolvable:
            reasons.append("unresolvable_citation:" + ",".join(unresolvable[:3]))
        checked = True
    else:
        checked = False

    if not ref_paths:
        scores = {"file_recall": None, "span_hit_rate": None,
                  "ref_files": 0, "ref_spans": 0,
                  "cand_files": len(cand_paths), "cand_spans": len(cand_spans),
                  "citations_checked": checked}
        if not gc["keep_ungradeable"]:
            reasons.append("ungradeable_reference")
        return scores, reasons

    matched = {p for p in ref_paths if any(same_file(p, c) for c in cand_paths)}
    file_recall = len(matched) / len(ref_paths)

    tol = int(gc["line_tolerance"])
    hits = 0
    for path, start, end in ref_spans:
        if any(same_file(path, cp) and cs <= end + tol and ce >= start - tol
               for cp, cs, ce in cand_spans):
            hits += 1
    span_hit_rate = hits / len(ref_spans) if ref_spans else None

    scores = {
        "file_recall": round(file_recall, 3),
        "span_hit_rate": None if span_hit_rate is None else round(span_hit_rate, 3),
        "ref_files": len(ref_paths), "ref_spans": len(ref_spans),
        "cand_files": len(cand_paths), "cand_spans": len(cand_spans),
        "citations_checked": checked,
    }
    if file_recall < gc["min_file_recall"]:
        reasons.append(f"low_file_recall:{file_recall:.2f}")
    if span_hit_rate is not None and span_hit_rate < gc["min_span_hit_rate"]:
        reasons.append(f"low_span_overlap:{span_hit_rate:.2f}")
    return scores, reasons


# --------------------------------------------------------------------------- #

def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--config", default=None)
    ap.add_argument("--batch", default=None)
    ap.add_argument("--report-only", action="store_true",
                    help="score and print, write nothing")
    ap.add_argument("--no-validate", action="store_true")
    args = ap.parse_args(argv)

    cfg = common.load_config(args.config)
    batch = args.batch or cfg["batch"]["name"]
    workspace = cfg.get_path("paths", "workspace")

    converted = os.path.join(workspace, "data", f"{batch}.converted.jsonl")
    if not os.path.exists(converted):
        raise SystemExit(f"no converted rows at {converted}; run convert.py first")
    rows = common.read_jsonl(converted)

    manifest = common.read_manifest(workspace, batch)
    pins = common.pins_by_repo(manifest)
    snap_root = cfg.get_path("paths", "snapshots")
    qfile = manifest.get("questions_file") or os.path.join(
        workspace, "questions", f"{batch}.jsonl")
    references = {q["qid"]: q.get("reference_answer", "")
                  for q in common.read_jsonl(qfile)}

    kept, rejected = [], []
    reason_counts: collections.Counter = collections.Counter()
    recalls = []
    for row in rows:
        meta = row["meta"]
        pin = pins.get(meta.get("repo", ""))
        snapshot = (os.path.join(snap_root, pin.snapshot)
                    if pin and not pin.dry_run else None)
        scores, reasons = grade_row(row, references.get(meta["qid"], ""), cfg,
                                    snapshot)
        meta.update(scores)
        meta["graded"] = True
        meta["verified"] = not reasons
        if scores.get("file_recall") is not None:
            recalls.append(scores["file_recall"])
        if reasons:
            meta["reject_reasons"] = reasons
            rejected.append(row)
            for reason in reasons:
                reason_counts[reason.split(":", 1)[0]] += 1
            print(f"  DROP {meta['qid']:<24} {', '.join(reasons)}")
        else:
            kept.append(row)
            print(f"  KEEP {meta['qid']:<24} file_recall="
                  f"{scores['file_recall']} span_hit_rate="
                  f"{scores['span_hit_rate']}")

    total = len(rows)
    print(f"\nkept {len(kept)}/{total} "
          f"({len(kept) / total * 100:.0f}%)" if total else "nothing to grade")
    if reason_counts:
        print("rejected:", ", ".join(f"{k}={v}" for k, v in
                                     reason_counts.most_common()))
    if recalls:
        recalls.sort()
        print(f"file_recall: min {recalls[0]:.2f} "
              f"median {recalls[len(recalls) // 2]:.2f} max {recalls[-1]:.2f}")

    if args.report_only:
        return 0

    out = os.path.join(workspace, "data", f"{batch}.train.jsonl")
    common.write_jsonl(out, kept)
    common.write_jsonl(os.path.join(workspace, "data",
                                    f"{batch}.grade-rejects.jsonl"), rejected)
    print(f"\nwrote {out}")
    if not kept:
        return 1
    return 0 if args.no_validate else common.run_validator(cfg, out)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
