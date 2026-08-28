#!/usr/bin/env python3
"""Stage 1 -- question in, raw teacher trajectory out.

One cell = one question answered by the teacher (gpt-5.6-luna at max reasoning
effort, driven through `opencode run --format json`) inside a pinned snapshot of
the question's repository, with lore's MCP server wired in so `bundle` and
`search` are real tool calls rather than shell commands.

The recording mechanism is opencode's own event stream. Nothing is intercepted
and nothing is re-implemented: `--format json` already emits, per model step,
a `text` part for assistant prose and a `tool_use` part carrying `callID`,
`state.input` (the exact arguments) and `state.output` (the exact result), all
tagged with the `messageID` they belong to. That is a superset of what the
emission format needs, and it is a file on disk rather than a live hook, so a
crashed cell still leaves a parseable partial trajectory. See README.md,
"Decision 3".

    python3 generate.py --config config.toml --batch pilot-01 \
        --repos qiboteam/qibo --limit-per-repo 5

    python3 generate.py --dry-run          # no teacher, no lore, no network

`--dry-run` writes genuine opencode-shaped event streams for a built-in
two-question fixture, so every downstream stage runs its real code path against
real input. It spends nothing.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import json
import os
import queue
import shutil
import subprocess
import sys
import time

import common
from common import Config, RepoPin

# --------------------------------------------------------------------------- #
# Teacher steering
#
# This is the teacher's prompt, NOT the scouter's. It never reaches the training
# data: convert.py replaces the whole system/user framing with
# `common.SCOUT_SYSTEM_PROMPT` plus the bare question, so harness scaffolding
# cannot become conditioning the student will never see at inference.
# --------------------------------------------------------------------------- #

TEACHER_PROMPT = """You are answering a question about the repository in your working directory. \
Answer it the way a disciplined repository scout would, because this session is being recorded as \
a demonstration.

Question:
{question}

How to work:

1. Call the `bundle` tool FIRST, before any other tool, with one rich \
question-shaped query covering everything the answer needs. Do NOT paste the \
question back verbatim -- expand it into the query you would actually want \
answered, naming the concepts, symbols and file kinds you expect to be \
involved. It returns a mechanically verified evidence bundle from this \
repository's local index: a VERDICT line, then real source quoted with real \
line numbers under `=== path:start-end [symbol] ===` headers, and possibly \
FURTHER READING pointers.
2. Then keep going. Use `search` for more of the index, and `grep`, `read` and \
`bash` to confirm and widen what the bundle gave you. Do not stop at the bundle: \
open the definitions that matter and check the call sites. Also do not thrash -- \
aim for a handful of well-chosen calls, not dozens.
3. If the bundle's VERDICT is `weak` or `none`, or a `NO MATCH FOR:` line names \
part of what you asked about, go and find that part yourself rather than \
answering around it.
4. Finish with a direct answer to the question. Cite every claim as a \
`path:start-end` line span. Every path you write MUST be relative to the \
repository root -- never an absolute path, never a path with a leading slash, \
never `~`. Do not invent spans; cite only what you actually saw.

Say briefly what you are doing before each tool call. Do not modify anything: \
this repository is a pinned snapshot and must stay byte-identical."""


# --------------------------------------------------------------------------- #
# Questions
# --------------------------------------------------------------------------- #

def load_questions(cfg: Config, repos: list[str], limit_per_repo: int) -> list[dict]:
    """Rows of {qid, repo, commit, question, answer, ...} in file order.

    Source is SWE-QA-Pro-Bench's `data/test.jsonl` (MIT): 260 human-validated
    questions, 10 each over 26 commit-pinned Python repositories, every
    reference answer carrying file/line citations. Point [questions].file at a
    local copy, or leave it empty to pull the pinned file from the Hub.
    """
    path = cfg.get_path("questions", "file")
    if not path:
        try:
            from huggingface_hub import hf_hub_download
        except ImportError as exc:
            raise SystemExit(
                "no [questions].file configured and huggingface_hub is not "
                f"installed ({exc}). Either pip install huggingface_hub or "
                "download data/test.jsonl and set [questions].file.") from exc
        path = hf_hub_download(cfg["questions"]["hf_dataset"],
                               cfg["questions"]["hf_file"], repo_type="dataset")

    seen: dict[str, int] = {}
    out = []
    for row in common.read_jsonl(path):
        repo = row["repo"]
        index = seen.get(repo, 0)
        seen[repo] = index + 1
        if repos and repo not in repos:
            continue
        if limit_per_repo and index >= limit_per_repo:
            continue
        out.append({
            "qid": common.question_id(repo, index),
            "repo": repo,
            "commit": row["commit_id"],
            "question": row["question"],
            "question_sha12": common.sha12(row["question"]),
            "reference_answer": row.get("answer", ""),
            "qa_class": (row.get("qa_type") or {}).get("class_name", ""),
        })
    return out


# --------------------------------------------------------------------------- #
# Snapshots
# --------------------------------------------------------------------------- #

def ensure_snapshot(cfg: Config, repo: str, commit: str) -> str:
    """Check `repo` out at `commit` under the snapshot root; return its path.

    Idempotent: an existing checkout already at the right commit is left alone,
    which is what makes a re-run cheap and a pin verifiable.
    """
    root = cfg.get_path("paths", "snapshots")
    dest = os.path.join(root, common.slug(repo))
    os.makedirs(root, exist_ok=True)

    def git(*args: str, cwd: str = dest) -> str:
        return subprocess.run(["git", *args], cwd=cwd, check=True,
                              capture_output=True, text=True).stdout.strip()

    if not os.path.isdir(os.path.join(dest, ".git")):
        url = cfg["questions"]["clone_url_template"].format(repo=repo)
        print(f"  cloning {repo} -> {dest}", flush=True)
        subprocess.run(["git", "clone", "--filter=blob:none", "--no-checkout",
                        url, dest], check=True)
    if git("rev-parse", "HEAD") != commit:
        try:
            git("checkout", "--force", "--detach", commit)
        except subprocess.CalledProcessError:
            git("fetch", "--depth", "1", "origin", commit)
            git("checkout", "--force", "--detach", commit)
    head = git("rev-parse", "HEAD")
    if head != commit:
        raise SystemExit(f"{repo}: snapshot is at {head}, wanted {commit}")
    return dest


def pin_repo(cfg: Config, repo: str, commit: str, dry_run: bool) -> RepoPin:
    """Snapshot + index state, recorded together so a row is reproducible.

    The pin is deliberately two-sided. The commit fixes what the teacher could
    read; the lore project key plus the daemon's monotonic index generation fix
    what `bundle` could have returned. A trajectory whose generation no longer
    matches the daemon's is not wrong, but it is no longer reproducible, and
    the manifest is what lets anyone tell.
    """
    project = (cfg["lore"]["project_prefix"] or "") + common.slug(repo)
    if dry_run:
        return RepoPin(repo=repo, commit=commit, snapshot=common.slug(repo),
                       lore_project=project, project_key="dry-run",
                       index_generation=0, dry_run=True)

    snapshot = ensure_snapshot(cfg, repo, commit)
    base = common.daemon_base(cfg)
    try:
        info = common.daemon_get(base, f"/resolve?path={snapshot}")
    except common.DaemonError as exc:
        raise SystemExit(
            f"{repo}: lore does not know {snapshot}.\n"
            f"  Register and index it first:  lore add {snapshot} && lore index\n"
            f"  ({exc})") from exc
    key = info.get("key") or info.get("project_key") or ""
    status = common.daemon_get(base, "/status")
    proj = next((p for p in status.get("projects", [])
                 if p.get("key") == key or p.get("name") == info.get("name")), {})
    if not proj.get("chunks"):
        raise SystemExit(f"{repo}: lore project {info.get('name')!r} has no chunks "
                         f"indexed; run `lore index` before generating.")
    # The daemon reports readiness as `embeddings.state == "ready"`, not as a
    # boolean `ready` key; the older spelling is still accepted so a daemon that
    # predates the change does not read as broken.
    embeddings = status.get("embeddings") or {}
    ready = embeddings.get("state") == "ready" or bool(embeddings.get("ready"))
    if not ready:
        raise SystemExit(
            f"{repo}: the embedding endpoint is not ready "
            f"(embeddings={embeddings!r}), so every bundle would silently "
            f"degrade to lexical-only. Start it and re-check `lore status` "
            f"before spending teacher calls.")
    # Readiness of the endpoint is not coverage of the index. A project whose
    # embedding backlog is still draining answers `bundle` from whatever subset
    # has vectors, which is lexical-only for everything else -- and it does so
    # without erroring, so nothing downstream would ever notice. Coverage has to
    # be complete before a teacher call is worth spending.
    chunks = int(proj.get("chunks") or 0)
    embedded = int(proj.get("embedded_chunks") or 0)
    if embedded < chunks:
        raise SystemExit(
            f"{repo}: lore project {info.get('name')!r} is only "
            f"{embedded}/{chunks} embedded ({embedded / chunks * 100:.0f}%). "
            f"Bundles would degrade to lexical-only over the remainder. Wait "
            f"for `lore status` to reach 100% before spending teacher calls.")
    return RepoPin(
        repo=repo, commit=commit, snapshot=common.slug(repo),
        lore_project=info.get("name") or project, project_key=key,
        index_generation=int(status.get("generation") or 0),
        files=int(proj.get("files") or 0), chunks=int(proj.get("chunks") or 0),
        embedded_chunks=int(proj.get("embedded_chunks") or 0),
        daemon_version=str(status.get("daemon_version") or ""),
    )


# --------------------------------------------------------------------------- #
# opencode
# --------------------------------------------------------------------------- #

def opencode_config(cfg: Config, out_dir: str) -> str:
    """Per-cell opencode config: the model, the tool gate, the lore MCP server.

    The tool surface is narrowed to the five the scouter will have, plus the
    two lore tools that back `bundle` and `search`. Everything the student will
    never own -- edit/write, webfetch/websearch, glob/list, the task subagent --
    is denied here rather than filtered afterwards, because a call the teacher
    never makes is a trajectory that never has to be thrown away.
    """
    model = cfg["teacher"]["model"]
    variant = cfg["teacher"]["variant"]
    provider_block: dict = {}
    if model.startswith("openai/"):
        # The effort has to follow the configured variant. Hardcoding one here
        # while passing another on the command line leaves two answers to the
        # same question in the same cell, and which of them the teacher
        # actually ran at is not recoverable from the recording.
        provider_block = {"openai": {"models": {
            model.split("/", 1)[1]: {"options": {"reasoningEffort": variant}}}}}
    doc = {
        "$schema": "https://opencode.ai/config.json",
        "model": model,
        "share": "disabled",
        "autoupdate": False,
        "snapshot": False,
        # `lore_expand` and `lore_status` are on the MCP server but not on the
        # student's five-tool surface, so a teacher that calls one costs the
        # whole trajectory to `forbidden_tool`. Decision 8: deny at the source,
        # because a call the teacher never makes is a trajectory that never has
        # to be thrown away. `expand` is the live risk -- lore's own `search`
        # description tells the caller to expand a hit before quoting it.
        "tools": {"webfetch": False, "websearch": False, "glob": False,
                  "list": False, "todowrite": False,
                  "lore_expand": False, "lore_status": False,
                  "task": bool(cfg["teacher"]["allow_task_subagent"])},
        "permission": {
            "webfetch": "deny", "websearch": "deny", "question": "deny",
            "edit": "deny", "write": "deny", "patch": "deny",
            "glob": "deny", "list": "deny", "todowrite": "deny",
            "task": "allow" if cfg["teacher"]["allow_task_subagent"] else "deny",
            "read": "allow", "grep": "allow", "bash": "allow",
            "external_directory": "deny",
        },
        "mcp": {"lore": {"type": "local",
                         "command": [cfg.get_path("lore", "mcp_bin") or
                                     cfg["lore"]["mcp_bin"]],
                         "enabled": True}},
    }
    if provider_block:
        doc["provider"] = provider_block
    path = os.path.join(out_dir, "opencode.json")
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(doc, handle, indent=2)
    return path


def port_pool(base: int, workers: int) -> "queue.Queue[int]":
    """One port per concurrent cell, leased for the whole of that cell.

    `opencode run --port N` binds a server, so two cells sharing a port collide.
    Deriving the port from the question's index does not prevent that: a slow
    cell still holds its port when a fast neighbour laps it and is handed the
    same number. A pool of exactly `workers` ports, each held from the start of
    a cell to its end, cannot issue one twice at once.
    """
    pool: "queue.Queue[int]" = queue.Queue()
    for slot in range(workers):
        pool.put(base + slot)
    return pool


@contextlib.contextmanager
def leased(pool: "queue.Queue[int]"):
    port = pool.get()
    try:
        yield port
    finally:
        pool.put(port)


TOKEN_FIELDS = ("input", "output", "reasoning", "cache_read")


def teacher_tokens(ndjson: str) -> dict:
    """What the teacher spent on one cell, summed over its model steps.

    opencode emits a `step_finish` event per model step carrying that step's
    `tokens` block -- `input`, `output`, `reasoning`, and a nested `cache`
    with `read`/`write`. Summing over steps is the honest cost of the cell:
    a multi-step tool-calling session re-sends its whole transcript every
    step, so `input` counts the same conditioning many times over, which is
    exactly what the provider bills for. `cache_read` is the part of that
    input which was served from cache, and `steps` is the divisor that makes
    the rest interpretable.

    A partial or torn log is not an error here: a cell that crashed still has
    the steps it managed, and a missing count is reported as zero rather than
    failing the cell it is only annotating.
    """
    totals = {field: 0 for field in TOKEN_FIELDS}
    totals["steps"] = 0
    try:
        handle = open(ndjson, encoding="utf-8")
    except OSError:
        return totals
    with handle:
        for line in handle:
            line = line.strip()
            if not line or "step_finish" not in line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue          # a torn final line costs its own step, not the cell
            if event.get("type") != "step_finish":
                continue
            tokens = (event.get("part") or {}).get("tokens") or {}
            cache = tokens.get("cache") or {}
            totals["steps"] += 1
            totals["input"] += int(tokens.get("input") or 0)
            totals["output"] += int(tokens.get("output") or 0)
            totals["reasoning"] += int(tokens.get("reasoning") or 0)
            totals["cache_read"] += int(cache.get("read") or 0)
    return totals


def run_cell(cfg: Config, question: dict, pin: RepoPin, raw_dir: str,
             port: int) -> dict:
    """One teacher session. Everything it produced lands under `raw_dir/<qid>`."""
    out_dir = os.path.join(raw_dir, question["qid"])
    shutil.rmtree(out_dir, ignore_errors=True)
    os.makedirs(out_dir, exist_ok=True)

    snapshot = os.path.join(cfg.get_path("paths", "snapshots"), pin.snapshot)
    prompt = TEACHER_PROMPT.format(question=question["question"])
    with open(os.path.join(out_dir, "prompt.txt"), "w", encoding="utf-8") as handle:
        handle.write(prompt)
    config_path = opencode_config(cfg, out_dir)

    env = dict(os.environ)
    env["OPENCODE_CONFIG"] = config_path
    # The MCP server scopes itself to one project. Pinning it by name here
    # removes the only ambiguity in the whole cell: which index answered.
    env["LORE_PROJECT"] = pin.project_key or pin.lore_project

    argv = [cfg["teacher"]["opencode_bin"], "run", "--pure", "--format", "json",
            "--variant", cfg["teacher"]["variant"], "--port", str(port),
            "--auto", "--dir", snapshot, "--", prompt]

    started = time.monotonic()
    ndjson = os.path.join(out_dir, "agent.ndjson")
    # stdin MUST be /dev/null: with an open pipe, `opencode run` waits for EOF
    # after init and produces nothing at all.
    with open(ndjson, "wb") as sink, open(os.devnull, "rb") as devnull, \
            open(os.path.join(out_dir, "agent.stderr"), "wb") as errsink:
        try:
            code = subprocess.call(argv, stdin=devnull, stdout=sink,
                                   stderr=errsink, env=env,
                                   timeout=cfg["teacher"]["timeout_s"])
            status = "OK" if code == 0 else f"EXIT_{code}"
        except subprocess.TimeoutExpired:
            code, status = -1, "TIMEOUT"
    wall_s = round(time.monotonic() - started, 1)

    meta = cell_meta(cfg, question, pin, status=status, exit_code=code,
                     wall_s=wall_s, tokens=teacher_tokens(ndjson))
    with open(os.path.join(out_dir, "meta.json"), "w", encoding="utf-8") as handle:
        json.dump(meta, handle, indent=2)
    return meta


def completed(raw_dir: str, qid: str) -> bool:
    """Whether `qid` already has a finished cell on disk.

    "Finished" means the run exited cleanly and left events behind: a timed-out
    or crashed cell is worth re-running, an empty `agent.ndjson` is worth
    re-running, and a good one is not.
    """
    cell = os.path.join(raw_dir, qid)
    try:
        with open(os.path.join(cell, "meta.json"), encoding="utf-8") as handle:
            meta = json.load(handle)
    except (OSError, json.JSONDecodeError):
        return False
    ndjson = os.path.join(cell, "agent.ndjson")
    return (meta.get("status") == "OK" and os.path.exists(ndjson)
            and os.path.getsize(ndjson) > 0)


def cell_meta(cfg: Config, question: dict, pin: RepoPin, **extra) -> dict:
    meta = {
        "qid": question["qid"],
        "question": question["question"],
        "question_sha12": question["question_sha12"],
        "qa_class": question.get("qa_class", ""),
        "teacher": f"{cfg['teacher']['model']}@{cfg['teacher']['variant']}",
        "question_source": cfg["questions"]["hf_dataset"],
        "generated_utc": common.utcnow(),
    }
    meta.update(pin.as_meta())
    meta.update(extra)
    return meta


# --------------------------------------------------------------------------- #
# Dry run
# --------------------------------------------------------------------------- #

DRY_QUESTIONS = [
    {
        "qid": "example__repo#00", "repo": "example/repo",
        "commit": "0" * 40,
        "question": "Where is the chunker plugin registry initialised, and what "
                    "does it register by default?",
        "reference_answer":
            "The registry is built in ChunkerRegistry::new() "
            "(src/registry.py lines 21-25), which pushes the default "
            "TreeSitterChunker. Its only production call site is "
            "src/index.py line 88.",
        "qa_class": "Where (Localization Queries)",
    },
    {
        "qid": "example__repo#01", "repo": "example/repo",
        "commit": "0" * 40,
        "question": "How does a search request reach the ranker?",
        "reference_answer":
            "Requests enter through src/http.py lines 40-70 and are handed to "
            "the ranker in src/rank.py lines 10-30.",
        "qa_class": "How (Procedural Queries)",
    },
]


def _part(msg_id: str, ptype: str, **fields) -> dict:
    return {"type": ptype, "timestamp": 0, "sessionID": "ses_dry",
            "part": {"messageID": msg_id, "type": ptype, **fields}}


def dry_events(question: dict, snapshot: str) -> list[dict]:
    """A hand-built event stream in exactly opencode's shape.

    It is deliberately awkward on purpose: the `read` call carries the absolute
    snapshot path, so convert.py's normaliser has real work to do, and the
    trailing `bash` call carries the same root inside a shell string, which is
    the form that survives naive rewriting.
    """
    m1, m2, m3 = "msg_1", "msg_2", "msg_3"
    return [
        _part(m1, "text", text="I'll start from the index bundle."),
        _part(m1, "tool_use", tool="lore_bundle", callID="call_1", state={
            "status": "completed",
            # An expansion of the question, never a copy of it -- see the
            # `question_echoed_verbatim` gate in convert.py.
            "input": {"query": "chunker plugin registry construction, default "
                               "registered plugins, and the call sites that "
                               "build it during indexing",
                      "limit": 12, "budget_tokens": 4000},
            "output": "VERDICT: found (2 verified locations, 0 dropped)\n"
                      "=== src/registry.py:12-25 [ChunkerRegistry] ===\n"
                      "12  class ChunkerRegistry:\n"
                      "21      def __init__(self):\n"
                      "22          self.plugins = [TreeSitterChunker()]\n"
                      "=== src/index.py:80-92 [build_index] ===\n"
                      "88      registry = ChunkerRegistry()\n"}),
        _part(m2, "text",
              text="The bundle points at the constructor. Reading it in full."),
        _part(m2, "tool_use", tool="read", callID="call_2", state={
            "status": "completed",
            "input": {"filePath": f"{snapshot}/src/registry.py",
                      "offset": 0, "limit": 40},
            "output": f"<file>\n{snapshot}/src/registry.py\n"
                      "12| class ChunkerRegistry:\n"
                      "21|     def __init__(self):\n"
                      "22|         self.plugins = [TreeSitterChunker()]\n"
                      "</file>"}),
        _part(m3, "text", text="Confirming there is no other call site."),
        _part(m3, "tool_use", tool="grep", callID="call_3", state={
            "status": "completed",
            "input": {"pattern": "ChunkerRegistry\\(", "include": "*.py",
                      "path": snapshot},
            "output": "src/index.py:88:    registry = ChunkerRegistry()"}),
        _part(m3, "tool_use", tool="bash", callID="call_4", state={
            "status": "completed",
            "input": {"command": f"wc -l {snapshot}/src/registry.py",
                      "workdir": snapshot, "timeout": 120000},
            "output": "120 src/registry.py"}),
        _part("msg_4", "text", text=(
            "The chunker plugin registry is initialised in "
            "`ChunkerRegistry.__init__` at src/registry.py:21-25, which "
            "registers a single default `TreeSitterChunker`. Its only "
            "production call site is src/index.py:88, inside `build_index`. "
            "Requests reach it via src/http.py:40-70 and src/rank.py:10-30.")),
        {"type": "step_finish", "timestamp": 0, "sessionID": "ses_dry",
         "part": {"messageID": "msg_4", "type": "step-finish", "reason": "stop",
                  "tokens": {"total": 0, "input": 0, "output": 0, "reasoning": 0,
                             "cache": {"read": 0, "write": 0}}}},
    ]


def run_dry(cfg: Config, batch: str) -> int:
    workspace = cfg.get_path("paths", "workspace")
    raw_dir = os.path.join(workspace, "raw", batch)
    snap_root = cfg.get_path("paths", "snapshots")
    pins, questions = [], []
    for row in DRY_QUESTIONS:
        row = dict(row, question_sha12=common.sha12(row["question"]))
        questions.append(row)
    pin = pin_repo(cfg, DRY_QUESTIONS[0]["repo"], DRY_QUESTIONS[0]["commit"],
                   dry_run=True)
    pins.append(pin)
    snapshot = os.path.join(snap_root, pin.snapshot)

    for question in questions:
        out_dir = os.path.join(raw_dir, question["qid"])
        shutil.rmtree(out_dir, ignore_errors=True)
        os.makedirs(out_dir, exist_ok=True)
        with open(os.path.join(out_dir, "agent.ndjson"), "w",
                  encoding="utf-8") as handle:
            for event in dry_events(question, snapshot):
                handle.write(json.dumps(event) + "\n")
        meta = cell_meta(cfg, question, pin, status="OK", exit_code=0,
                         wall_s=0.0, dry_run=True,
                         tokens=teacher_tokens(
                             os.path.join(out_dir, "agent.ndjson")))
        with open(os.path.join(out_dir, "meta.json"), "w",
                  encoding="utf-8") as handle:
            json.dump(meta, handle, indent=2)
        print(f"  {question['qid']:<22} DRY  {out_dir}")

    # The reference answers have to travel with the batch: grade.py needs them
    # and a dry run has no dataset file to read them back from.
    qfile = os.path.join(workspace, "questions", f"{batch}.jsonl")
    common.write_jsonl(qfile, questions)
    common.write_manifest(workspace, batch,
                          {"model": "<dry-run>", "variant": "<dry-run>"},
                          pins, {"dry_run": True, "questions_file": qfile})
    print(f"\nwrote {len(questions)} dry trajectories to {raw_dir}")
    print(f"manifest: {common.manifest_path(workspace, batch)}")
    print(f"\nnext:  python3 convert.py --batch {batch}")
    return 0


# --------------------------------------------------------------------------- #

def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--config", default=None)
    ap.add_argument("--batch", default=None, help="batch name (default from config)")
    ap.add_argument("--repos", default="", help="comma-separated owner/name filter")
    ap.add_argument("--limit-per-repo", type=int, default=None)
    ap.add_argument("--concurrency", type=int, default=None,
                    help="cells in flight at once (default from config)")
    ap.add_argument("--dry-run", action="store_true",
                    help="fabricate opencode-shaped trajectories; call nothing")
    ap.add_argument("--prepare-only", action="store_true",
                    help="check out snapshots and write the manifest, then stop")
    ap.add_argument("--resume", action="store_true",
                    help="skip cells that already completed; re-run the rest")
    args = ap.parse_args(argv)

    cfg = common.load_config(args.config)
    if args.concurrency is not None:
        cfg["teacher"]["concurrency"] = max(1, args.concurrency)
    batch = args.batch or cfg["batch"]["name"]
    print(f"config: {cfg.source}   batch: {batch}")

    if args.dry_run:
        return run_dry(cfg, batch)

    repos = [r for r in args.repos.split(",") if r] or list(cfg["questions"]["repos"])
    limit = (args.limit_per_repo if args.limit_per_repo is not None
             else cfg["questions"]["limit_per_repo"])
    questions = load_questions(cfg, repos, limit)
    if not questions:
        raise SystemExit("no questions selected")

    workspace = cfg.get_path("paths", "workspace")
    raw_dir = os.path.join(workspace, "raw", batch)
    os.makedirs(raw_dir, exist_ok=True)

    wanted = {}
    for question in questions:
        wanted.setdefault(question["repo"], question["commit"])
    print(f"{len(questions)} questions over {len(wanted)} repos")

    pins = {}
    for repo, commit in wanted.items():
        pin = pin_repo(cfg, repo, commit, dry_run=False)
        pins[repo] = pin
        print(f"  pinned {repo}@{commit[:8]}  lore={pin.lore_project} "
              f"key={pin.project_key} gen={pin.index_generation} "
              f"chunks={pin.chunks} embedded={pin.embedded_chunks}")

    qfile = os.path.join(workspace, "questions", f"{batch}.jsonl")
    common.write_jsonl(qfile, questions)
    common.write_manifest(
        workspace, batch,
        {"model": cfg["teacher"]["model"], "variant": cfg["teacher"]["variant"],
         "prompt_sha12": common.sha12(TEACHER_PROMPT)},
        list(pins.values()), {"questions_file": qfile})

    if args.prepare_only:
        print(f"\nprepared only; manifest at "
              f"{common.manifest_path(workspace, batch)}")
        return 0

    if args.resume:
        # A cell is a teacher call, and a teacher call is the expensive thing
        # here. Re-running a batch to add the cells that failed -- or to finish
        # one that was inspected after its first cell -- must not re-spend the
        # ones that already succeeded.
        pending = [q for q in questions if not completed(raw_dir, q["qid"])]
        print(f"resume: {len(questions) - len(pending)} cells already complete, "
              f"{len(pending)} to run")
        questions = pending
        if not questions:
            print("nothing to do")
            return 0

    workers = max(1, int(cfg["teacher"]["concurrency"]))
    ports = port_pool(int(cfg["teacher"]["port"]), workers)

    def cell(question: dict) -> dict:
        with leased(ports) as port:
            return run_cell(cfg, question, pins[question["repo"]], raw_dir, port)

    started = time.monotonic()
    print(f"\n{'qid':<24}{'wall_s':>8}  status")
    done, metas = 0, []
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(cell, question): question for question in questions}
        for future in concurrent.futures.as_completed(futures):
            question = futures[future]
            try:
                meta = future.result()
            except Exception as exc:  # one dead cell must not take the batch
                meta = {"qid": question["qid"], "wall_s": 0,
                        "status": f"HARNESS_ERROR: {type(exc).__name__}: {exc}"}
            metas.append(meta)
            done += 1
            print(f"{meta.get('qid', '?'):<24}{meta.get('wall_s', 0):>8}  "
                  f"{meta.get('status')}   [{done}/{len(questions)}]", flush=True)
    wall_s = round(time.monotonic() - started, 1)

    ok = [m for m in metas if m.get("status") == "OK"]
    totals = {field: sum(int((m.get("tokens") or {}).get(field) or 0)
                         for m in metas) for field in TOKEN_FIELDS}
    steps = sum(int((m.get("tokens") or {}).get("steps") or 0) for m in metas)
    billed = totals["input"] + totals["output"]
    print(f"\n{len(ok)}/{len(questions)} cells OK in {wall_s}s wall "
          f"at concurrency {workers}")
    print(f"teacher tokens: in={totals['input']} out={totals['output']} "
          f"reasoning={totals['reasoning']} cache_read={totals['cache_read']} "
          f"over {steps} steps")
    if ok:
        print(f"  {billed} in+out total, {billed // len(ok)} per completed cell")
    print(f"\nraw trajectories in {raw_dir}")
    print(f"next:  python3 convert.py --batch {batch}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
