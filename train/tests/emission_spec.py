"""An in-repo replica of the critical checks in `~/lora-prep/validate_dataset.py`.

The real validator is referenced, never vendored (README, Decision 6), and it
needs a tokenizer, a patched chat template and a model download to run. None of
that belongs in a unit test, so this module reimplements the subset of its
checks that a test can own honestly:

  SCHEMA  top-level keys, `tools` byte-identical across rows, `content` always a
          plain string, `tool_calls[].function.arguments` a JSON *string*
          parsing to an object with no null-valued keys, role grammar and
          tool_call_id pairing.
  MASK    the render is reproduced at the *character* level rather than the
          token level -- which is enough for every mask question that matters
          here: at least one supervised span, one supervised span per assistant
          message, tool calls and `<|im_end|>` supervised, and the system
          prompt, the tool schemas, the user question and every
          `<tool_response>` payload never supervised.

What this deliberately does NOT replicate: token counts, `max_length`, and the
Arrow round-trip advisory. Those need the real tokenizer and `datasets`, so
they stay the real validator's job -- see the tests section of README.md.
"""

from __future__ import annotations

import json

IM_START = "<|im_start|>"
IM_END = "<|im_end|>"
ASSISTANT_HEADER = "<|im_start|>assistant\n"

# The same 80-character prefix the validator uses for "the user question is not
# supervised". `convert.py`'s `question_echoed_verbatim` gate is pinned to this
# number so the two can never disagree about the same row.
PREFIX = 80


def render_with_mask(messages: list[dict], tools: list[dict]) -> list[tuple[bool, str]]:
    """[(supervised, text)] in the shape a Qwen tool-calling ChatML render emits.

    Assistant bodies and their `<tool_call>` blocks are supervised through the
    closing `<|im_end|>`; the assistant *header*, the system block, the user
    turns and the `<tool_response>` payloads are not.
    """
    out: list[tuple[bool, str]] = []
    schema = "\n".join(json.dumps(t, ensure_ascii=False) for t in tools)
    for index, message in enumerate(messages):
        role, content = message["role"], message.get("content")
        if not isinstance(content, str):
            content = ""
        if role == "system":
            block = f"{IM_START}system\n{content}"
            if index == 0:
                block += f"\n\n# Tools\n\n<tools>\n{schema}\n</tools>"
            out.append((False, block + IM_END + "\n"))
        elif role == "user":
            out.append((False, f"{IM_START}user\n{content}{IM_END}\n"))
        elif role == "tool":
            out.append((False, f"{IM_START}user\n<tool_response>\n{content}\n"
                               f"</tool_response>{IM_END}\n"))
        elif role == "assistant":
            out.append((False, ASSISTANT_HEADER))
            body = content
            for call in message.get("tool_calls") or []:
                fn = call.get("function", {})
                raw = fn.get("arguments")
                try:
                    args = json.loads(raw) if isinstance(raw, str) else raw
                except json.JSONDecodeError:
                    args = raw
                body += ("\n<tool_call>\n"
                         + json.dumps({"name": fn.get("name"), "arguments": args},
                                      ensure_ascii=False)
                         + "\n</tool_call>")
            out.append((True, body + IM_END + "\n"))
    return out


def supervised_text(row: dict) -> str:
    return "".join(t for s, t in render_with_mask(row["messages"], row["tools"]) if s)


def masked_text(row: dict) -> str:
    return "".join(t for s, t in render_with_mask(row["messages"], row["tools"])
                   if not s)


def check_schema(row: dict, tools_ref_json: str | None = None) -> list[str]:
    """Return a list of failure descriptions. Empty is the pass case."""
    bad: list[str] = []
    extra = set(row) - {"meta", "tools", "messages"}
    if extra:
        bad.append(f"unexpected top-level keys: {sorted(extra)}")
    if "tools" not in row or "messages" not in row:
        return bad + ["missing tools or messages"]

    tj = json.dumps(row["tools"], sort_keys=True, ensure_ascii=False)
    if tools_ref_json is not None and tj != tools_ref_json:
        bad.append("tools not byte-identical across rows")

    known = {t["function"]["name"] for t in row["tools"]}
    messages = row["messages"]
    if not messages or messages[0]["role"] != "system":
        bad.append("first message is not system")
    if not messages or messages[-1]["role"] != "assistant" or \
            messages[-1].get("tool_calls"):
        bad.append("does not end with a bare assistant answer")

    open_calls: list[str] = []
    for i, message in enumerate(messages):
        where = f"#{i}({message.get('role')})"
        if not isinstance(message.get("content"), str):
            bad.append(f"{where}: content is not a plain string")
        if message.get("role") not in {"system", "user", "assistant", "tool"}:
            bad.append(f"{where}: unknown role")
        for call in message.get("tool_calls") or []:
            if message["role"] != "assistant":
                bad.append(f"{where}: tool_calls on a non-assistant message")
            fn = call.get("function", {})
            args = fn.get("arguments")
            if not isinstance(args, str):
                bad.append(f"{where}: ARROW TRAP -- arguments is not a JSON string")
            else:
                try:
                    parsed = json.loads(args)
                except json.JSONDecodeError:
                    bad.append(f"{where}: arguments does not parse")
                    parsed = None
                if parsed is not None and not isinstance(parsed, dict):
                    bad.append(f"{where}: arguments does not parse to an object")
                elif isinstance(parsed, dict) and any(
                        v is None for v in parsed.values()):
                    bad.append(f"{where}: ARROW TRAP -- null-valued argument key")
            if fn.get("name") not in known:
                bad.append(f"{where}: calls {fn.get('name')!r}, not in the schema")
            open_calls.append(call.get("id"))
        if message.get("role") == "tool":
            if not open_calls:
                bad.append(f"{where}: tool result answers no open call")
            else:
                expect = open_calls.pop(0)
                if message.get("tool_call_id") not in (None, expect):
                    bad.append(f"{where}: tool_call_id "
                               f"{message.get('tool_call_id')!r} != {expect!r}")
    if open_calls:
        bad.append(f"unanswered tool_calls: {open_calls}")
    return bad


def check_mask(row: dict) -> list[str]:
    bad: list[str] = []
    messages = row["messages"]
    spans = render_with_mask(messages, row["tools"])
    sup = "".join(t for s, t in spans if s)
    msk = "".join(t for s, t in spans if not s)
    n_sup = sum(1 for s, _ in spans if s)
    n_assistant = sum(1 for m in messages if m["role"] == "assistant")

    if not sup.strip():
        bad.append("render: zero supervised text")
    if n_sup != n_assistant:
        bad.append(f"mask: {n_sup} supervised spans vs "
                   f"{n_assistant} assistant messages")
    if IM_END not in sup:
        bad.append("mask: <|im_end|> is not supervised")
    if any(m.get("tool_calls") for m in messages) and "<tool_call>" not in sup:
        bad.append("mask: <tool_call> is not supervised")
    if messages[-1]["content"][:PREFIX] and messages[-1]["content"][:PREFIX] not in sup:
        bad.append("mask: the final answer is not supervised")
    for i, message in enumerate(messages):
        if message["role"] == "assistant" and message["content"].strip() and \
                message["content"].strip()[:60] not in sup:
            bad.append(f"mask: assistant body #{i} is not supervised")

    if "<tool_response>" in sup:
        bad.append("mask: a tool result is supervised")
    if "<tools>" in sup:
        bad.append("mask: the tool schema is supervised")
    if messages[0]["content"][:PREFIX] in sup:
        bad.append("mask: the system prompt is supervised")
    user = next((m for m in messages if m["role"] == "user"), None)
    if user and user["content"][:PREFIX] in sup:
        bad.append("mask: the user question is supervised")
    if ASSISTANT_HEADER in sup:
        bad.append("mask: the assistant header is supervised")
    if any(m["role"] == "tool" for m in messages) and "<tool_response>" not in msk:
        bad.append("mask: tool results are not in the masked text")
    return bad


def check_no_struct_arguments(raw_line: str) -> list[str]:
    """The validator's raw-text trap, run against the file bytes."""
    n = raw_line.count('"arguments": {') + raw_line.count('"arguments":{')
    return [f"file carries {n} struct-valued arguments"] if n else []


def check_row(row: dict, tools_ref_json: str | None = None,
              raw_line: str | None = None) -> list[str]:
    bad = check_schema(row, tools_ref_json)
    bad += check_mask(row)
    if raw_line is not None:
        bad += check_no_struct_arguments(raw_line)
    return bad


def check_file(path: str) -> list[str]:
    """Every check this replica owns, over a finished JSONL."""
    bad: list[str] = []
    ref = None
    with open(path, encoding="utf-8") as handle:
        for i, line in enumerate(handle):
            if not line.strip():
                continue
            row = json.loads(line)
            if ref is None:
                ref = json.dumps(row["tools"], sort_keys=True, ensure_ascii=False)
            bad += [f"row {i}: {m}" for m in check_row(row, ref, line)]
    return bad
