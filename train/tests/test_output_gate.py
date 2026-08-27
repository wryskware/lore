"""How the referenced validator is invoked, and the daemon preflight.

Decision 6 puts the whole output gate outside this repository: "`grade.py` and
`convert.py` hand their output to `~/lora-prep/validate_dataset.py`". These
tests check the handing-over -- the argv, the exit code, and what happens when
the script is not there -- with a stub standing in for the real validator, so
nothing here depends on a tokenizer or a model download.
"""

from __future__ import annotations

import json
import os
import sys

import pytest

import common

STUB = """
import json, sys
json.dump(sys.argv[1:], open(sys.argv[sys.argv.index("--argv-out") + 1], "w"))
sys.exit(int(sys.argv[sys.argv.index("--exit") + 1]))
"""


def stub_validator(tmp_path, cfg, exit_code: int = 0):
    script = tmp_path / "stub_validator.py"
    script.write_text(STUB, encoding="utf-8")
    cfg["validate"]["python"] = sys.executable
    cfg["validate"]["script"] = str(script)
    return str(tmp_path / "argv.json"), exit_code


def test_the_validator_receives_the_file_and_the_configured_limits(cfg, tmp_path):
    argv_out, _ = stub_validator(tmp_path, cfg)
    jsonl = str(tmp_path / "rows.jsonl")
    common.write_jsonl(jsonl, [{"meta": {}}])

    code = common.run_validator(cfg, jsonl,
                                ["--argv-out", argv_out, "--exit", "0"])
    assert code == 0
    argv = json.loads(open(argv_out, encoding="utf-8").read())
    assert argv[0] == jsonl          # the finished file is the first argument
    assert argv[argv.index("--sample") + 1] == str(cfg["validate"]["sample"])
    assert argv[argv.index("--max-length") + 1] == str(cfg["emit"]["max_length"])


def test_a_failing_validator_propagates_its_exit_code(cfg, tmp_path):
    argv_out, _ = stub_validator(tmp_path, cfg)
    jsonl = common.write_jsonl(str(tmp_path / "rows.jsonl"), [{"meta": {}}])
    assert common.run_validator(
        cfg, jsonl, ["--argv-out", argv_out, "--exit", "7"]) == 7


def test_a_missing_validator_is_skipped_with_a_warning(cfg, tmp_path, capsys):
    """It is referenced, not vendored, so it can legitimately be absent."""
    cfg["validate"]["script"] = str(tmp_path / "not-here.py")
    jsonl = common.write_jsonl(str(tmp_path / "rows.jsonl"), [{"meta": {}}])
    code = common.run_validator(cfg, jsonl)
    assert "validator not found" in capsys.readouterr().err
    assert code == 0


@pytest.mark.xfail(reason="README Decision 6 vs code: a batch converted with no "
                          "validator on the box exits 0, exactly as a validated "
                          "one does, so a scripted pipeline cannot tell the "
                          "output gate ran from the fact that it passed")
def test_a_missing_validator_does_not_report_success(cfg, tmp_path):
    cfg["validate"]["script"] = str(tmp_path / "not-here.py")
    jsonl = common.write_jsonl(str(tmp_path / "rows.jsonl"), [{"meta": {}}])
    assert common.run_validator(cfg, jsonl) != 0


# --------------------------------------------------------------------------- #
# the daemon preflight -- read-only, and it must refuse rather than guess
# --------------------------------------------------------------------------- #

def test_an_explicit_daemon_url_wins_and_is_not_probed(cfg):
    cfg["lore"]["daemon_url"] = "http://127.0.0.1:9999/v1/"
    assert common.daemon_base(cfg) == "http://127.0.0.1:9999/v1"


def test_an_unreachable_state_dir_is_a_daemon_error_not_a_guess(cfg, tmp_path):
    """Decision 2: the harness never mutates index state, so it cannot recover
    from not finding the daemon -- it has to refuse."""
    cfg["lore"]["state_dir"] = str(tmp_path / "no-such-state")
    with pytest.raises(common.DaemonError) as exc:
        common.daemon_base(cfg)
    assert "cannot read the lore daemon port" in str(exc.value)


def test_a_corrupt_daemon_json_is_a_daemon_error(cfg, tmp_path):
    state = tmp_path / "state"
    state.mkdir()
    (state / "daemon.json").write_text("{not json", encoding="utf-8")
    cfg["lore"]["state_dir"] = str(state)
    with pytest.raises(common.DaemonError):
        common.daemon_base(cfg)


def test_the_daemon_port_is_read_from_the_state_dir(cfg, tmp_path):
    state = tmp_path / "state"
    state.mkdir()
    (state / "daemon.json").write_text(json.dumps({"port": 4321}),
                                       encoding="utf-8")
    cfg["lore"]["state_dir"] = str(state)
    assert common.daemon_base(cfg) == "http://127.0.0.1:4321/v1"


def test_the_harness_never_calls_lore_add_or_lore_index():
    """Decision 2: "Registration and indexing stay the operator's job -- `lore
    add` and `lore index` are not called from here." D-0003's
    single-authoritative-owner constraint is what makes this load-bearing.
    """
    train = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    for name in ("generate.py", "convert.py", "grade.py", "common.py"):
        source = open(os.path.join(train, name), encoding="utf-8").read()
        for line in source.splitlines():
            stripped = line.strip()
            if stripped.startswith("#") or stripped.startswith('"'):
                continue
            assert '"lore", "add"' not in line and '"lore", "index"' not in line
            assert "'lore', 'add'" not in line and "'lore', 'index'" not in line
