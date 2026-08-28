"""Citation repair: unresolvable-but-unambiguous paths are rewritten.

One bad citation rejects a whole row, so the teacher's habit of citing bare
filenames (`client.py` for `docker/client.py`) was the biggest yield sink in
glm-run-01 (78 of 260 cells). Repair rewrites only what is unambiguous; the
grade gate still kills everything else.
"""

from __future__ import annotations

import convert

FILES = [
    "docker/client.py",
    "docker/api/client.py",
    "docker/transport/unixconn.py",
    "docker/utils/utils.py",
    "setup.py",
]


def test_a_bare_filename_with_a_unique_suffix_is_repaired():
    text = "The socket is opened in unixconn.py:41-77."
    out, n = convert.repair_citations(text, FILES)
    assert out == "The socket is opened in docker/transport/unixconn.py:41-77."
    assert n == 1


def test_an_existing_full_path_is_untouched():
    text = "See docker/client.py:10-20 and setup.py:1-5."
    out, n = convert.repair_citations(text, FILES)
    assert out == text
    assert n == 0


def test_an_ambiguous_basename_without_a_reference_is_left_to_die():
    text = "The entry point is client.py:5-9."
    out, n = convert.repair_citations(text, FILES)
    assert out == text
    assert n == 0


def test_the_reference_answer_disambiguates_an_ambiguous_citation():
    """File recall already scores `client.py` as matching the reference's
    `docker/client.py` by suffix -- the row was dying on the existence gate
    alone, so the repair may write out the path the grader already accepted."""
    text = "The entry point is client.py:5-9."
    out, n = convert.repair_citations(text, FILES,
                                      ref_paths=frozenset({"docker/client.py"}))
    assert out == "The entry point is docker/client.py:5-9."
    assert n == 1


def test_a_citation_matching_multiple_reference_paths_stays_ambiguous():
    text = "The entry point is client.py:5-9."
    refs = frozenset({"docker/client.py", "docker/api/client.py"})
    out, n = convert.repair_citations(text, FILES, ref_paths=refs)
    assert out == text
    assert n == 0


def test_a_mistyped_directory_with_a_unique_basename_is_repaired():
    text = "Connection reuse lives in transport/unixcon2.py... "\
           "actually docker/trnsport/unixconn.py:12-30."
    out, n = convert.repair_citations(text, FILES)
    assert "docker/transport/unixconn.py:12-30" in out
    assert n >= 1


def test_a_partial_reference_path_still_resolves_against_the_snapshot():
    """References are themselves inconsistent about path depth; the adopted
    path must be a real snapshot file, not the reference's spelling."""
    text = "The entry point is client.py:5-9."
    out, n = convert.repair_citations(
        text, ["src/docker/client.py", "tests/client.py", "setup.py"],
        ref_paths=frozenset({"docker/client.py"}))
    assert out == "The entry point is src/docker/client.py:5-9."
    assert n == 1


def test_spans_and_surrounding_prose_survive_repair():
    text = ("Two claims: unixconn.py:41-77 handles sockets, and "
            "docker/utils/utils.py:100-120 parses hosts.")
    out, n = convert.repair_citations(text, FILES)
    assert out == ("Two claims: docker/transport/unixconn.py:41-77 handles "
                   "sockets, and docker/utils/utils.py:100-120 parses hosts.")
    assert n == 1


def test_no_repairs_returns_the_text_unchanged():
    text = "No citations here at all."
    out, n = convert.repair_citations(text, FILES)
    assert out is text
    assert n == 0
