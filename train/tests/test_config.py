"""Config loading and the shipped `config.example.toml`.

`config.toml` is gitignored, so `config.example.toml` is the only configuration
in the repository -- which makes it documentation, and makes its claim that
"nothing in this file may name a host, a user, a LAN name, or a
machine-specific absolute path" a testable one.
"""

from __future__ import annotations

import os

import pytest

import common
from conftest import TRAIN

EXAMPLE = os.path.join(TRAIN, "config.example.toml")


def test_the_example_config_names_nothing_machine_specific():
    """This repository is public."""
    text = open(EXAMPLE, encoding="utf-8").read()
    leaks = [frag for frag in common.absolute_leaks(text) if frag != "~/"]
    assert leaks == [], leaks


def test_the_example_config_loads_and_merges_over_the_defaults():
    cfg = common.load_config(EXAMPLE)
    assert cfg["teacher"]["model"] == "openai/gpt-5.6-luna"
    assert cfg["emit"]["max_length"] == common.DEFAULTS["emit"]["max_length"]
    # a key the example does not set still comes from the defaults
    assert cfg["teacher"]["timeout_s"] == common.DEFAULTS["teacher"]["timeout_s"]


def test_the_documented_thresholds_are_the_shipped_defaults():
    """README Decision 4's table gives these as the defaults; if they drift the
    README's reject taxonomy is describing a different harness."""
    grade = common.DEFAULTS["grade"]
    assert grade["min_file_recall"] == 0.5
    assert grade["min_span_hit_rate"] == 0.34
    assert grade["line_tolerance"] == 20
    assert grade["min_tool_calls"] == 2
    emit = common.DEFAULTS["emit"]
    assert emit["max_tool_chars"] == 4000
    assert emit["max_bundle_chars"] == 12000
    # Recalibrated by the 2026-08-27 pilot: 60 calls (the only reject at 30
    # was a 31-call cell grading 1.00/1.00) and the proven 32k training budget.
    assert emit["max_tool_calls"] == 60
    assert emit["max_length"] == 32768
    assert emit["allow_question_echo"] is False


def test_relative_paths_resolve_against_train_not_the_cwd(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text('[paths]\nworkspace = "./work"\n', encoding="utf-8")
    cfg = common.load_config(str(path))
    assert cfg.get_path("paths", "workspace") == os.path.join(TRAIN, "work")


def test_an_empty_path_stays_empty(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text('[lore]\nstate_dir = ""\n', encoding="utf-8")
    assert common.load_config(str(path)).get_path("lore", "state_dir") == ""


def test_a_loaded_config_does_not_alias_the_module_defaults(tmp_path):
    """Latent today -- nothing in the three stages writes to its config -- but a
    single `--min-file-recall` override flag, or two batches converted in one
    process, would silently rewrite the defaults for everything after it.
    """
    path = tmp_path / "config.toml"
    path.write_text('[paths]\nworkspace = "./work"\n', encoding="utf-8")
    cfg = common.load_config(str(path))
    before = common.DEFAULTS["grade"]["min_file_recall"]
    cfg["grade"]["min_file_recall"] = 0.99
    try:
        assert common.DEFAULTS["grade"]["min_file_recall"] == before
    finally:
        common.DEFAULTS["grade"]["min_file_recall"] = before
