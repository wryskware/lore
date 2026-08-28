"""The glm-run-01 corruption class: lore tools failing silently.

Two defenses, tested together because they guard the same failure. cell_env
must pin LORE_STATE_DIR to the real state dir (the per-cell XDG isolation
otherwise redirects lore-mcp's daemon discovery, and every bundle/search call
errors); and convert must reject any row whose recording shows a lore tool
call that errored, so if generation-side wiring ever breaks again the batch
fails loudly at 100% instead of training a model that believes bundle fails.
"""

from __future__ import annotations

import os

import common
import convert
import generate
from conftest import TRAIN

EXAMPLE = os.path.join(TRAIN, "config.example.toml")


def _tool_event(tool: str, status: str, output=None, mid="m1", cid="c1"):
    return {"type": "tool_use",
            "part": {"messageID": mid, "callID": cid, "tool": tool,
                     "state": {"status": status, "input": {"query": "q"},
                               "output": output}}}


def _text_event(text: str, mid: str):
    return {"type": "text", "part": {"messageID": mid, "text": text}}


def test_an_errored_lore_call_rejects_the_whole_row(tmp_path):
    cfg = common.load_config(EXAMPLE)
    meta = {"qid": "r#00", "question": "where is x?"}
    events = [_tool_event("lore_bundle", "error"),
              _text_event("the answer", "m2")]
    row, reasons = convert.convert_one(cfg, meta, events, roots=[])
    assert row is None
    assert reasons == ["lore_tool_error:lore_bundle"]


def test_an_errored_bash_call_is_legitimate_conditioning(tmp_path):
    """A failing shell command is something the model genuinely saw and
    should learn to recover from; only lore infrastructure errors reject."""
    cfg = common.load_config(EXAMPLE)
    meta = {"qid": "r#00", "question": "where is x?"}
    events = [_tool_event("lore_bundle", "completed", output="VERDICT: found"),
              _tool_event("bash", "error", mid="m2", cid="c2"),
              _text_event("the answer cites src/x.py:1-2", "m3")]
    row, reasons = convert.convert_one(cfg, meta, events, roots=[])
    assert "lore_tool_error:bash" not in reasons
    assert not any(r.startswith("lore_tool_error") for r in reasons)


def test_cell_env_isolates_xdg_but_pins_the_real_state_dir(tmp_path):
    cfg = common.load_config(EXAMPLE)
    pin = common.RepoPin(repo="o/r", commit="0" * 40, snapshot="o__r",
                         lore_project="o__r", project_key="o-r")
    out_dir = str(tmp_path / "cell")
    os.makedirs(out_dir)
    env = generate.cell_env(cfg, pin, out_dir, str(tmp_path / "oc.json"))
    assert env["XDG_DATA_HOME"] == os.path.join(out_dir, "xdg")
    state = env["LORE_STATE_DIR"]
    assert state and not state.startswith(env["XDG_DATA_HOME"]), \
        "daemon discovery must not follow the per-cell isolation dir"


def test_cell_env_respects_an_explicit_configured_state_dir(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text('[lore]\nstate_dir = "./mystate"\n', encoding="utf-8")
    cfg = common.load_config(str(path))
    pin = common.RepoPin(repo="o/r", commit="0" * 40, snapshot="o__r",
                         lore_project="o__r", project_key="o-r")
    out_dir = str(tmp_path / "cell")
    os.makedirs(out_dir)
    env = generate.cell_env(cfg, pin, out_dir, "oc.json")
    assert env["LORE_STATE_DIR"] == os.path.join(TRAIN, "mystate")
