"""What a cell cost, and the port it cost it on.

Two things generate.py owns that nothing downstream can reconstruct.

The first is teacher token usage. opencode emits a `step_finish` event per model
step whose `part.tokens` carries that step's `input`, `output`, `reasoning` and
a nested `cache` with `read`/`write`. The shapes asserted here are copied from a
real captured session (`first-contact/qiboteam__qibo#00/agent.ndjson`), not from
the README's description of one, because the first pilot's lesson was that a
fixture modelled on prose agrees with the prose and not with the wire.

The second is the port each concurrent cell binds. `opencode run --port N`
starts a server, so the invariant is that no two cells in flight ever hold the
same number.
"""

from __future__ import annotations

import json
import threading

import generate


# --------------------------------------------------------------------------- #
# token capture
# --------------------------------------------------------------------------- #

def step_finish(msg_id: str, *, input_t: int, output_t: int, reasoning: int,
                cache_read: int, cache_write: int = 0) -> dict:
    """Verbatim shape of a real `step_finish`, down to the nested `cache`."""
    return {
        "type": "step_finish", "timestamp": 1787888938119,
        "sessionID": "ses_fb983f5aaffe5x2CVA2OXydG40",
        "part": {
            "id": "prt_0467c2c70001Y5Ch5WzQg9xASW", "reason": "tool-calls",
            "messageID": msg_id, "sessionID": "ses_fb983f5aaffe5x2CVA2OXydG40",
            "type": "step-finish",
            "tokens": {
                "total": input_t + output_t, "input": input_t,
                "output": output_t, "reasoning": reasoning,
                "cache": {"write": cache_write, "read": cache_read},
            },
            "cost": 0,
        },
    }


def write_log(path, events) -> str:
    path.write_text("".join(json.dumps(e) + "\n" for e in events),
                    encoding="utf-8")
    return str(path)


def test_tokens_sum_across_every_step(tmp_path):
    """A tool-calling session bills its whole transcript per step; so do we."""
    log = write_log(tmp_path / "agent.ndjson", [
        {"type": "step_start", "part": {"messageID": "msg_1",
                                        "type": "step-start"}},
        step_finish("msg_1", input_t=5754, output_t=179, reasoning=149,
                    cache_read=0),
        {"type": "text", "part": {"messageID": "msg_2", "type": "text",
                                  "text": "reading the constructor"}},
        step_finish("msg_2", input_t=7621, output_t=199, reasoning=160,
                    cache_read=5632),
    ])
    assert generate.teacher_tokens(log) == {
        "input": 5754 + 7621, "output": 179 + 199,
        "reasoning": 149 + 160, "cache_read": 5632, "steps": 2,
    }


def test_a_log_with_no_step_finish_costs_zero(tmp_path):
    log = write_log(tmp_path / "agent.ndjson", [
        {"type": "text", "part": {"messageID": "msg_1", "type": "text",
                                  "text": "hello"}},
    ])
    assert generate.teacher_tokens(log) == {
        "input": 0, "output": 0, "reasoning": 0, "cache_read": 0, "steps": 0}


def test_a_missing_log_is_zero_rather_than_an_error(tmp_path):
    """Token counts annotate a cell; they must never be what kills one."""
    assert generate.teacher_tokens(str(tmp_path / "absent.ndjson"))["steps"] == 0


def test_a_torn_final_line_costs_only_its_own_step(tmp_path):
    """A crashed cell still spent what its completed steps spent."""
    good = json.dumps(step_finish("msg_1", input_t=100, output_t=10,
                                  reasoning=5, cache_read=0))
    path = tmp_path / "agent.ndjson"
    path.write_text(good + "\n" + good[:40], encoding="utf-8")
    totals = generate.teacher_tokens(str(path))
    assert totals["steps"] == 1 and totals["input"] == 100


def test_a_step_with_no_cache_block_reads_as_zero_cache(tmp_path):
    event = step_finish("msg_1", input_t=42, output_t=1, reasoning=0,
                        cache_read=0)
    del event["part"]["tokens"]["cache"]
    log = write_log(tmp_path / "agent.ndjson", [event])
    assert generate.teacher_tokens(log)["cache_read"] == 0


def test_the_dry_run_records_a_token_block_per_cell(cfg, tmp_path):
    """--dry-run must exercise the capture path, not route around it."""
    assert generate.run_dry(cfg, "tokens-batch") == 0
    workspace = cfg.get_path("paths", "workspace")
    for question in generate.DRY_QUESTIONS:
        meta_path = (tmp_path / "work" / "raw" / "tokens-batch"
                     / question["qid"] / "meta.json")
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        assert set(meta["tokens"]) == set(generate.TOKEN_FIELDS) | {"steps"}
        assert meta["tokens"]["steps"] == 1
    assert workspace  # the batch really was written where the config says


# --------------------------------------------------------------------------- #
# port leasing
# --------------------------------------------------------------------------- #

def test_a_pool_holds_exactly_one_port_per_worker():
    pool = generate.port_pool(4177, 3)
    assert sorted(pool.get() for _ in range(3)) == [4177, 4178, 4179]
    assert pool.empty()


def test_no_two_cells_in_flight_ever_share_a_port():
    """The bug the index-parity port had: a lapped cell reissues a live port."""
    workers, cells = 2, 12
    pool = generate.port_pool(4177, workers)
    held: set[int] = set()
    collisions: list[int] = []
    over_capacity: list[int] = []
    guard = threading.Lock()

    def cell(_):
        with generate.leased(pool) as port:
            with guard:
                if port in held:
                    collisions.append(port)
                held.add(port)
                if len(held) > workers:
                    over_capacity.append(len(held))
            for _ in range(200):        # long enough for the threads to overlap
                pass
            with guard:
                held.discard(port)

    threads = [threading.Thread(target=cell, args=(i,)) for i in range(cells)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    assert collisions == []
    assert over_capacity == []
    assert sorted(pool.get() for _ in range(workers)) == [4177, 4178]


def test_a_port_comes_back_when_its_cell_raises():
    """One dead cell must not permanently shrink the pool."""
    pool = generate.port_pool(4177, 1)
    try:
        with generate.leased(pool):
            raise RuntimeError("cell died")
    except RuntimeError:
        pass
    assert pool.get() == 4177
