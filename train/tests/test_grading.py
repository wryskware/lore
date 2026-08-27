"""Citation grading (README, Decision 4).

`file_recall` is the primary signal and `span_hit_rate` the secondary one, with
the documented thresholds 0.5 and 0.34 and a +/-20 line tolerance. Path
comparison is a component-wise suffix match, because the reference answers are
inconsistent about how much of a path they write.
"""

from __future__ import annotations

import json

import pytest

import grade


# --------------------------------------------------------------------------- #
# same_file -- component-wise suffix matching
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("a,b", [
    # the README's own example: the same file, written two ways
    ("src/qibo/models/circuit.py", "qibo/models/circuit.py"),
    ("src/qibo/models/circuit.py", "models/circuit.py"),
    ("src/qibo/models/circuit.py", "circuit.py"),
    ("a/b/c/d.py", "a/b/c/d.py"),
])
def test_suffix_paths_are_the_same_file(a, b):
    assert grade.same_file(a, b) and grade.same_file(b, a)


@pytest.mark.parametrize("a,b", [
    ("x/y.py", "z/y.py"),                       # the README's counter-example
    ("src/qibo/models/circuit.py", "src/qibo/gates/circuit.py"),
    ("lib/x.py", "b/x.py"),
    ("src/a.py", "src/b.py"),
])
def test_different_directories_are_different_files(a, b):
    assert not grade.same_file(a, b)


def test_component_match_is_not_a_string_suffix_match():
    """`b/x.py` must not match `lib/x.py` -- the trap a bare `endswith` falls into."""
    assert "lib/x.py".endswith("b/x.py")
    assert not grade.same_file("lib/x.py", "b/x.py")


# --------------------------------------------------------------------------- #
# citation parsing
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("text,expected", [
    ("src/x.py:12-30", ("src/x.py", 12, 30)),
    ("src/x.py: line 12-30", ("src/x.py", 12, 30)),
    ("src/x.py lines 12-30", ("src/x.py", 12, 30)),
    ("src/x.py (lines 12-30)", ("src/x.py", 12, 30)),
    ("src/x.py L12-30", ("src/x.py", 12, 30)),
    ("src/x.py line 12", ("src/x.py", 12, 12)),
])
def test_the_four_citation_styles_all_parse(text, expected):
    paths, spans = grade.citations(text)
    assert paths == {"src/x.py"}
    assert spans == [expected]


def test_spans_are_found_at_a_nonzero_position():
    """Pin the fixed `^`-anchor bug.

    `LINES_RE` is applied with `.match(text, pos)`. A `^` anchor in that pattern
    binds to position 0, not to `pos`, so every citation that is not the very
    first thing in the answer silently yields no span -- which reads as a model
    that never cites lines rather than as a broken regex.
    """
    prose = ("The registry is built in ChunkerRegistry.__init__, and the only "
             "production call site is in build_index. See src/registry.py:21-25 "
             "and src/index.py:88 for the evidence.")
    assert prose.index("src/registry.py") > 0
    paths, spans = grade.citations(prose)
    assert paths == {"src/registry.py", "src/index.py"}
    assert ("src/registry.py", 21, 25) in spans
    assert ("src/index.py", 88, 88) in spans


def test_a_path_without_a_line_reference_has_no_span():
    paths, spans = grade.citations("see src/a.py and src/b.py lines 5-9")
    assert paths == {"src/a.py", "src/b.py"}
    assert spans == [("src/b.py", 5, 9)]


def test_prose_with_no_citation_parses_to_nothing():
    assert grade.citations("There is no file reference in this sentence.") == (
        set(), [])


def test_citations_tolerates_empty_text():
    assert grade.citations("") == (set(), [])
    assert grade.citations(None) == (set(), [])


# --------------------------------------------------------------------------- #
# grade_row
# --------------------------------------------------------------------------- #

def row(answer: str, n_tool_calls: int = 4) -> dict:
    return {"meta": {"qid": "example__repo#00", "n_tool_calls": n_tool_calls},
            "messages": [{"role": "assistant", "content": answer}]}


def test_a_perfect_answer_is_kept(cfg):
    reference = ("Built in src/registry.py lines 21-25 and called from "
                 "src/index.py line 88.")
    scores, reasons = grade.grade_row(
        row("See src/registry.py:21-25 and src/index.py:88."), reference, cfg,
        None)
    assert reasons == []
    assert scores["file_recall"] == 1.0
    assert scores["span_hit_rate"] == 1.0
    assert scores["citations_checked"] is False


def test_file_recall_at_the_threshold_is_kept(cfg):
    """`min_file_recall` = 0.5 is documented as a floor, so exactly 0.5 passes."""
    reference = "src/registry.py lines 21-25 and src/index.py lines 80-90."
    scores, reasons = grade.grade_row(
        row("Only src/registry.py:21-25 matters."), reference, cfg, None)
    assert scores["file_recall"] == 0.5
    assert not any(r.startswith("low_file_recall") for r in reasons)


def test_file_recall_below_the_threshold_is_rejected(cfg):
    reference = ("src/registry.py lines 21-25, src/index.py lines 80-90 and "
                 "src/rank.py lines 10-30.")
    scores, reasons = grade.grade_row(
        row("Only src/registry.py:21-25 matters."), reference, cfg, None)
    assert scores["file_recall"] == pytest.approx(1 / 3, abs=1e-3)
    assert any(r.startswith("low_file_recall:") for r in reasons)


def test_span_hit_rate_below_the_threshold_is_rejected(cfg):
    """1/3 = 0.333 is below the documented 0.34 floor; 1/2 is above it."""
    reference = ("src/a.py lines 10-20, src/a.py lines 200-210 and "
                 "src/a.py lines 400-410.")
    scores, reasons = grade.grade_row(row("src/a.py:10-20"), reference, cfg, None)
    assert scores["span_hit_rate"] == pytest.approx(0.333, abs=1e-3)
    assert any(r.startswith("low_span_overlap:") for r in reasons)

    reference = "src/a.py lines 10-20 and src/a.py lines 200-210."
    scores, reasons = grade.grade_row(row("src/a.py:10-20"), reference, cfg, None)
    assert scores["span_hit_rate"] == 0.5
    assert not any(r.startswith("low_span_overlap") for r in reasons)


@pytest.mark.parametrize("candidate,hit", [
    ("src/a.py:100-150", True),      # exact
    ("src/a.py:120-130", True),      # contained
    ("src/a.py:151-170", True),      # starts within tolerance after the end
    ("src/a.py:170-180", True),      # last line that is still within +20
    ("src/a.py:171-180", False),     # one line past the tolerance
    ("src/a.py:80-99", True),        # ends within tolerance before the start
    ("src/a.py:70-80", True),        # last line that is still within -20
    ("src/a.py:60-79", False),       # one line short of the tolerance
])
def test_line_span_overlap_honours_the_twenty_line_tolerance(cfg, candidate, hit):
    reference = "src/a.py lines 100-150."
    scores, _ = grade.grade_row(row(candidate), reference, cfg, None)
    assert scores["span_hit_rate"] == (1.0 if hit else 0.0)


def test_the_tolerance_is_the_configured_knob(cfg):
    cfg["grade"]["line_tolerance"] = 0
    scores, _ = grade.grade_row(row("src/a.py:151-170"), "src/a.py lines 100-150.",
                                cfg, None)
    assert scores["span_hit_rate"] == 0.0


def test_an_answer_with_no_citation_is_rejected(cfg):
    scores, reasons = grade.grade_row(
        row("The registry is built in the constructor."),
        "src/registry.py lines 21-25.", cfg, None)
    assert "no_citations" in reasons
    assert scores["cand_files"] == 0


def test_too_few_tool_calls_is_rejected(cfg):
    _, reasons = grade.grade_row(
        row("src/registry.py:21-25", n_tool_calls=1),
        "src/registry.py lines 21-25.", cfg, None)
    assert "few_tool_calls:1" in reasons


# --------------------------------------------------------------------------- #
# the ungradeable reference -- 10 of 260, and it must not be a crash
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("reference", [
    "",
    "The registry is constructed lazily on first use.",   # prose, no path
    "See the ChunkerRegistry constructor.",
])
def test_a_reference_with_no_parseable_citation_is_the_documented_reject(
        cfg, reference):
    scores, reasons = grade.grade_row(row("src/registry.py:21-25"), reference,
                                      cfg, None)
    assert reasons == ["ungradeable_reference"]
    assert scores["file_recall"] is None
    assert scores["span_hit_rate"] is None
    assert scores["ref_files"] == 0


def test_keep_ungradeable_turns_the_reject_off(cfg):
    cfg["grade"]["keep_ungradeable"] = True
    _, reasons = grade.grade_row(row("src/registry.py:21-25"), "", cfg, None)
    assert reasons == []


def test_a_reference_with_files_but_no_spans_scores_only_file_recall(cfg):
    scores, reasons = grade.grade_row(
        row("src/registry.py:21-25"), "Look at src/registry.py.", cfg, None)
    assert scores["file_recall"] == 1.0
    assert scores["span_hit_rate"] is None
    assert reasons == []


# --------------------------------------------------------------------------- #
# unresolvable citations, against a real tree
# --------------------------------------------------------------------------- #

def test_a_cited_path_absent_from_the_snapshot_is_rejected(cfg, tmp_path):
    snapshot = tmp_path / "snap"
    (snapshot / "src").mkdir(parents=True)
    (snapshot / "src" / "registry.py").write_text("x", encoding="utf-8")
    scores, reasons = grade.grade_row(
        row("src/registry.py:21-25 and src/imaginary.py:1-9"),
        "src/registry.py lines 21-25.", cfg, str(snapshot))
    assert scores["citations_checked"] is True
    assert "unresolvable_citation:src/imaginary.py" in reasons


def test_citations_are_not_checked_without_a_snapshot(cfg, tmp_path):
    scores, _ = grade.grade_row(row("src/registry.py:21-25"),
                                "src/registry.py lines 21-25.", cfg,
                                str(tmp_path / "does-not-exist"))
    assert scores["citations_checked"] is False


def test_the_reference_answer_never_enters_the_scores(cfg):
    """Decision 1: the key is read for grading and never quoted into `meta`."""
    reference = "SECRET-KEY-TEXT src/registry.py lines 21-25."
    scores, _ = grade.grade_row(row("src/registry.py:21-25"), reference, cfg, None)
    assert "SECRET-KEY-TEXT" not in json.dumps(scores)
    assert set(scores) == {"file_recall", "span_hit_rate", "ref_files",
                           "ref_spans", "cand_files", "cand_spans",
                           "citations_checked"}
