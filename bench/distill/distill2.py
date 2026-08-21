#!/usr/bin/env python3
"""Distilled knowledge cards v1 — judgment-carved areas.

Two phases (design/6_Evaluation/2026-08-18_distilled-cards-v0.md §v1):

  map:     one model call over a repo digest (tree + file heads) returns a
           JSON plan of areas — slug, title, intent, explicit file list.
           Judgment decides the carving; directories are just paths.
  compile: one call per area with WHOLE files (large caps) plus the plan's
           intent line, so the card is written toward the questions the
           planner said the area answers.

The plan is validated (hallucinated paths dropped loudly) and cached under
plans/, so it is reviewable and the compile pass resumes against it.
Laundering exclusions are shared with v0 via import.

  python distill2.py --root C:\\Users\\perag\\bench-e2e\\lore-bench-d2
"""

import argparse
import json
import re
import sys
import time
import urllib.request
from pathlib import Path

from distill import EXCLUDE_DIRS, EXTENSIONS, sha12

DIGEST_HEAD_BYTES = 1_200     # per-file head shown to the planner
CARD_FILE_BYTES = 24_000      # per-file cap shown to the compiler
CARD_AREA_BYTES = 72_000      # total source budget per card
MAX_OUTPUT_TOKENS = 1600
PLAN_MAX_TOKENS = 24_000  # plan JSON + any thinking share this budget
DIGEST_BUDGET = 300_000   # plan-input cap; big repos fall back to tree mode

PLAN_SYSTEM = (
    "You are planning a distillation of a repository into knowledge cards "
    "for a code retrieval index. A card is intent-level documentation of one "
    "coherent area — the unit a developer asks questions about. Your job is "
    "only the carving: decide the areas and which files tell each area's "
    "story. Carve by concept and responsibility, never by directory for its "
    "own sake; an area may span directories, and boilerplate may be left out."
)

PLAN_TEMPLATE = """Repository digest (every indexable file, with its head):

{digest}

Return ONLY a JSON object, no prose, in this exact shape:

{{"areas": [{{"slug": "kebab-case-name",
             "title": "what this area is",
             "intent": "one sentence: the questions this area answers",
             "files": ["exact/path/from/digest", "..."]}}]}}

Rules: 12-40 areas, each citing the 2-6 files that best tell its story —
cards are routers to the right sources, not exhaustive coverage, so leaving
most files uncited is correct. Every cited path must be copied exactly from
the digest. A file may appear in two areas when it genuinely serves both."""

CARD_SYSTEM = (
    "You write distilled knowledge cards for a code retrieval index. A card "
    "is intent-level documentation of one area of a repository, written so "
    "that natural-language questions about behavior, guarantees, and design "
    "intent match it. The audience is a coding agent's semantic search: the "
    "card's job is to be found by question-shaped queries and to route the "
    "reader to the right source files."
)

CARD_TEMPLATE = """Area: {title}
This area answers: {intent}

Source files (some truncated):

{files}

Write the card now. Format: a single '# Title' heading naming the area's \
subject, then 150-400 words of flowing prose aimed squarely at the questions \
above. Rules:
- Describe purpose, mechanisms, how the files relate, and the invariants \
they maintain, in plain language a developer would use when *asking* about \
this area — do not just repeat identifiers.
- Every substantive claim names its source file inline in backticks, \
e.g. `{example}`.
- No code blocks, no bullet lists, no headings other than the title, no \
restating of the file list.
Output only the card text."""


def chat(api, model, system, user, timeout=900, max_tokens=MAX_OUTPUT_TOKENS):
    body = json.dumps({
        "model": model,
        "messages": [{"role": "system", "content": system},
                     {"role": "user", "content": user}],
        "temperature": 0.3,
        "max_tokens": max_tokens,
    }).encode()
    req = urllib.request.Request(f"{api}/chat/completions", data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        out = json.load(resp)
    text = out["choices"][0]["message"]["content"].strip()
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    return text, out.get("usage") or {}


def indexable_files(root: Path) -> list[Path]:
    files = []
    for p in sorted(root.rglob("*")):
        if not p.is_file() or p.suffix.lower() not in EXTENSIONS:
            continue
        if any(part in EXCLUDE_DIRS for part in p.relative_to(root).parts[:-1]):
            continue
        files.append(p)
    return files


def rel(p: Path, root: Path) -> str:
    return str(p.relative_to(root)).replace("\\", "/")


def build_digest(root: Path, files: list[Path]) -> str:
    parts = []
    for p in files:
        head = p.read_bytes()[:DIGEST_HEAD_BYTES].decode("utf-8", errors="replace")
        parts.append(f"=== {rel(p, root)} ({p.stat().st_size:,} bytes) ===\n{head}")
    return "\n\n".join(parts)


def make_plan(args, root: Path, files: list[Path]) -> dict:
    digest = build_digest(root, files)
    print(f"map pass: {len(files)} files, digest {len(digest):,} chars "
          f"-> {args.plan_model}", flush=True)
    text, usage = chat(args.plan_api, args.plan_model, PLAN_SYSTEM,
                       PLAN_TEMPLATE.format(digest=digest),
                       max_tokens=PLAN_MAX_TOKENS)
    m = re.search(r"\{.*\}", text, flags=re.S)
    if not m:
        sys.exit(f"planner returned no JSON object:\n{text[:2000]}")
    plan = json.loads(m.group(0))

    known = {rel(p, root) for p in files}
    areas, seen_slugs = [], set()
    for a in plan.get("areas", []):
        valid = [f for f in a.get("files", []) if f in known]
        dropped = [f for f in a.get("files", []) if f not in known]
        if dropped:
            print(f"  plan: dropped unknown paths in '{a.get('slug')}': {dropped}",
                  flush=True)
        slug = re.sub(r"[^a-z0-9]+", "-", str(a.get("slug", "")).lower()).strip("-")
        if not valid or not slug or slug in seen_slugs:
            continue
        seen_slugs.add(slug)
        areas.append({"slug": slug, "title": str(a.get("title", slug)),
                      "intent": str(a.get("intent", "")), "files": valid})
    if not areas:
        sys.exit("plan validation left no areas")
    covered = {f for a in areas for f in a["files"]}
    print(f"  plan: {len(areas)} areas; {len(known - covered)} of {len(known)} "
          f"files in no area; usage {usage.get('prompt_tokens', '?')}->"
          f"{usage.get('completion_tokens', '?')} tok", flush=True)
    return {"strategy": "v1-planned", "plan_model": args.plan_model,
            "generated": time.strftime("%Y-%m-%d"), "areas": areas,
            "uncovered": sorted(known - covered), "usage": usage}


def compile_card(args, root: Path, area: dict) -> tuple[str, dict]:
    blocks, spent = [], 0
    for f in area["files"]:
        p = root / f
        data = p.read_bytes()
        cap = min(CARD_FILE_BYTES, max(2_000, CARD_AREA_BYTES - spent))
        text = data[:cap].decode("utf-8", errors="replace")
        spent += min(len(data), cap)
        note = " (truncated)" if len(data) > cap else ""
        blocks.append(f"=== {f}{note} ===\n{text}")
    user = CARD_TEMPLATE.format(title=area["title"], intent=area["intent"],
                                files="\n\n".join(blocks), example=area["files"][0])
    prose, usage = chat(args.card_api, args.card_model, CARD_SYSTEM, user)
    if not prose.startswith("#"):
        prose = "# " + area["title"] + "\n\n" + prose
    return prose, usage


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True, type=Path)
    ap.add_argument("--plan-api", default="http://localhost:11434/v1")
    ap.add_argument("--plan-model", default="qwen3.8:latest")
    ap.add_argument("--card-api", default="http://localhost:11434/v1")
    ap.add_argument("--card-model", default="qwen3.8:latest")
    ap.add_argument("--plan-file", type=Path, help="default plans/<rootname>.json")
    ap.add_argument("--force-plan", action="store_true")
    ap.add_argument("--force", action="store_true", help="recompile existing cards")
    ap.add_argument("--dry-run", action="store_true", help="plan only, compile nothing")
    ap.add_argument("--file-list", type=Path,
                    help="newline-separated project-relative paths to use instead of walking (e.g. exported from the live index)")
    args = ap.parse_args()

    root = args.root.resolve()
    if args.file_list:
        rels = [l.strip() for l in args.file_list.read_text(encoding="utf-8").splitlines() if l.strip()]
        files = [root / r for r in rels if (root / r).is_file()]
        missing = len(rels) - len(files)
        if missing:
            print(f"file-list: {missing} listed paths not on disk (skipped)", flush=True)
    else:
        files = indexable_files(root)
    plan_file = args.plan_file or Path(__file__).parent / "plans" / f"{root.name}.json"

    if plan_file.exists() and not args.force_plan:
        plan = json.loads(plan_file.read_text(encoding="utf-8"))
        print(f"plan: reusing {plan_file} ({len(plan['areas'])} areas)", flush=True)
    else:
        plan = make_plan(args, root, files)
        plan_file.parent.mkdir(exist_ok=True)
        plan_file.write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8",
                             newline="\n")
        print(f"plan -> {plan_file}", flush=True)

    if args.dry_run:
        for a in plan["areas"]:
            print(f"  {a['slug']}: {len(a['files'])} files — {a['intent']}")
        return

    out_dir = root / "distilled"
    out_dir.mkdir(exist_ok=True)
    failures, written, skipped = [], 0, 0
    tok_in = tok_out = 0
    for area in plan["areas"]:
        card_path = out_dir / f"{area['slug']}.md"
        if card_path.exists() and not args.force:
            skipped += 1
            continue
        t0 = time.time()
        try:
            prose, usage = compile_card(args, root, area)
        except Exception as e:
            print(f"  FAIL {area['slug']}: {e}", flush=True)
            failures.append(area["slug"])
            continue
        tok_in += usage.get("prompt_tokens", 0)
        tok_out += usage.get("completion_tokens", 0)
        sources = "\n".join(
            f"  - path: {f}\n    sha256: {sha12(root / f)}" for f in area["files"])
        card = (
            "---\n"
            "distilled: v1\n"
            "generator: bench/distill/distill2.py\n"
            f"strategy: v1-planned ({plan['plan_model']})\n"
            f"model: {args.card_model} ({args.card_api})\n"
            f"generated: {time.strftime('%Y-%m-%d')}\n"
            f"area: {area['title']}\n"
            f"intent: {area['intent']}\n"
            f"sources:\n{sources}\n"
            "---\n\n"
            f"{prose}\n"
        )
        card_path.write_text(card, encoding="utf-8", newline="\n")
        written += 1
        print(f"  [{written}] {area['slug']} ({len(area['files'])} files, "
              f"{time.time() - t0:.0f}s, {usage.get('prompt_tokens', '?')}->"
              f"{usage.get('completion_tokens', '?')} tok)", flush=True)

    print(f"\nwritten {written}, skipped {skipped} existing, failed {len(failures)}")
    print(f"compile tokens: {tok_in:,} prompt, {tok_out:,} completion "
          "(completion includes any stripped reasoning)")
    if failures:
        print("failed areas: " + ", ".join(failures))
        sys.exit(1)


if __name__ == "__main__":
    main()
