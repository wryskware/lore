"""The emitted TRL row, against the output gate (Decision 5, Decision 6).

The real validator lives outside this repo and needs a tokenizer; `emission_spec`
replicates the checks a unit test can own -- string-only `arguments`, at least
one supervised span, no null injection, and the rule that user and tool content
are never supervised. See that module for what it deliberately does not cover.
"""

from __future__ import annotations

import json

import common
import convert
import emission_spec
from conftest import fixture_events, meta_for, minimal_trajectory

QUESTION = "Where is the chunker plugin registry initialised?"


def rows(cfg) -> list[dict]:
    """Three structurally different trajectories, converted."""
    out = []
    for fixture, question in (("single_call.ndjson", QUESTION),
                              ("parallel_calls.ndjson", QUESTION),
                              ("interleaved.ndjson", QUESTION)):
        row, reasons = convert.convert_one(
            cfg, meta_for(question, qid=fixture), fixture_events(fixture), [])
        assert reasons == [], (fixture, reasons)
        out.append(row)
    return out


def test_arguments_are_json_strings_not_dicts(cfg):
    """The Arrow trap: a struct-valued `arguments` gets null-filled across rows
    and the model learns to emit the nulls."""
    for row in rows(cfg):
        for message in row["messages"]:
            for call in message.get("tool_calls") or []:
                args = call["function"]["arguments"]
                assert isinstance(args, str)
                assert isinstance(json.loads(args), dict)


def test_no_argument_key_is_null(cfg):
    for row in rows(cfg):
        for message in row["messages"]:
            for call in message.get("tool_calls") or []:
                parsed = json.loads(call["function"]["arguments"])
                assert not any(v is None for v in parsed.values()), parsed


def test_every_content_is_a_plain_string(cfg):
    for row in rows(cfg):
        for message in row["messages"]:
            assert isinstance(message["content"], str), message


def test_the_tools_block_is_identical_across_rows(cfg):
    encoded = {json.dumps(r["tools"], sort_keys=True, ensure_ascii=False)
               for r in rows(cfg)}
    assert len(encoded) == 1
    assert encoded.pop() == json.dumps(common.TOOLS, sort_keys=True,
                                       ensure_ascii=False)


def test_the_system_prompt_is_the_frozen_one(cfg):
    for row in rows(cfg):
        assert row["messages"][0]["content"] == common.SCOUT_SYSTEM_PROMPT
        assert row["meta"]["system_prompt_sha12"] == common.sha12(
            common.SCOUT_SYSTEM_PROMPT)


def test_the_teachers_prompt_never_reaches_the_row(cfg):
    """Decision 3: the whole framing is replaced, so harness scaffolding cannot
    become conditioning the student will not see at inference."""
    import generate
    blob = json.dumps(rows(cfg), ensure_ascii=False)
    for phrase in ("this session is being recorded",
                   "disciplined repository scout",
                   "pinned snapshot and must stay byte-identical"):
        assert phrase in generate.TEACHER_PROMPT
        assert phrase not in blob


def test_the_user_turn_is_the_bare_question(cfg):
    for row in rows(cfg):
        assert row["messages"][1] == {"role": "user", "content": QUESTION}
        assert "question" not in row["meta"]      # it is messages[1], not meta


def test_a_row_survives_a_json_round_trip_byte_stably(cfg, tmp_path):
    out = common.write_jsonl(str(tmp_path / "rows.jsonl"), rows(cfg))
    with open(out, encoding="utf-8") as handle:
        for line in handle:
            row = json.loads(line)
            assert json.dumps(row, ensure_ascii=False) + "\n" == line


def test_the_file_carries_no_struct_valued_arguments(cfg, tmp_path):
    """The validator's raw-text trap, run against the bytes on disk."""
    out = common.write_jsonl(str(tmp_path / "rows.jsonl"), rows(cfg))
    with open(out, encoding="utf-8") as handle:
        for line in handle:
            assert emission_spec.check_no_struct_arguments(line) == []


# --------------------------------------------------------------------------- #
# the mask
# --------------------------------------------------------------------------- #

def test_every_row_passes_the_replicated_validator(cfg, tmp_path):
    out = common.write_jsonl(str(tmp_path / "rows.jsonl"), rows(cfg))
    assert emission_spec.check_file(out) == []


def test_at_least_one_supervised_token(cfg):
    for row in rows(cfg):
        assert emission_spec.supervised_text(row).strip()


def test_user_and_tool_content_are_never_supervised(cfg):
    for row in rows(cfg):
        supervised = emission_spec.supervised_text(row)
        for message in row["messages"]:
            if message["role"] in ("user", "tool", "system") and \
                    message["content"].strip():
                assert message["content"][:80] not in supervised, message["role"]


def test_the_final_answer_and_every_tool_call_are_supervised(cfg):
    for row in rows(cfg):
        supervised = emission_spec.supervised_text(row)
        assert row["messages"][-1]["content"][:80] in supervised
        for message in row["messages"]:
            for call in message.get("tool_calls") or []:
                assert call["function"]["name"] in supervised


def test_the_tool_schemas_are_masked_conditioning(cfg):
    for row in rows(cfg):
        masked = emission_spec.masked_text(row)
        assert "<tools>" in masked
        assert "<tools>" not in emission_spec.supervised_text(row)


# --------------------------------------------------------------------------- #
# meta
# --------------------------------------------------------------------------- #

def test_meta_carries_the_pin_that_makes_a_stray_row_traceable(cfg):
    """Decision 2: a row separated from its batch is still traceable."""
    row = rows(cfg)[0]
    for key in ("repo", "commit", "lore_project", "lore_project_key",
                "index_generation"):
        assert key in row["meta"], key


def test_meta_counts_the_things_the_pilot_is_told_to_read(cfg):
    row = rows(cfg)[0]
    for key in ("n_tool_calls", "n_messages", "path_rewrites",
                "dropped_tool_calls", "dropped_arg_keys",
                "masked_abs_fragments", "graded"):
        assert key in row["meta"], key
    assert row["meta"]["graded"] is False       # grade.py sets it, convert does not


def test_no_absolute_path_survives_into_any_supervised_string(cfg):
    """The whole point of Decision 5, asserted over the finished rows."""
    for row in rows(cfg) + [convert.convert_one(
            cfg, meta_for("How does the ranker get its input?"),
            minimal_trajectory(), [])[0]]:
        for message in row["messages"]:
            if message["role"] != "assistant":
                continue
            assert common.absolute_leaks(message["content"]) == []
            for call in message.get("tool_calls") or []:
                assert common.absolute_leaks(
                    call["function"]["arguments"]) == []
