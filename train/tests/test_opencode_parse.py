"""Reconstructing assistant turns from opencode's event stream (Decision 3).

The claim under test: grouping `text` and `tool_use` parts by `messageID` in
wire order reconstructs assistant turns exactly, gets parallel tool calls for
free, and leaves a parseable partial trajectory when a cell dies mid-stream.

The structural gates in Decision 4's first table are tested here too, because
they are what turns "not a trajectory" into a named reject reason rather than a
malformed row.
"""

from __future__ import annotations

import json
import os

import pytest

import common
import convert
from conftest import FIXTURES, fixture_events, meta_for, minimal_trajectory, \
    text_event, tool_event

QUESTION = "Where is the chunker plugin registry initialised?"


# --------------------------------------------------------------------------- #
# reconstruction
# --------------------------------------------------------------------------- #

def test_a_single_tool_call_reconstructs_one_assistant_turn(cfg):
    row, reasons = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("single_call.ndjson"), [])
    assert reasons == []
    roles = [m["role"] for m in row["messages"]]
    assert roles == ["system", "user", "assistant", "tool", "assistant"]
    call = row["messages"][2]["tool_calls"][0]
    assert call["function"]["name"] == "bundle"          # lore_bundle -> bundle
    assert call["id"] == "call_1"
    assert row["messages"][3]["tool_call_id"] == "call_1"
    assert row["meta"]["n_tool_calls"] == 1


def test_non_part_events_are_ignored(cfg):
    """`step_finish` and friends carry no `text`/`tool_use` part to reconstruct."""
    events = fixture_events("single_call.ndjson")
    assert any(e["type"] == "step_finish" for e in events)
    row, _ = convert.convert_one(cfg, meta_for(QUESTION), events, [])
    assert len(row["messages"]) == 5


def test_parallel_calls_in_one_message_stay_in_one_assistant_turn(cfg):
    """Several `tool_use` parts sharing a `messageID` are one turn with several
    calls -- exactly how the chat template renders them."""
    row, reasons = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("parallel_calls.ndjson"), [])
    assert reasons == []
    roles = [m["role"] for m in row["messages"]]
    assert roles == ["system", "user",
                     "assistant", "tool",
                     "assistant", "tool", "tool",
                     "assistant"]
    parallel = row["messages"][4]
    assert [c["function"]["name"] for c in parallel["tool_calls"]] == ["grep", "bash"]
    # results follow their calls, in call order, so tool_call_id pairing holds
    assert [m["tool_call_id"] for m in row["messages"][5:7]] == ["call_2", "call_3"]
    assert row["meta"]["n_tool_calls"] == 3


def test_interleaved_parts_are_grouped_by_message_not_by_wire_position(cfg):
    """Parts of two messages arriving interleaved must not merge or reorder."""
    row, reasons = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("interleaved.ndjson"), [])
    assert reasons == []
    assistants = [m for m in row["messages"] if m["role"] == "assistant"]
    assert len(assistants) == 3
    # m1's two text parts are joined, in wire order, into one turn
    assert assistants[0]["content"] == ("Starting from the index bundle. "
                                        "The bundle verdict is strong.")
    assert assistants[0]["tool_calls"][0]["function"]["name"] == "bundle"
    assert assistants[1]["tool_calls"][0]["function"]["name"] == "read"
    assert not assistants[2].get("tool_calls")


def test_a_crashed_log_parses_what_survived_and_is_rejected(cfg):
    """A torn last line is normal on a killed cell: parse the rest, then reject
    the trajectory for ending mid-exploration rather than emitting half of it."""
    raw = open(os.path.join(FIXTURES, "truncated.ndjson"), encoding="utf-8").read()
    assert not raw.endswith("}\n"), "fixture must end on a torn line"
    events = fixture_events("truncated.ndjson")
    assert len(events) == 4                     # the torn line is dropped, silently
    row, reasons = convert.convert_one(cfg, meta_for(QUESTION), events, [])
    assert row is None
    assert "ends_on_tool_call" in reasons


def test_an_empty_log_is_no_events(cfg):
    assert fixture_events("empty.ndjson") == []
    row, reasons = convert.convert_one(cfg, meta_for(QUESTION), [], [])
    assert (row, reasons) == (None, ["no_events"])


def test_a_missing_log_file_is_no_events(cfg, tmp_path):
    assert convert.parse_events(str(tmp_path / "nope.ndjson")) == []


def test_an_errored_lore_call_in_the_fixture_now_rejects(cfg):
    """This fixture (a lore_bundle whose state is `error`, no output) is the
    exact shape every cell of glm-run-01/-02 recorded when daemon discovery
    broke. It used to convert quietly with an empty tool result; that silence
    is how a fully corrupted corpus passed every gate. It rejects now."""
    row, reasons = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("missing_output.ndjson"), [])
    assert row is None
    assert reasons == ["lore_tool_error:lore_bundle"]


def test_a_tool_use_with_no_output_never_injects_a_literal_null(cfg):
    """A non-lore `tool_use` whose `state` carries no `output` (a failed or
    interrupted call) must not render the four characters `null` as the tool
    result.

    `json.dumps(None)` is the string "null", and a masked tool result reading
    `null` is junk conditioning that no validator check catches: the row is a
    perfectly well-formed string everywhere.
    """
    events = fixture_events("missing_output.ndjson")
    for event in events:
        part = event.get("part") or {}
        if part.get("tool") == "lore_bundle":
            # Interrupted, not errored: the lore_tool_error gate keys on the
            # `error` status alone, and an interrupted call still must not
            # render as the string "null".
            (part.get("state") or {})["status"] = "completed"
    row, reasons = convert.convert_one(cfg, meta_for(QUESTION), events, [])
    assert reasons == []
    results = [m["content"] for m in row["messages"] if m["role"] == "tool"]
    assert results == [""], f"tool result rendered as {results!r}"


# --------------------------------------------------------------------------- #
# argument mapping (Decision 8)
# --------------------------------------------------------------------------- #

def test_read_offset_and_limit_map_to_one_based_start_and_end(cfg):
    """opencode's `read.offset` is 1-based, so it *is* the citation's start.

    Measured against the real event stream at opencode 1.18.23 during the first
    pilot: `{"filePath": ..., "offset": 302, "limit": 430}` returned a body
    whose first line is the file's line 302, and `offset: 1` returned line 1.
    The README originally claimed 0-based and the code added one, which shifted
    every read span in every trajectory by a line.
    """
    row, _ = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("interleaved.ndjson"), [])
    read = [c for m in row["messages"] if m["role"] == "assistant"
            for c in (m.get("tool_calls") or [])
            if c["function"]["name"] == "read"][0]
    assert json.loads(read["function"]["arguments"]) == {
        "path": "src/registry.py", "start": 20, "end": 25}


def test_grep_include_is_renamed_to_glob(cfg):
    row, _ = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("parallel_calls.ndjson"), [])
    grep = row["messages"][4]["tool_calls"][0]
    assert json.loads(grep["function"]["arguments"]) == {
        "pattern": "ChunkerRegistry\\(", "glob": "*.py"}


def test_grep_path_and_include_both_survive_into_the_glob(cfg):
    """The teacher scopes greps with `path` *and* filters with `include`.

    Found in the first pilot: two greps sharing a pattern but scoped to
    different subtrees rendered as two byte-identical supervised calls with
    different tool results, because only `include` was kept.
    """
    assert convert.map_grep({"pattern": "QAOA", "path": "/snap/src/qibo/tests",
                             "include": "*.py"}) == {
        "pattern": "QAOA", "glob": "/snap/src/qibo/tests/**/*.py"}
    assert convert.map_grep({"pattern": "QAOA", "path": "/snap"}) == {
        "pattern": "QAOA", "glob": "/snap/**"}
    assert convert.map_grep({"pattern": "QAOA", "include": "*.py"}) == {
        "pattern": "QAOA", "glob": "*.py"}
    assert convert.map_grep({"pattern": "QAOA", "path": "."}) == {
        "pattern": "QAOA"}
    # `path` is not always a directory: pinning it to a single file and then
    # appending `/**/*.py` yields a glob that matches nothing.
    assert convert.map_grep({"pattern": "QAOA", "include": "*.py",
                             "path": "src/qibo/models/circuit.py"}) == {
        "pattern": "QAOA", "glob": "src/qibo/models/circuit.py"}


def test_dropped_argument_keys_are_counted(cfg):
    """"A renaming that starts losing information is visible in the data."""
    row, _ = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("single_call.ndjson"), [])
    assert row["meta"]["dropped_arg_keys"] == {"lore_bundle.budget_tokens": 1}


def test_every_emitted_tool_name_is_on_the_five_tool_surface(cfg):
    row, _ = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("parallel_calls.ndjson"), [])
    for message in row["messages"]:
        for call in message.get("tool_calls") or []:
            assert call["function"]["name"] in common.TOOL_NAMES


# --------------------------------------------------------------------------- #
# structural gates (Decision 4, first table)
# --------------------------------------------------------------------------- #

def test_an_off_surface_tool_is_the_documented_reject(cfg):
    row, reasons = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("forbidden_tool.ndjson"), [])
    assert row is None
    assert reasons == ["forbidden_tool:webfetch"]


def test_a_dropped_tool_is_removed_rather_than_rejected(cfg):
    """`[emit].drop_tools` is a drop, not a rejection -- README, config comment."""
    events = minimal_trajectory()
    events.insert(3, tool_event("m2", "todowrite", "call_x",
                                {"todos": []}, "ok"))
    row, reasons = convert.convert_one(
        cfg, meta_for("How does the ranker get its input?"), events, [])
    assert reasons == []
    assert row["meta"]["dropped_tool_calls"] == 1
    assert row["meta"]["n_tool_calls"] == 2


def test_answering_from_memory_is_no_tool_calls(cfg):
    events = [text_event("m1", "The registry is built in the constructor.")]
    row, reasons = convert.convert_one(cfg, meta_for(QUESTION), events, [])
    assert row is None
    assert "no_tool_calls" in reasons


def test_not_calling_bundle_first_is_the_documented_reject(cfg):
    events = [
        text_event("m1", "Grepping first."),
        tool_event("m1", "grep", "call_1", {"pattern": "ChunkerRegistry"}, "hit"),
        text_event("m2", "Now the bundle."),
        tool_event("m2", "lore_bundle", "call_2",
                   {"query": "registry construction and defaults"}, "VERDICT"),
        text_event("m3", "Built at src/registry.py:21-25."),
    ]
    row, reasons = convert.convert_one(cfg, meta_for(QUESTION), events, [])
    assert row is None
    assert "bundle_not_first:grep" in reasons


def test_a_final_turn_with_no_prose_is_rejected(cfg):
    events = minimal_trajectory()
    events[-1] = text_event("m3", "   ")
    row, reasons = convert.convert_one(
        cfg, meta_for("How does the ranker get its input?"), events, [])
    assert row is None
    assert reasons


@pytest.mark.xfail(reason="README vs code: `empty_answer` is unreachable. A "
                          "whitespace-only final turn is skipped as 'not a "
                          "turn', so the row ends on its tool result and the "
                          "reason recorded is `ends_on_tool_call` instead")
def test_a_final_turn_with_no_prose_is_reported_as_empty_answer(cfg):
    """Decision 4's table names `empty_answer` = "last turn has no prose".

    No input can produce it: `convert_one` drops a group with neither content
    nor calls before the gate runs, and the gate's `elif` only fires when the
    last message is already a bare assistant turn -- which cannot have blank
    content. A reject reason that can never be counted is a hole in the
    histogram the pilot is told to read.
    """
    events = minimal_trajectory()
    events[-1] = text_event("m3", "   ")
    _, reasons = convert.convert_one(
        cfg, meta_for("How does the ranker get its input?"), events, [])
    assert "empty_answer" in reasons


def test_thrashing_is_over_budget(cfg):
    cfg["emit"]["max_tool_calls"] = 2
    row, reasons = convert.convert_one(
        cfg, meta_for(QUESTION), fixture_events("parallel_calls.ndjson"), [])
    assert row is None
    assert "too_many_tool_calls:3" in reasons


# --------------------------------------------------------------------------- #
# tool-result capping (Decision 7)
# --------------------------------------------------------------------------- #

def test_a_long_tool_result_is_elided_with_an_explicit_marker(cfg):
    cfg["emit"]["max_tool_chars"] = 100
    events = minimal_trajectory()
    events[3]["part"]["state"]["output"] = "x" * 500
    row, reasons = convert.convert_one(
        cfg, meta_for("How does the ranker get its input?"), events, [])
    assert reasons == []
    result = row["messages"][-2]["content"]
    assert "[400 characters elided]" in result
    assert result.startswith("x") and result.endswith("x")


def test_bundle_results_get_the_larger_cap(cfg):
    cfg["emit"]["max_tool_chars"] = 100
    cfg["emit"]["max_bundle_chars"] = 1000
    events = minimal_trajectory()
    events[1]["part"]["state"]["output"] = "b" * 500
    events[3]["part"]["state"]["output"] = "r" * 500
    row, _ = convert.convert_one(
        cfg, meta_for("How does the ranker get its input?"), events, [])
    results = [m["content"] for m in row["messages"] if m["role"] == "tool"]
    assert results[0] == "b" * 500                      # under the bundle cap
    assert "elided" in results[1]                       # over the general cap


def test_a_result_under_the_cap_is_untouched(cfg):
    assert convert.elide("short", 100) == "short"
    assert convert.elide("short", 0) == "short"         # 0 disables the cap
