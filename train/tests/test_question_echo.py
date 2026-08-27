"""`question_echoed_verbatim` and its 80-character threshold (Decision 4).

The README pins this gate to the validator's own number so "the two can never
disagree about the same row": `validate_dataset.py` fails a row when the user
question's first 80 characters appear in the supervised text, and `convert.py`
rejects a `bundle` query that carries the same prefix.
"""

from __future__ import annotations

import pytest

import convert
import emission_spec
from conftest import meta_for, tool_event, text_event

LONG_Q = ("Where is the chunker plugin registry initialised, and what does it "
          "register by default when no configuration file is present?")
assert len(LONG_Q) > emission_spec.PREFIX


def trajectory(bundle_query: str, answer: str = "Built at src/registry.py:21-25."):
    return [
        text_event("m1", "Starting from the index bundle."),
        tool_event("m1", "lore_bundle", "call_1", {"query": bundle_query},
                   "VERDICT: found\n"),
        text_event("m2", "Reading it."),
        tool_event("m2", "read", "call_2", {"filePath": "src/registry.py"},
                   "21| ...\n"),
        text_event("m3", answer),
    ]


def test_the_question_pasted_back_verbatim_is_rejected(cfg):
    row, reasons = convert.convert_one(
        cfg, meta_for(LONG_Q), trajectory(LONG_Q), [])
    assert row is None
    assert "question_echoed_verbatim" in reasons


def test_a_genuinely_expanded_query_passes(cfg):
    query = ("chunker plugin registry construction, the default plugin set it "
             "registers, and the configuration lookup that can override it")
    row, reasons = convert.convert_one(
        cfg, meta_for(LONG_Q), trajectory(query), [])
    assert reasons == []
    assert row is not None


def test_the_threshold_is_exactly_eighty_characters(cfg):
    """At the boundary: 80 characters of the question is an echo, 79 is not."""
    at = LONG_Q[:emission_spec.PREFIX] + " -- plus registry internals"
    below = LONG_Q[:emission_spec.PREFIX - 1] + "|| plus registry internals"
    assert LONG_Q[:emission_spec.PREFIX] in at
    assert LONG_Q[:emission_spec.PREFIX] not in below

    _, reasons = convert.convert_one(
        cfg, meta_for(LONG_Q), trajectory(at), [])
    assert "question_echoed_verbatim" in reasons

    row, reasons = convert.convert_one(
        cfg, meta_for(LONG_Q), trajectory(below), [])
    assert reasons == []
    assert row is not None


def test_a_short_question_is_compared_whole(cfg):
    """Below the threshold there is no prefix to hide behind: the whole question
    counts."""
    short = "How does a search request reach the ranker?"
    assert len(short) < emission_spec.PREFIX
    _, reasons = convert.convert_one(
        cfg, meta_for(short), trajectory(short), [])
    assert "question_echoed_verbatim" in reasons


def test_the_gate_can_be_turned_off(cfg):
    cfg["emit"]["allow_question_echo"] = True
    row, reasons = convert.convert_one(
        cfg, meta_for(LONG_Q), trajectory(LONG_Q), [])
    assert reasons == []
    assert row is not None


def test_an_echo_the_gate_lets_through_would_fail_the_validator(cfg):
    """The two halves of the argument, checked against each other.

    With the gate disabled, the emitted row carries the user's own words inside
    a supervised tool call -- which is exactly what the shared validator's
    "user question NOT supervised" check rejects. That is why the harness gate
    uses the validator's threshold rather than one of its own.
    """
    cfg["emit"]["allow_question_echo"] = True
    row, _ = convert.convert_one(cfg, meta_for(LONG_Q), trajectory(LONG_Q), [])
    failures = emission_spec.check_mask(row)
    assert any("user question is supervised" in f for f in failures)


def test_a_clean_row_satisfies_the_validators_user_turn_rule(cfg):
    query = ("chunker plugin registry construction and the default plugin set "
             "it registers")
    row, reasons = convert.convert_one(cfg, meta_for(LONG_Q), trajectory(query), [])
    assert reasons == []
    assert emission_spec.check_mask(row) == []


@pytest.mark.parametrize("where", ["assistant_prose", "bash_command"])
def test_the_question_echoed_anywhere_supervised_is_caught(cfg, where):
    """The gate scans every supervised string, not only the `bundle` query --
    the user's words are a leak into the loss wherever they land."""
    events = trajectory("chunker plugin registry construction and its defaults")
    if where == "assistant_prose":
        events[2] = text_event("m2", f"Restating the ask: {LONG_Q}")
    else:
        events[3] = tool_event("m2", "bash", "call_2",
                               {"command": f"echo '{LONG_Q}'"}, "")
    _, reasons = convert.convert_one(cfg, meta_for(LONG_Q), events, [])
    assert "question_echoed_verbatim" in reasons
