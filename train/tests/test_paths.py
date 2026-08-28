"""Path normalisation and the absolute-path leak gate (README, Decision 5).

This is the seam the README calls "the single most expensive defect" in the
SWE-QA-Pro conversion: 92.8% of its supervised tool calls carried the generating
harness's own repo root. The contract here is three-part --

  1. every snapshot root is rewritten out of arguments, assistant prose and
     tool results;
  2. supervised strings are re-scanned and a survivor is *rejected, not
     repaired*;
  3. survivors in masked tool results are counted, not rejected.

-- plus the detector's declared positive and negative controls: `https://` must
not read as drive `s:`, and `and/or` / `src/qibo/` must not read as rooted.
"""

from __future__ import annotations

import json

import pytest

import common
import convert
from conftest import fixture_events, meta_for, minimal_trajectory, tool_event, \
    text_event

SNAP = "/snap/example__repo"


# --------------------------------------------------------------------------- #
# The detector: negative controls
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("text", [
    "src/qibo/models/circuit.py",
    "qibo/models/circuit.py",
    "and/or",
    "docs/architecture.md",
    "a/b/c",
    "read the file at src/registry.py:21-25",
    "src/registry.py",
    "tests/fixtures/single_call.ndjson",
])
def test_repo_relative_paths_are_not_leaks(text):
    """A repo-relative path is exactly what the student should emit."""
    assert common.absolute_leaks(text) == []


@pytest.mark.parametrize("text", [
    "https://github.com/TIGER-Lab/SWE-QA-Pro-Bench",
    "https://opencode.ai/config.json",
    "http://127.0.0.1:4177/v1/status",
    "see https://example.com/a/b/ for details",
    '{"url": "https://huggingface.co/datasets/TIGER-Lab/SWE-QA-Pro-Bench"}',
])
def test_https_urls_are_not_mangled_as_drive_letters(text):
    """Pin the fixed bug: `https://` must not read as Windows drive `s:`.

    The lookbehind in `ABS_PATH_RE` is what buys this, and it is the difference
    between the gate being usable and every trajectory that names a URL being
    rejected.
    """
    assert common.absolute_leaks(text) == []


# --------------------------------------------------------------------------- #
# The detector: positive controls
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("text", [
    "/mnt/c/Users/someone/repo/src/x.py",          # WSL view of a Windows drive
    "/home/someone/lora-prep/data/test.jsonl",     # a WSL home
    "~/lora-prep/validate_dataset.py",             # home-relative
    "~/work/snapshots/qiboteam__qibo",
    "C:\\Users\\someone\\repo\\src\\x.py",         # Windows drive, backslashes
    "c:/work/snapshots/qiboteam__qibo",            # Windows drive, forward
    "D:/repos/qibo",
    "/usr/local/lib/python3.12/site-packages",
    "work/snapshots is fine but /snap/example__repo/src is not",
])
def test_absolute_paths_are_leaks(text):
    assert common.absolute_leaks(text), f"{text!r} slipped past the gate"


def test_dev_is_shell_plumbing_not_a_leak():
    """`2>/dev/null` is portable POSIX, identical on every box; treating it as
    a leak cost a third of the 2026-08-28 glm pilot cells for no privacy gain.
    The exemption is `/dev/` alone -- everything else stays a leak."""
    assert common.absolute_leaks("grep -r foo src/ 2>/dev/null") == []
    assert common.absolute_leaks("cat /dev/stdin") == []
    assert common.absolute_leaks("ls /usr/local/bin/ 2>/dev/null") == \
        ["/usr/local/bin/"]


def test_harness_snapshot_root_forms_are_all_caught():
    """The three shapes the README names: a rooted path, a drive, and `~/`."""
    for text in ("/snap/example__repo/src/x.py",
                 "C:\\snap\\example__repo\\src\\x.py",
                 "~/snap/example__repo/src/x.py"):
        assert common.absolute_leaks(text)


# --------------------------------------------------------------------------- #
# normalize_paths
# --------------------------------------------------------------------------- #

def test_snapshot_root_is_rewritten_out():
    text = f"{SNAP}/src/registry.py"
    out, hits = common.normalize_paths(text, [SNAP])
    assert out == "src/registry.py"
    assert hits == 1
    assert common.absolute_leaks(out) == []


def test_both_slash_conventions_are_rewritten():
    """A shell command may carry either convention; both must be rewritten."""
    win = "C:\\snap\\example__repo"
    out, hits = common.normalize_paths(
        "read C:\\snap\\example__repo\\src\\x.py and C:/snap/example__repo/src/y.py",
        [win])
    assert hits == 2
    assert common.absolute_leaks(out) == []
    assert "src\\x.py" in out and "src/y.py" in out


def test_bare_root_mention_becomes_a_dot():
    out, hits = common.normalize_paths(f"cd {SNAP} && ls", [SNAP])
    assert hits == 1
    assert out == "cd . && ls"


def test_longest_root_wins_over_its_parent():
    """A nested root must not be shadowed by the snapshot root above it."""
    parent, child = "/snap", "/snap/example__repo"
    out, _ = common.normalize_paths(f"{child}/src/x.py", [parent, child])
    assert out == "src/x.py"


def test_repo_relative_text_is_left_alone():
    out, hits = common.normalize_paths("src/registry.py:21-25", [SNAP])
    assert (out, hits) == ("src/registry.py:21-25", 0)


def test_empty_text_is_a_no_op():
    assert common.normalize_paths("", [SNAP]) == ("", 0)


# --------------------------------------------------------------------------- #
# The gate, through convert_one
# --------------------------------------------------------------------------- #

def test_absolute_path_smuggled_inside_a_tool_call_argument_is_caught(cfg):
    """A sibling checkout named inside a `bash` string is the form that survives
    naive rewriting -- the gate has to see it inside the JSON-encoded argument.
    """
    events = fixture_events("harness_paths.ndjson")
    row, reasons = convert.convert_one(
        cfg, meta_for("Where is the chunker registry built?"), events, [SNAP])
    assert row is None
    leak = next(r for r in reasons if r.startswith("abs_path_leak:"))
    assert "/snap/other__repo/" in leak


def test_this_rows_own_root_is_rewritten_rather_than_rejected(cfg):
    """Same fixture, but with the sibling root configured too: nothing leaks."""
    events = fixture_events("harness_paths.ndjson")
    row, reasons = convert.convert_one(
        cfg, meta_for("Where is the chunker registry built?"), events,
        [SNAP, "/snap/other__repo"])
    assert reasons == []
    blob = json.dumps(row["messages"], ensure_ascii=False)
    assert "/snap/" not in blob
    assert row["meta"]["path_rewrites"] >= 3


def test_harness_paths_never_survive_into_supervised_text(cfg):
    """The assertion the README makes: supervised strings carry no absolute path."""
    events = fixture_events("harness_paths.ndjson")
    row, _ = convert.convert_one(
        cfg, meta_for("Where is the chunker registry built?"), events,
        [SNAP, "/snap/other__repo"])
    supervised = []
    for message in row["messages"]:
        if message["role"] != "assistant":
            continue
        supervised.append(message["content"])
        for call in message.get("tool_calls") or []:
            supervised.append(call["function"]["arguments"])
    assert supervised
    for blob in supervised:
        assert common.absolute_leaks(blob) == [], blob


@pytest.mark.xfail(reason="README Decision 5 vs code: normalisation runs after "
                          "json.dumps, so a backslash root is doubled before it "
                          "is matched and is never rewritten")
def test_backslash_root_is_rewritten_out_of_an_encoded_argument():
    """KNOWN DISCREPANCY, pinned.

    Decision 5 says "every snapshot root ... is rewritten out of every
    argument", and `normalize_paths` documents that "each root is matched in
    both slash conventions". `convert_one` normalises *after* `json.dumps`, so a
    root containing backslashes has had them doubled by the encoder and no form
    of the root matches -- zero substitutions, and the trajectory is then
    rejected as `abs_path_leak:C:\\`.

    Unreachable in the documented environment (the pilot runs from WSL, where
    roots are POSIX). Reachable the moment anyone drives the harness natively on
    Windows, where `[paths].snapshots` resolves to a drive-rooted path.
    """
    root = "C:\\snap\\example__repo"
    blob = json.dumps({"path": root + "\\src\\registry.py"}, ensure_ascii=False)
    out, hits = common.normalize_paths(blob, [root])
    assert hits == 1 and "C:" not in out, (
        "the JSON-escaped root was not rewritten: " + out)


@pytest.mark.xfail(reason="normalisation runs after json.dumps; a backslash "
                          "root is doubled before it is matched -- see "
                          "test_backslash_root_survives_json_encoding")
def test_windows_drive_snapshot_root_is_normalised(cfg, tmp_path):
    """The harness may be driven from Windows; drive-rooted snapshots normalise."""
    root = "C:\\snap\\example__repo"
    events = [
        text_event("m1", "Starting from the index bundle."),
        tool_event("m1", "lore_bundle", "call_1",
                   {"query": "chunker plugin registry construction and defaults"},
                   "VERDICT: found\n"),
        text_event("m2", "Reading it."),
        tool_event("m2", "read", "call_2",
                   {"filePath": f"{root}\\src\\registry.py"}, "21| ...\n"),
        text_event("m3", "Built at src/registry.py:21-25."),
    ]
    row, reasons = convert.convert_one(
        cfg, meta_for("Where is the chunker registry built?"), events, [root])
    assert reasons == []
    args = json.loads(row["messages"][3]["tool_calls"][0]["function"]["arguments"])
    assert args["path"] == "src\\registry.py"


def test_masked_leaks_are_counted_not_rejected(cfg):
    """Decision 5, point 3: a survivor in a masked tool result is conditioning,
    not a target, so it is counted rather than thrown away."""
    events = minimal_trajectory()
    events[1]["part"]["state"]["output"] = (
        "VERDICT: found\n"
        "resolved from /mnt/c/Users/someone/checkout/src/http.py\n")
    row, reasons = convert.convert_one(
        cfg, meta_for("How does the ranker get its input?"), events, [SNAP])
    assert reasons == []
    assert row["meta"]["masked_abs_fragments"] == 1


def test_grep_scope_that_is_an_unrewritten_root_is_rejected(cfg):
    """`map_grep` leaves the leading slash on deliberately so the gate can see it."""
    events = minimal_trajectory()
    events.insert(3, tool_event("m2", "grep", "call_3",
                                {"pattern": "rank", "path": "/mnt/c/elsewhere/repo"},
                                "no matches"))
    row, reasons = convert.convert_one(
        cfg, meta_for("How does the ranker get its input?"), events, [SNAP])
    assert row is None
    assert any(r.startswith("abs_path_leak:") for r in reasons)
