#!/usr/bin/env python3
"""Distilled knowledge cards v0 — generator.

Walks a corpus tree, groups indexable files into areas (one per directory,
small directories merged into their parent, oversized groups split), and asks
a local model for one intent-level prose card per area. Cards land in
<root>/distilled/<area-slug>.md with deterministic frontmatter carrying the
source anchors (path + first 12 hex of the file's sha256).

Design + protocol: design/6_Evaluation/2026-08-18_distilled-cards-v0.md

Standalone on purpose: no daemon, no lore imports. Resumable — existing cards
are skipped unless --force; failures skip the card and are listed at the end.

  python distill.py --root C:\\Users\\perag\\bench-e2e\\lore-bench-d
"""

import argparse
import hashlib
import json
import re
import sys
import time
import urllib.request
from pathlib import Path

EXTENSIONS = {".rs", ".md", ".toml", ".ts", ".js", ".cs", ".py", ".ps1"}
# Mirrors the walker's hard-exclude list plus our own output dir.
EXCLUDE_DIRS = {
    ".git", "target", "node_modules", "Library", "Temp", "obj", "bin",
    "distilled", ".lore", "results", "data", "models", "tools",
    # v0 scope: tests document behavior but their paths are never answer
    # material, and distilling scratch would launder path-capped exploration
    # into uncapped cards — the exact failure mode cards must not introduce.
    "tests", "fixtures", "9_Scratch", "99_Scratch", "raw",
}
MIN_GROUP_FILES = 3        # smaller directories merge into their parent
MAX_GROUP_BYTES = 32_000   # split areas larger than this (post-truncation)
MAX_FILE_BYTES = 8_000     # per-file head included in the prompt
MAX_OUTPUT_TOKENS = 800

SYSTEM = (
    "You write distilled knowledge cards for a code retrieval index. A card "
    "is intent-level documentation of one area of a repository, written so "
    "that natural-language questions about behavior, guarantees, and design "
    "intent match it. The audience is a coding agent's semantic search: the "
    "card's job is to be found by question-shaped queries and to route the "
    "reader to the right source files."
)

USER_TEMPLATE = """Area: {area}

Source files (some truncated):

{files}

Write the card now. Format: a single '# Title' heading naming the area's \
subject (a topic, not a file path), then 150-400 words of flowing prose. \
Rules:
- Describe purpose, mechanisms, how the files relate, and the invariants \
they maintain, in plain language a developer would use when *asking* about \
this area — do not just repeat identifiers.
- Every substantive claim names its source file inline in backticks, \
e.g. `{example}`.
- No code blocks, no bullet lists, no headings other than the title, no \
restating of the file list.
Output only the card text."""


def sha12(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()[:12]


def slugify(rel: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", rel.lower()).strip("-")
    return slug or "root"


def collect_groups(root: Path):
    """dir(rel posix) -> [Path], small dirs merged upward, big groups split."""
    by_dir: dict[str, list[Path]] = {}
    for p in sorted(root.rglob("*")):
        if not p.is_file() or p.suffix.lower() not in EXTENSIONS:
            continue
        rel_parts = p.relative_to(root).parts
        if any(part in EXCLUDE_DIRS for part in rel_parts[:-1]):
            continue
        d = "/".join(rel_parts[:-1])
        by_dir.setdefault(d, []).append(p)

    # Merge small directories into their parent, deepest first.
    for d in sorted(by_dir, key=lambda d: d.count("/"), reverse=True):
        if d and len(by_dir[d]) < MIN_GROUP_FILES:
            parent = "/".join(d.split("/")[:-1])
            by_dir.setdefault(parent, []).extend(by_dir.pop(d))

    # Split oversized groups by cumulative (truncated) size.
    groups = {}
    for d, files in sorted(by_dir.items()):
        files.sort()
        parts, part, size = [], [], 0
        for f in files:
            fsize = min(f.stat().st_size, MAX_FILE_BYTES)
            if part and size + fsize > MAX_GROUP_BYTES:
                parts.append(part)
                part, size = [], 0
            part.append(f)
            size += fsize
        if part:
            parts.append(part)
        for n, part in enumerate(parts, start=1):
            groups[d if len(parts) == 1 else f"{d}#p{n}"] = part
    return groups


def read_head(p: Path) -> tuple[str, bool]:
    data = p.read_bytes()
    truncated = len(data) > MAX_FILE_BYTES
    text = data[:MAX_FILE_BYTES].decode("utf-8", errors="replace")
    return text, truncated


def call_model(api: str, model: str, area: str, files: list[tuple[str, str, bool]]) -> str:
    blocks = []
    for rel, text, truncated in files:
        note = " (truncated)" if truncated else ""
        blocks.append(f"=== {rel}{note} ===\n{text}")
    user = USER_TEMPLATE.format(area=area or "repository root",
                                files="\n\n".join(blocks),
                                example=files[0][0])
    body = json.dumps({
        "model": model,
        "messages": [{"role": "system", "content": SYSTEM},
                     {"role": "user", "content": user}],
        "temperature": 0.3,
        "max_tokens": MAX_OUTPUT_TOKENS,
    }).encode()
    req = urllib.request.Request(f"{api}/chat/completions", data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as resp:
        out = json.load(resp)
    text = out["choices"][0]["message"]["content"].strip()
    # Strip any <think> reasoning block a local qwen may emit.
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    if not text.startswith("#"):
        text = "# " + (area or "Repository root") + "\n\n" + text
    return text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True, type=Path)
    ap.add_argument("--api", default="http://localhost:11434/v1")
    ap.add_argument("--model", default="qwen3.8:latest")
    ap.add_argument("--force", action="store_true", help="regenerate existing cards")
    ap.add_argument("--dry-run", action="store_true", help="print the plan, call nothing")
    args = ap.parse_args()

    root = args.root.resolve()
    out_dir = root / "distilled"
    groups = collect_groups(root)
    print(f"{len(groups)} areas in {root}")

    if args.dry_run:
        for area, files in groups.items():
            size = sum(min(f.stat().st_size, MAX_FILE_BYTES) for f in files)
            print(f"  {area or '(root)'}: {len(files)} files, {size:,} bytes")
        return

    out_dir.mkdir(exist_ok=True)
    failures, written, skipped = [], 0, 0
    for area, files in groups.items():
        card_path = out_dir / f"{slugify(area)}.md"
        if card_path.exists() and not args.force:
            skipped += 1
            continue
        rels = [str(f.relative_to(root)).replace("\\", "/") for f in files]
        heads = [read_head(f) for f in files]
        t0 = time.time()
        try:
            prose = call_model(args.api, args.model, area,
                               [(r, t, tr) for r, (t, tr) in zip(rels, heads)])
        except Exception as e:
            print(f"  FAIL {area or '(root)'}: {e}")
            failures.append(area or "(root)")
            continue
        sources = "\n".join(
            f"  - path: {r}\n    sha256: {sha12(f)}" for r, f in zip(rels, files))
        card = (
            "---\n"
            "distilled: v0\n"
            "generator: bench/distill/distill.py\n"
            f"model: {args.model} ({args.api})\n"
            f"generated: {time.strftime('%Y-%m-%d')}\n"
            f"area: {area or '(root)'}\n"
            f"sources:\n{sources}\n"
            "---\n\n"
            f"{prose}\n"
        )
        card_path.write_text(card, encoding="utf-8", newline="\n")
        written += 1
        print(f"  [{written}] {area or '(root)'} ({len(files)} files, "
              f"{time.time() - t0:.0f}s) -> {card_path.name}")

    print(f"\nwritten {written}, skipped {skipped} existing, failed {len(failures)}")
    if failures:
        print("failed areas: " + ", ".join(failures))
        sys.exit(1)


if __name__ == "__main__":
    main()
