"""Shared pieces of the scouter trajectory harness.

Everything that more than one stage needs lives here: the config loader, the
frozen scouter system prompt and tool surface, the path normaliser, the batch
manifest, and the wrapper that hands a finished JSONL to the external
validator.

Nothing in `train/` imports from `bench/rcb/`. See README.md, "Separation from
bench/rcb".
"""

from __future__ import annotations

import copy
import dataclasses
import datetime as _dt
import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))

# --------------------------------------------------------------------------- #
# The frozen surface.  Both constants below are byte-identical in every emitted
# row -- the validator enforces it for `tools`, and the trainer's conditioning
# depends on it for the system prompt.  Changing either is a dataset version
# bump, not an edit.
# --------------------------------------------------------------------------- #

SCOUT_SYSTEM_PROMPT = (
    "You are CodeScout, a read-only repository scout. Answer the user's "
    "question about this repository.\n"
    "\n"
    "Always call `bundle` first: it returns a pre-assembled, mechanically "
    "verified context bundle from the local index, and it is cheaper and more "
    "reliable than exploring blind. Then explore with the remaining read-only "
    "tools until you can answer.\n"
    "\n"
    "Finish with a direct answer that cites its evidence as repository-relative "
    "`path:start-end` line spans. Never cite a path you have not seen, and "
    "never write an absolute path."
)

# Section 5.3 of the emission spec, verbatim -- the same five schemas the
# masking smoke was proven against.  `arguments` on the wire is always a JSON
# *string*; these are the declarations, not the calls.
TOOLS = [
    {"type": "function", "function": {
        "name": "bundle",
        "description": "Return a pre-assembled context bundle for a question: "
                       "ranked file/chunk excerpts from the lore index.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string", "description": "Natural-language question."},
            "limit": {"type": "integer", "description": "Max chunks to return."}},
            "required": ["query"]}}},
    {"type": "function", "function": {
        "name": "search",
        "description": "Semantic search over the lore index. Returns ranked chunks.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string"},
            "limit": {"type": "integer"}},
            "required": ["query"]}}},
    {"type": "function", "function": {
        "name": "grep",
        "description": "Literal / regex search across indexed files.",
        "parameters": {"type": "object", "properties": {
            "pattern": {"type": "string"},
            "glob": {"type": "string"}},
            "required": ["pattern"]}}},
    {"type": "function", "function": {
        "name": "read",
        "description": "Read a file, optionally a line range.",
        "parameters": {"type": "object", "properties": {
            "path": {"type": "string"},
            "start": {"type": "integer"},
            "end": {"type": "integer"}},
            "required": ["path"]}}},
    {"type": "function", "function": {
        "name": "bash",
        "description": "Run a read-only shell command (ls, find, wc, rg).",
        "parameters": {"type": "object", "properties": {
            "command": {"type": "string"}},
            "required": ["command"]}}},
]

TOOL_NAMES = {t["function"]["name"] for t in TOOLS}


# --------------------------------------------------------------------------- #
# Config
# --------------------------------------------------------------------------- #

DEFAULTS: dict = {
    "batch": {"name": "pilot-01"},
    "teacher": {
        "model": "openai/gpt-5.6-luna",
        "variant": "max",
        "opencode_bin": "opencode",
        "port": 4177,
        "timeout_s": 1800,
        "concurrency": 1,
        "allow_task_subagent": False,
    },
    "lore": {
        "mcp_bin": "lore-mcp",
        "state_dir": "",
        "daemon_url": "",
        "project_prefix": "",
        "bundle_limit": 0,
        "bundle_budget_tokens": 0,
    },
    "paths": {
        "workspace": "./work",
        "snapshots": "./work/snapshots",
    },
    "questions": {
        "file": "",
        "hf_dataset": "TIGER-Lab/SWE-QA-Pro-Bench",
        "hf_file": "data/test.jsonl",
        "repos": [],
        "limit_per_repo": 0,
        "clone_url_template": "https://github.com/{repo}.git",
    },
    "emit": {
        "max_tool_chars": 4000,
        "max_bundle_chars": 12000,
        # 32k: the 2026-08-27 context smoke proved a 4B bf16 LoRA trains a
        # 32,768 budget in ~12 GiB (padding_free off); 8192 kept 0 of the
        # pilot's 4 rows. 60 calls: the pilot's ONLY reject reason at 30 was
        # too_many_tool_calls, and the cell it rejected at 31 graded 1.00/1.00.
        "max_length": 32768,
        "max_tool_calls": 60,
        "drop_tools": ["todowrite"],
        "allow_question_echo": False,
    },
    "grade": {
        "min_file_recall": 0.5,
        "min_span_hit_rate": 0.34,
        "line_tolerance": 20,
        "min_tool_calls": 2,
        "keep_ungradeable": False,
    },
    "validate": {
        "python": "python3",
        "script": "~/lora-prep/validate_dataset.py",
        # Per-row token counter for the over_length drop gate; empty = gate
        # off, validator remains the backstop.
        "counter": "",
        "sample": 25,
    },
}


def _merge(base: dict, over: dict) -> dict:
    # Deep-copied, not aliased: a section the TOML never mentions must come
    # back as a private copy, or the first caller to write through the config
    # (an override flag, a second batch in one process) silently rewrites the
    # module-level DEFAULTS for everyone after it.
    out = {k: copy.deepcopy(v) for k, v in base.items()}
    for key, value in over.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], value)
        else:
            out[key] = value
    return out


class Config(dict):
    """Dotted read-only-ish view over the merged TOML."""

    def __init__(self, data: dict, source: str):
        super().__init__(data)
        self.source = source

    def get_path(self, section: str, key: str) -> str:
        """A configured path, `~`-expanded and made absolute against train/."""
        raw = self[section][key]
        if not raw:
            return ""
        raw = os.path.expanduser(os.path.expandvars(raw))
        return raw if os.path.isabs(raw) else os.path.normpath(os.path.join(HERE, raw))


def load_config(path: str | None) -> Config:
    data, source = {}, "<defaults>"
    if path:
        source = os.path.abspath(os.path.expanduser(path))
        with open(source, "rb") as handle:
            data = tomllib.load(handle)
    else:
        for candidate in ("config.toml", "config.example.toml"):
            candidate = os.path.join(HERE, candidate)
            if os.path.exists(candidate):
                source = candidate
                with open(candidate, "rb") as handle:
                    data = tomllib.load(handle)
                break
    return Config(_merge(DEFAULTS, data), source)


# --------------------------------------------------------------------------- #
# Identity
# --------------------------------------------------------------------------- #

def slug(repo: str) -> str:
    """`owner/name` -> `owner__name`. Used for snapshot dirs and question ids."""
    return repo.replace("/", "__")


def question_id(repo: str, index: int) -> str:
    return f"{slug(repo)}#{index:02d}"


def sha12(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:12]


def utcnow() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# --------------------------------------------------------------------------- #
# Path normalisation
#
# The single most expensive defect found in the SWE-QA-Pro conversion was that
# 92.8% of its supervised tool calls carried the generating harness's own repo
# root (`repos_tmp/worker_646683/...`).  A model trained on that learns to
# prefix every path with a directory that will not exist at inference.  So this
# harness normalises on the way in and then *asserts* -- a trajectory whose
# supervised tokens still carry an absolute path is rejected, not repaired.
# --------------------------------------------------------------------------- #

# Deliberately broad: anything that looks rooted, plus Windows drive letters and
# `~`.  False positives are cheap (one rejected trajectory); a false negative is
# a permanent defect baked into the weights.
ABS_PATH_RE = re.compile(
    r"""(?x)
    (?: (?<![A-Za-z]) [A-Za-z]:[\\/] ) # C:\ or C:/ -- the lookbehind keeps
                                       # `https://` from reading as drive `s:`
  | (?: ~/ )                           # home-relative
  | (?: (?<![\w:/~.\-])                # not a URL scheme, not mid-path, not
                                       # a repo-relative path like src/x/
        / (?: [\w.\-]+ / )+ )          # a rooted path with at least one dir
    """
)


def normalize_paths(text: str, roots: list[str]) -> tuple[str, int]:
    """Rewrite every configured snapshot root out of `text`.

    Returns the rewritten text and the number of substitutions. Roots are tried
    longest-first so a nested root cannot shadow its parent, and each root is
    matched in both slash conventions because a shell command may carry either.
    """
    if not text:
        return text, 0
    hits = 0
    for root in sorted({r for r in roots if r}, key=len, reverse=True):
        for form in {root, root.replace("\\", "/"), root.replace("/", "\\")}:
            for pattern, repl in ((form + "/", ""), (form + "\\", ""), (form, ".")):
                if pattern in text:
                    hits += text.count(pattern)
                    text = text.replace(pattern, repl)
    return text, hits


def absolute_leaks(text: str) -> list[str]:
    """Absolute-looking path fragments still present. Empty is the pass case.

    `/dev/` is exempt: `2>/dev/null` is portable shell plumbing, identical on
    every POSIX box, so a fragment under it names no machine and teaches the
    student nothing untrue. (The 2026-08-28 glm pilots had the blanket rule
    rejecting a third of otherwise-good cells for `/dev/null` redirects.)
    """
    return [m.group(0) for m in ABS_PATH_RE.finditer(text or "")
            if not m.group(0).startswith("/dev/")]


# --------------------------------------------------------------------------- #
# Manifest -- what a batch was generated against, so a row is reproducible
# --------------------------------------------------------------------------- #

@dataclasses.dataclass
class RepoPin:
    repo: str
    commit: str
    snapshot: str          # relative to [paths].snapshots -- never absolute
    lore_project: str
    project_key: str = ""
    index_generation: int = 0
    files: int = 0
    chunks: int = 0
    embedded_chunks: int = 0
    daemon_version: str = ""
    dry_run: bool = False

    def as_meta(self) -> dict:
        """The subset that rides in every trajectory's `meta`."""
        return {
            "repo": self.repo,
            "commit": self.commit,
            "lore_project": self.lore_project,
            "lore_project_key": self.project_key,
            "index_generation": self.index_generation,
        }


def manifest_path(workspace: str, batch: str) -> str:
    return os.path.join(workspace, "manifests", f"{batch}.json")


def write_manifest(workspace: str, batch: str, teacher: dict,
                   pins: list[RepoPin], extra: dict | None = None) -> str:
    path = manifest_path(workspace, batch)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    doc = {
        "batch": batch,
        "created_utc": utcnow(),
        "teacher": teacher,
        "repos": [dataclasses.asdict(p) for p in pins],
    }
    doc.update(extra or {})
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(doc, handle, indent=2)
        handle.write("\n")
    return path


def read_manifest(workspace: str, batch: str) -> dict:
    with open(manifest_path(workspace, batch), encoding="utf-8") as handle:
        return json.load(handle)


def pins_by_repo(manifest: dict) -> dict[str, RepoPin]:
    return {r["repo"]: RepoPin(**r) for r in manifest["repos"]}


# --------------------------------------------------------------------------- #
# lore daemon (read-only; registration and indexing stay the operator's job)
# --------------------------------------------------------------------------- #

class DaemonError(RuntimeError):
    pass


def daemon_base(cfg: Config) -> str:
    url = cfg["lore"]["daemon_url"]
    if url:
        return url.rstrip("/")
    state = cfg.get_path("lore", "state_dir") or os.path.expanduser(
        os.environ.get("LORE_STATE_DIR") or "~/.local/share/lore")
    try:
        with open(os.path.join(state, "daemon.json"), encoding="utf-8") as handle:
            port = json.load(handle)["port"]
    except (OSError, KeyError, json.JSONDecodeError) as exc:
        raise DaemonError(f"cannot read the lore daemon port from {state}: {exc}") from exc
    return f"http://127.0.0.1:{port}/v1"


def daemon_get(base: str, route: str, timeout: float = 15.0) -> dict:
    try:
        with urllib.request.urlopen(f"{base}{route}", timeout=timeout) as resp:
            return json.load(resp)
    except Exception as exc:  # noqa: BLE001 - any failure here is "daemon unusable"
        raise DaemonError(f"GET {base}{route}: {exc}") from exc


# --------------------------------------------------------------------------- #
# Validator
# --------------------------------------------------------------------------- #

def token_lengths(cfg: Config, rows: list[dict]) -> list[int] | None:
    """Exact rendered token length per row, or None when no counter is wired.

    The counter runs under the validator's own python with the validator's own
    template, so the number it reports is the number the validator will later
    check against --max-length. It is an *optimization* for the drop gate, not
    the gate itself: with no counter the rows still hit the validator, which
    fails the batch loudly rather than shipping an over-length row.
    """
    counter = cfg.get_path("validate", "counter")
    python = os.path.expanduser(cfg["validate"]["python"])
    if not counter or not os.path.exists(counter) or not os.path.exists(python):
        return None
    import tempfile
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False,
                                     encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row) + "\n")
        tmp = handle.name
    try:
        proc = subprocess.run([python, "-u", counter, tmp],
                              capture_output=True, text=True, timeout=600)
        if proc.returncode != 0:
            print(f"!! token counter failed ({proc.returncode}): "
                  f"{proc.stderr.strip()[:300]}", file=sys.stderr)
            return None
        lengths = json.loads(proc.stdout)
        if not isinstance(lengths, list) or len(lengths) != len(rows):
            print("!! token counter returned a malformed length list",
                  file=sys.stderr)
            return None
        return [int(n) for n in lengths]
    finally:
        os.unlink(tmp)


def run_validator(cfg: Config, jsonl: str, extra_args: list[str] | None = None) -> int:
    """Hand a finished JSONL to `validate_dataset.py`; return its exit code.

    The validator is deliberately *not* vendored. It is the same file that
    proved the masking on the SWE-QA-Pro conversion, and one copy means the two
    corpora cannot drift apart on what "valid" means.
    """
    script = cfg.get_path("validate", "script")
    python = os.path.expanduser(cfg["validate"]["python"])
    if not script or not os.path.exists(script):
        print(f"!! validator not found at {script!r}; skipping "
              f"(set [validate].script)", file=sys.stderr)
        return 0
    argv = [python, "-u", script, jsonl,
            "--sample", str(cfg["validate"]["sample"]),
            "--max-length", str(cfg["emit"]["max_length"])]
    argv += extra_args or []
    print(f"\n$ {' '.join(argv)}", flush=True)
    return subprocess.call(argv)


# --------------------------------------------------------------------------- #
# Small IO helpers
# --------------------------------------------------------------------------- #

def read_jsonl(path: str) -> list[dict]:
    with open(path, encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def write_jsonl(path: str, rows: list[dict]) -> str:
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, ensure_ascii=False) + "\n")
    return path
