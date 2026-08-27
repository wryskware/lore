"""The per-batch manifest and what it pins (README, Decision 2).

"A batch is reproducible from `work/manifests/<batch>.json` plus the pinned
commits." Two properties matter most: the manifest never records an absolute
path (this repository is public), and the subset that identifies the pin rides
in every row's `meta` so a row separated from its batch is still traceable.
"""

from __future__ import annotations

import json
import os

import pytest

import common
import convert
import generate
import grade
from conftest import meta_for, minimal_trajectory, write_events


def pin() -> common.RepoPin:
    return common.RepoPin(
        repo="qiboteam/qibo", commit="a" * 40, snapshot="qiboteam__qibo",
        lore_project="qiboteam__qibo", project_key="k-1234",
        index_generation=17, files=900, chunks=12000, embedded_chunks=12000,
        daemon_version="0.9.0")


def test_the_manifest_records_commit_project_key_and_generation(tmp_path):
    path = common.write_manifest(str(tmp_path), "pilot-01",
                                 {"model": "openai/gpt-5.6-luna", "variant": "max"},
                                 [pin()])
    doc = json.loads(open(path, encoding="utf-8").read())
    assert doc["batch"] == "pilot-01"
    assert doc["teacher"]["model"] == "openai/gpt-5.6-luna"
    repo = doc["repos"][0]
    for key in ("repo", "commit", "snapshot", "lore_project", "project_key",
                "index_generation", "files", "chunks", "embedded_chunks",
                "daemon_version"):
        assert key in repo, key
    assert repo["commit"] == "a" * 40
    assert repo["project_key"] == "k-1234"
    assert repo["index_generation"] == 17


def test_the_manifest_never_records_an_absolute_path(tmp_path):
    """This repository is public and `work/` is shared in bug reports."""
    path = common.write_manifest(str(tmp_path), "pilot-01", {}, [pin()])
    text = open(path, encoding="utf-8").read()
    assert common.absolute_leaks(text) == []
    assert json.loads(text)["repos"][0]["snapshot"] == "qiboteam__qibo"


def test_the_manifest_round_trips_into_pins(tmp_path):
    common.write_manifest(str(tmp_path), "pilot-01", {}, [pin()])
    pins = common.pins_by_repo(common.read_manifest(str(tmp_path), "pilot-01"))
    assert pins["qiboteam/qibo"] == pin()


def test_the_pin_subset_that_rides_in_meta():
    assert pin().as_meta() == {
        "repo": "qiboteam/qibo", "commit": "a" * 40,
        "lore_project": "qiboteam__qibo", "lore_project_key": "k-1234",
        "index_generation": 17}


def test_a_dry_run_pin_is_marked_as_one(cfg):
    p = generate.pin_repo(cfg, "example/repo", "0" * 40, dry_run=True)
    assert p.dry_run is True
    assert p.project_key == "dry-run"
    assert p.snapshot == "example__repo"      # a directory name, not a path


def test_the_project_prefix_is_applied(cfg):
    cfg["lore"]["project_prefix"] = "train-"
    p = generate.pin_repo(cfg, "example/repo", "0" * 40, dry_run=True)
    assert p.lore_project == "train-example__repo"


# --------------------------------------------------------------------------- #
# convert.py refuses to run without its pins
# --------------------------------------------------------------------------- #

def seed_raw_cell(cfg, batch: str = "pilot-01") -> str:
    workspace = cfg.get_path("paths", "workspace")
    cell = os.path.join(workspace, "raw", batch, "example__repo#00")
    os.makedirs(cell, exist_ok=True)
    write_events(os.path.join(cell, "agent.ndjson"), minimal_trajectory())
    with open(os.path.join(cell, "meta.json"), "w", encoding="utf-8") as handle:
        json.dump(meta_for("How does the ranker get its input?"), handle)
    return workspace


def test_convert_refuses_a_batch_with_no_manifest(cfg):
    """Without the manifest there are no snapshot roots to normalise against,
    so a row emitted here could carry the harness's own paths."""
    workspace = seed_raw_cell(cfg)
    assert not os.path.exists(common.manifest_path(workspace, "pilot-01"))
    with pytest.raises(FileNotFoundError):
        convert.main(["--config", cfg.source, "--batch", "pilot-01",
                      "--no-validate"])
    assert not os.path.exists(
        os.path.join(workspace, "data", "pilot-01.converted.jsonl"))


def test_convert_refuses_a_batch_with_no_raw_trajectories(cfg):
    with pytest.raises(SystemExit) as exc:
        convert.main(["--config", cfg.source, "--batch", "nope", "--no-validate"])
    assert "run generate.py first" in str(exc.value)


def test_grade_refuses_a_batch_that_was_never_converted(cfg):
    with pytest.raises(SystemExit) as exc:
        grade.main(["--config", cfg.source, "--batch", "pilot-01",
                    "--no-validate"])
    assert "run convert.py first" in str(exc.value)


def test_convert_normalises_against_every_snapshot_root_in_the_manifest(cfg):
    """Decision 5, point 1: "not just this row's, because a `bash` command in
    one cell can name a sibling checkout"."""
    workspace = seed_raw_cell(cfg)
    snap_root = cfg.get_path("paths", "snapshots")
    common.write_manifest(workspace, "pilot-01", {}, [
        common.RepoPin(repo="example/repo", commit="0" * 40,
                       snapshot="example__repo", lore_project="example__repo",
                       dry_run=True),
        common.RepoPin(repo="other/repo", commit="0" * 40,
                       snapshot="other__repo", lore_project="other__repo",
                       dry_run=True),
    ], {"questions_file": ""})

    cell = os.path.join(workspace, "raw", "pilot-01", "example__repo#00")
    events = minimal_trajectory()
    sibling = os.path.join(snap_root, "other__repo")
    events[3]["part"]["state"]["input"]["filePath"] = \
        os.path.join(sibling, "src", "rank.py")
    write_events(os.path.join(cell, "agent.ndjson"), events)

    assert convert.main(["--config", cfg.source, "--batch", "pilot-01",
                         "--no-validate"]) == 0
    out = os.path.join(workspace, "data", "pilot-01.converted.jsonl")
    rows = common.read_jsonl(out)
    assert len(rows) == 1
    assert "other__repo" not in json.dumps(rows[0]["messages"])


def test_the_reference_answer_never_enters_a_cell_meta(cfg):
    """Decision 1: the key is never shown to the teacher, never emitted into a
    row, and never quoted in `meta` beyond the derived scores.

    `cell_meta` is the only writer of `meta.json`, and `convert.py` copies that
    file forward wholesale, so this is the choke point: what is not written here
    cannot reach a row.
    """
    question = {"qid": "example__repo#00",
                "question": "How does the ranker get its input?",
                "question_sha12": "0" * 12, "qa_class": "How (Procedural)",
                "reference_answer": "ANSWER-KEY-TEXT src/rank.py lines 10-30."}
    p = generate.pin_repo(cfg, "example/repo", "0" * 40, dry_run=True)
    meta = generate.cell_meta(cfg, question, p, status="OK")
    assert "ANSWER-KEY-TEXT" not in json.dumps(meta)
    assert "reference_answer" not in meta
    assert "answer" not in meta
