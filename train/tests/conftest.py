"""Test scaffolding for the scouter trajectory harness.

`train/` is a directory of scripts, not a package, so the modules under test are
imported by putting `train/` on `sys.path` here rather than by relative import.

Every fixture builds its own config from `common.DEFAULTS` via a temporary TOML
file. Nothing reads the operator's `train/config.toml`: a test whose result
depends on a gitignored file on one machine is not a test.
"""

from __future__ import annotations

import copy
import json
import os
import sys

import pytest

TESTS = os.path.dirname(os.path.abspath(__file__))
TRAIN = os.path.dirname(TESTS)
FIXTURES = os.path.join(TESTS, "fixtures")

if TRAIN not in sys.path:
    sys.path.insert(0, TRAIN)

import common  # noqa: E402  (path must be set first)


@pytest.fixture
def cfg(tmp_path):
    """A `Config` that is exactly `common.DEFAULTS`, rooted in a tmpdir.

    Written through `load_config` rather than constructed by hand, so the test
    exercises the same merge the harness does.

    The deep copy is a workaround, not decoration: `common._merge` copies only
    one level, so any section the TOML does not mention is returned *aliased to
    `common.DEFAULTS`* and a test that tunes a threshold would tune it for every
    later test in the process. See `test_config.py`.
    """
    path = tmp_path / "config.toml"
    path.write_text(
        "[paths]\n"
        f'workspace = "{(tmp_path / "work").as_posix()}"\n'
        f'snapshots = "{(tmp_path / "work" / "snapshots").as_posix()}"\n'
        "[validate]\n"
        'script = ""\n',
        encoding="utf-8",
    )
    loaded = common.load_config(str(path))
    isolated = common.Config(copy.deepcopy(dict(loaded)), loaded.source)
    return isolated


# --------------------------------------------------------------------------- #
# opencode event-stream builders
#
# The shapes here mirror `--format json` exactly: a `text` or `tool_use` event,
# each carrying a `part` tagged with the `messageID` it belongs to, and a
# `tool_use` part carrying `callID`, `state.input` and `state.output`.
# --------------------------------------------------------------------------- #

def text_event(msg_id: str, text: str) -> dict:
    return {"type": "text", "timestamp": 0, "sessionID": "ses_test",
            "part": {"messageID": msg_id, "type": "text", "text": text}}


def tool_event(msg_id: str, tool: str, call_id: str, args: dict,
               output: str | None = "", *, omit_output: bool = False,
               status: str = "completed") -> dict:
    state: dict = {"status": status, "input": args}
    if not omit_output:
        state["output"] = output
    return {"type": "tool_use", "timestamp": 0, "sessionID": "ses_test",
            "part": {"messageID": msg_id, "type": "tool_use", "tool": tool,
                     "callID": call_id, "state": state}}


def minimal_trajectory(question: str = "How does the ranker get its input?",
                       *, bundle_query: str = "ranker input plumbing, the call "
                                              "sites that construct it, and the "
                                              "request path that reaches it",
                       answer: str = "Requests enter at src/http.py:40-70 and "
                                     "are ranked in src/rank.py:10-30.",
                       ) -> list[dict]:
    """A structurally valid two-call trajectory: bundle first, prose last."""
    return [
        text_event("m1", "Starting from the index bundle."),
        tool_event("m1", "lore_bundle", "call_1", {"query": bundle_query},
                   "VERDICT: found\n=== src/http.py:40-70 [handle] ===\n"),
        text_event("m2", "Confirming the ranker entry point."),
        tool_event("m2", "read", "call_2", {"filePath": "src/rank.py"},
                   "10| def rank(...):\n"),
        text_event("m3", answer),
    ]


def meta_for(question: str, qid: str = "example__repo#00") -> dict:
    return {"qid": qid, "question": question, "repo": "example/repo",
            "commit": "0" * 40, "lore_project": "example__repo",
            "lore_project_key": "test-key", "index_generation": 7}


def fixture_events(name: str) -> list[dict]:
    """Parse one of the committed NDJSON fixture logs through the real parser."""
    import convert
    return convert.parse_events(os.path.join(FIXTURES, name))


def write_events(path, events: list[dict]) -> str:
    with open(path, "w", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event) + "\n")
    return str(path)
