"""`--dry-run` end to end: three stages chained in a subprocess, in a tmpdir.

The README's claim is that `generate.py --dry-run` fabricates genuine
opencode-shaped event streams so `convert.py` and `grade.py` then run their
**real** code paths -- no teacher call, no lore daemon, no network. This test is
what makes that claim checkable: it runs the three commands the README prints,
against a config that names nothing outside the tmpdir, and then holds the
result up to the replicated output gate.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

import pytest

import common
import emission_spec
from conftest import TRAIN

BATCH = "pilot-e2e"


@pytest.fixture
def dry_config(tmp_path):
    """A config whose every path is inside the tmpdir, and whose validator is
    switched off -- the real one lives outside this repo and needs a tokenizer."""
    path = tmp_path / "config.toml"
    path.write_text(
        f'[batch]\nname = "{BATCH}"\n'
        "[paths]\n"
        f'workspace = "{(tmp_path / "work").as_posix()}"\n'
        f'snapshots = "{(tmp_path / "work" / "snapshots").as_posix()}"\n'
        "[validate]\n"
        'script = ""\n',
        encoding="utf-8")
    return str(path)


def stage(script: str, config: str, *extra: str) -> subprocess.CompletedProcess:
    argv = [sys.executable, os.path.join(TRAIN, script), "--config", config,
            *extra]
    proc = subprocess.run(argv, capture_output=True, text=True, cwd=TRAIN,
                          timeout=180)
    assert proc.returncode == 0, f"{script} failed:\n{proc.stdout}\n{proc.stderr}"
    return proc


def test_the_three_stages_chain_into_validating_rows(dry_config, tmp_path):
    workspace = str(tmp_path / "work")

    gen = stage("generate.py", dry_config, "--dry-run")
    assert "wrote 2 dry trajectories" in gen.stdout
    manifest = common.read_manifest(workspace, BATCH)
    assert manifest["dry_run"] is True
    assert manifest["repos"][0]["snapshot"] == "example__repo"

    con = stage("convert.py", dry_config, "--batch", BATCH)
    assert "converted 2/2 trajectories" in con.stdout
    assert "snapshot-root rewrites" in con.stdout

    gra = stage("grade.py", dry_config, "--batch", BATCH)
    assert "kept 2/2" in gra.stdout

    train = os.path.join(workspace, "data", f"{BATCH}.train.jsonl")
    assert emission_spec.check_file(train) == []

    rows = common.read_jsonl(train)
    assert len(rows) == 2
    for row in rows:
        assert row["meta"]["graded"] is True
        assert row["meta"]["verified"] is True
        assert row["meta"]["file_recall"] >= 0.5
        # the dry fixture has no tree to resolve paths against
        assert row["meta"]["citations_checked"] is False


def test_the_dry_run_normalises_its_own_fabricated_harness_paths(dry_config,
                                                                 tmp_path):
    """The fixture deliberately carries the absolute snapshot root in a `read`
    argument and inside a `bash` string, so the normaliser has real work."""
    stage("generate.py", dry_config, "--dry-run")
    stage("convert.py", dry_config, "--batch", BATCH)

    workspace = str(tmp_path / "work")
    raw = os.path.join(workspace, "raw", BATCH, "example__repo#00",
                       "agent.ndjson")
    assert common.absolute_leaks(open(raw, encoding="utf-8").read()), \
        "the fixture is supposed to contain absolute paths before conversion"

    rows = common.read_jsonl(
        os.path.join(workspace, "data", f"{BATCH}.converted.jsonl"))
    for row in rows:
        assert row["meta"]["path_rewrites"] > 0
        for message in row["messages"]:
            if message["role"] != "assistant":
                continue
            assert common.absolute_leaks(message["content"]) == []
            for call in message.get("tool_calls") or []:
                assert common.absolute_leaks(
                    call["function"]["arguments"]) == []


def test_nothing_outside_the_tmpdir_is_written(dry_config, tmp_path):
    """`work/` is the only thing the harness produces, and it is gitignored."""
    before = set(os.listdir(TRAIN))
    stage("generate.py", dry_config, "--dry-run")
    stage("convert.py", dry_config, "--batch", BATCH)
    stage("grade.py", dry_config, "--batch", BATCH)
    assert set(os.listdir(TRAIN)) - before <= {"work", "__pycache__"}
    assert (tmp_path / "work" / "data" / f"{BATCH}.train.jsonl").exists()


def test_the_rejects_are_written_beside_the_output(dry_config, tmp_path):
    """Decision 4: rejects are written out in full rather than deleted, so a
    threshold change is a re-run of grade.py and not a re-run of the teacher."""
    stage("generate.py", dry_config, "--dry-run")
    stage("convert.py", dry_config, "--batch", BATCH)
    stage("grade.py", dry_config, "--batch", BATCH)
    data = tmp_path / "work" / "data"
    for name in (f"{BATCH}.converted.jsonl", f"{BATCH}.convert-rejects.jsonl",
                 f"{BATCH}.train.jsonl", f"{BATCH}.grade-rejects.jsonl"):
        assert (data / name).exists(), name


def test_a_raised_threshold_rejects_without_re_running_the_teacher(dry_config,
                                                                   tmp_path):
    stage("generate.py", dry_config, "--dry-run")
    stage("convert.py", dry_config, "--batch", BATCH)

    with open(dry_config, "a", encoding="utf-8") as handle:
        handle.write("\n[grade]\nmin_span_hit_rate = 1.01\n")
    argv = [sys.executable, os.path.join(TRAIN, "grade.py"), "--config",
            dry_config, "--batch", BATCH]
    proc = subprocess.run(argv, capture_output=True, text=True, cwd=TRAIN,
                          timeout=180)
    assert "low_span_overlap" in proc.stdout
    assert proc.returncode == 1                     # nothing left to keep
    rejects = common.read_jsonl(
        str(tmp_path / "work" / "data" / f"{BATCH}.grade-rejects.jsonl"))
    assert len(rejects) == 2
    assert all(r["meta"]["verified"] is False for r in rejects)


def test_the_manifest_alone_identifies_the_batch(dry_config, tmp_path):
    stage("generate.py", dry_config, "--dry-run")
    text = open(common.manifest_path(str(tmp_path / "work"), BATCH),
                encoding="utf-8").read()
    doc = json.loads(text)
    assert doc["teacher"] == {"model": "<dry-run>", "variant": "<dry-run>"}
    assert doc["questions_file"].endswith(f"{BATCH}.jsonl")
    # Decision 2 scopes "never an absolute path" to the pin itself.
    assert common.absolute_leaks(json.dumps(doc["repos"])) == []


@pytest.mark.xfail(reason="README vs code: the manifest is described as naming "
                          "'directory name only, never an absolute path', but "
                          "`questions_file` records the operator's absolute "
                          "workspace path in a public repository's output")
def test_no_part_of_the_manifest_names_an_absolute_path(dry_config, tmp_path):
    stage("generate.py", dry_config, "--dry-run")
    text = open(common.manifest_path(str(tmp_path / "work"), BATCH),
                encoding="utf-8").read()
    assert common.absolute_leaks(text) == [], text
