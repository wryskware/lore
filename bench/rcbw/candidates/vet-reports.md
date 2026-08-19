# RCB-W candidate vetting reports — verbatim worker output

Three Opus agents, one cluster of four PRs each, run 2026-08-19 against the pin
checkout (`microsoft/agent-framework` @ `47fa59f8`). Briefs asked for seven
checks per candidate: bug-at-pin, symptom draft, locate-difficulty (with the
actual greps run), test-at-pin, boundedness, leak check, verdict.

These are raw worker reports: evidence, not decisions. Claims marked as
parent-verified in the task-set draft were spot-checked against the pin tree
and the diffs; everything else is the worker's own reasoning. The selection
made from these reports lives in
`design/6_Evaluation/2026-08-19_rcb-w-round-1-task-set-draft.md`.

Parent spot-checks performed (all confirmed):

- `ensure_ascii=True` appears exactly once in non-test source, at
  `_compaction.py:482` (7124's soft grep tell).
- Pin's `_process_stream_event` takes one argument; 7162's tests call the
  fix's new two-argument signature.
- 7470's pin guard is `if self.max_messages is not None: ... ltrim(-self.max_messages, -1)`.
- 7652's pin gate is `if content.call_id and content.name:` with no dedup memory.
- 7200's test D imports the fix's private helper `_normalize_nested_schemas`.

---

## Agent A — streaming / token-usage cluster (7162, 7124, 6809, 6822)

### PR 7162 — Anthropic streaming double-counts token usage

**1. bug-at-pin — CONFIRMED.**
`python/packages/anthropic/agent_framework_anthropic/_chat_client.py:954-988`: `_process_stream_event` emits a usage `Content` on `message_start` (line 967, seeded `output_tokens=1`) *and* on `message_delta` (line 985). Anthropic's `message_delta` usage is a **cumulative** per-message total, but `_merge_update_into_response` in `python/packages/core/agent_framework/_types.py:1907-1911` does `add_usage_details(response.usage_details, content.usage_details)` for every usage content — so the seed is summed onto the cumulative total (`output_token_count` = 26 when the API reported 25), and for server-tool turns that also report cumulative `input_tokens` on `message_delta`, the prompt is counted twice. Non-streaming path (`_process_message`, line 947) reports the correct number.

**2. symptom draft.**
> When I stream a response from an Anthropic model, the token usage reported back to me doesn't match what the provider says. The output token count is always exactly one higher than the number the API reports, and on turns that use provider-hosted tools the input/prompt token count comes back roughly double. Issuing the identical request with streaming turned off gives the correct figures, so my per-request cost accounting is wrong only for streamed calls.

**3. locate-difficulty — trivial-to-moderate.**
- `Grep "output token|token usage|usage"` over all `agent_framework*` source → 378 hits / 46 files. **Miss** (too broad).
- `Grep "usage"` scoped to `python/packages/anthropic/` (justified by "Anthropic model" in the symptom) → **2 files only**, `_chat_client.py` (18 hits) + its test. **Direct hit.**
- The anthropic package has 5 source files total, so provider-scoping collapses the search instantly.
Classification: **trivial to locate the file, moderate to locate the defect** — the agent still has to discover that `from_updates` *sums* usage contents and that Anthropic's deltas are cumulative, neither of which is stated anywhere in the file.

**4. test-at-pin — FAILS at pin, PASSES with fix, but API-coupled.**
All referenced symbols exist at pin: `create_test_anthropic_client` (test file line 42, same signature), `mock_anthropic_client` fixture (conftest line 46), `BetaUsage`, `ChatResponse`/`UsageDetails` from `agent_framework`. `BetaMessageDeltaUsage` is not imported at pin but exists in the pinned SDK (`anthropic>=0.80.0,<0.80.1`); the pin's `_parse_usage_from_anthropic` already reads all four fields the test constructs. I hand-traced `_incremental_usage` against both tests and it yields exactly 25/10 and 25/12.
**Pin-incompatibility:** the tests call `client._process_stream_event(event, emitted)` with **two positional args**; at pin the method takes one → `TypeError`, so it "fails" by erroring, not by asserting. This means the regression test **pins the fix's internal signature**: a correct alternative fix (e.g. accumulating in `_stream()` and rewriting the update, or suppressing the `message_start` seed) would still fail the grader. Also note the test-file hunk anchors at line 1716 but the pin's file is 2809 lines with the anchor function at 1415 — the test patch needs fuzz/append, not clean `git apply`.

**5. bounded — yes.** Source change ≈45 lines in one file.

**6. leak check — clean.** `grep -rni "cumulative|double.count" python/packages/anthropic/` → 0 hits. `grep -rn "7143"` → 0 hits. No comment in `_process_stream_event` or `_parse_usage_from_anthropic` hints at the semantics.

**7. verdict — usable (not strong).** Bug is real, reachable, and the symptom is cleanly behavioral with a sharp numeric signature; downgraded from strong because the regression test asserts through a private two-argument signature, so grading would reject correct alternative fixes unless the harness scores behavior rather than replaying the test verbatim.

### PR 7124 — Compaction token count inflated for non-ASCII text

**1. bug-at-pin — CONFIRMED.**
`python/packages/core/agent_framework/_compaction.py:482`: `_serialize_message` returns `json.dumps(payload, ensure_ascii=True, ...)`, and its output is fed straight to `tokenizer.count_tokens(...)` at lines 462 and 510 (`annotate_token_counts`). With `ensure_ascii=True`, `こ` becomes the 6-character `こ`, so the built-in `CharacterEstimatorTokenizer` (`len(text)//4`, line 66-70) counts ~6x the characters for CJK text. Budget/compaction thresholds therefore fire far earlier than real model usage warrants.

**2. symptom draft.**
> My agent's conversation history gets summarized and truncated far too aggressively when the chat is in Japanese or Chinese. An equivalent-length English conversation keeps many more turns before anything is dropped. The framework's internal estimate of how much of the context budget a message occupies is several times larger than what the model actually charges for the same message, so the agent starts forgetting things while the real context window is still mostly empty.

**3. locate-difficulty — moderate.**
- `Grep "compact|token count|estimat"` over `core/agent_framework/` → 9 files; `_compaction.py` is the clear top hit (48 occurrences). **Hit (file).**
- `Grep -i "japanese|CJK|non-ascii|unicode"` over the same tree → **miss** for the buggy line (only `_serialize_message`'s neighbours; the only reason `_compaction.py:482` appears is that I also included `ensure_ascii`, which is a fix word, not a symptom word).
- Reaching the actual line requires forming the hypothesis "the estimate is computed over an escaped serialization." The file is ~950 lines and `_serialize_message` is a private helper with no token/budget words in it.
Classification: **moderate** — file is trivial to find, the one-token defect is not.

**4. test-at-pin — FAILS at pin, PASSES with fix, applies cleanly.**
`_serialize_message` exists at pin and is added to an import block (`from agent_framework._compaction import ...`) that already exists at pin line 34. `CharacterEstimatorTokenizer` is already imported (line 18). `Message(role="user", contents=["…"])` is valid — `Message.__init__` accepts `Sequence[Content | str | Mapping]` and auto-wraps strings (`_types.py:1722-1744`). At pin, `serialized` is already pure ASCII, so `escaped == serialized`, making both `assert text in serialized` and the strict `<` token comparison fail. With the fix, the Japanese string survives and is 11 chars vs 66 escaped → `2 < 16`. Test file is 954 lines at pin and the hunk appends at EOF after `test_context_window_strategy_validates_thresholds` — anchor exists, appends cleanly.

**5. bounded — yes.** One-token source change (`True` → `False`) plus a 2-line comment.

**6. leak check — one soft tell, no explicit leak.** No `7022`/TODO references anywhere in `python/`. However `grep -rn "ensure_ascii=True"` across all `agent_framework*` source returns **exactly one hit — the buggy line** — while eight sibling call sites in the same codebase (`observability.py`, `_sessions.py`, `_tools.py`, `_harness/_memory.py`, `_workflows/_checkpoint.py`) use `ensure_ascii=False`. Once an agent suspects escaping, that grep hands it the answer. No comment or doc states the bug.

**7. verdict — strong.** Real and reachable at pin, the symptom is purely behavioral and needs no jargon, the test is public-behavior-only and applies cleanly, and locating the one-token defect requires the semantic leap from "history over-compacts in CJK" to "estimation runs on escaped JSON."

### PR 6809 — Preserve function-call name when merging streaming deltas

**1. bug-at-pin — defect confirmed, real-world reachability NOT confirmed.**
`python/packages/core/agent_framework/_types.py:1486`: `name=getattr(self, "name", getattr(other, "name", None))`. `Content.__init__` unconditionally executes `self.name = name` (line 543), so the attribute is never absent and the `other.name` fallback is **dead code** — merging a nameless first delta with a named second yields `name=None`. That much is airtight.
**However**, I could not find any shipped path that produces a nameless-*first* function-call delta. Every in-repo producer puts the name on the first chunk: OpenAI (`_chat_completion_client.py:810`, `name=... if ... else ""`), Anthropic (`content_block_start` carries the name; only the later `input_json_delta` uses `name=""`, `_chat_client.py:1300-1312`), ag-ui (`_handle_tool_call_start` sets `current_tool_name` before `_handle_tool_call_args`, `_event_converters.py:127-158`), Gemini (`or ""`). Since `from_updates` merges as `contents[-1] += new` (self = the earlier chunk), self always holds the name in practice.

**2. symptom draft.**
> When I stream a response and the model calls a tool, the tool call sometimes comes back with an empty function name, so the framework can't dispatch it and the tool never runs. The same request without streaming resolves the tool correctly. It seems to depend on how the provider splits the call across chunks — the arguments arrive intact, only the identity of the function is lost.

*(This draft is honest about the mechanism but I cannot attest that any user actually observes it via a bundled client — see item 7.)*

**3. locate-difficulty — hard.**
- `Grep -i "tool call|function name|call_id"` over `core/agent_framework/` → 185 hits / 13 files, top hits `_tools.py` (55), `_types.py` (41), `security.py` (30). **Miss** — `_types.py` is 4339 lines and the hit density points at `_tools.py`.
- `Grep -i "merge|delta"` scoped to `_types.py` → 12 hits; only line 1408 (`"""Concatenate or merge two Content instances."""`) is relevant, and it is generic. **Weak hit at best.**
- The word "streaming" does not appear anywhere near `_add_function_call_content`; nothing in the function mentions tools-not-running.
Classification: **hard** — requires knowing that streaming deltas are reconciled through `Content.__add__` in the core types module, which no symptom word reaches.

**4. test-at-pin — FAILS at pin, PASSES with fix, cleanly.**
The test is pure public behavior (`Content(...)` constructor + `+` operator), no new symbols. At pin, `(a + b).name` is `None` while the test asserts `"get_weather"` → fails; `(b + a)` and the both-`None` case already pass. Nothing pin-incompatible.
**Patch-application caveats:** the test hunk anchors at line 570 but the target function is at pin line 513 (file drifted ~57 lines), and — more importantly — the **source** hunk's context includes an `informational_only=...` argument that does **not exist** at pin (`_types.py:1483-1491` has only `call_id/name/arguments/exception/additional_properties/raw_representation`). Neither hunk will `git apply` cleanly; both need semantic re-application.

**5. bounded — yes.** One line.

**6. leak check — clean.** No TODO/comment near the site; `grep -rn "name=getattr"` returns exactly the one buggy line with no explanatory comment.

**7. verdict — reject (borderline usable).** The defect is provable and the locate-difficulty is genuinely hard, but I could not identify any bundled provider path that triggers it, so the "behavioral symptom report" would describe a failure the benchmark agent cannot reproduce or confirm at the pin — it is an API-latent bug, not an observable one. It would only work as a task if reframed as a library-consumer/API-contract report rather than an end-user symptom.

### PR 6822 — Ollama parallel tool calls collide on the same `call_id`

**1. bug-at-pin — CONFIRMED.**
`python/packages/ollama/agent_framework_ollama/_chat_client.py:561-571`: `_parse_tool_calls_from_ollama` sets `call_id=tool.function.name`. Two calls to the same tool in one turn therefore share one `call_id`; when those contents flow through `from_updates` (`_types.py:1902-1906`), adjacent `function_call` contents are merged with `+=`, and `_add_function_call_content` only raises on *differing* call_ids — so identical ones merge and the two argument dicts collapse via `{**self_args, **other_args}`, i.e. the second call's arguments overwrite the first's and one call disappears.

**2. symptom draft.**
> Running against a local Ollama model, when I ask for two things at once and the model correctly issues the same tool twice with different arguments, only one invocation actually happens. The arguments it runs with are a blend of the two requests — the later values win — and the first request's parameters are silently discarded. Two different tools called in the same turn work fine; it only breaks when the same tool is called more than once.

**3. locate-difficulty — trivial.**
- `Grep -i "tool call|parallel|same tool|twice"` over `python/packages/ollama/` → **0 matches.**
- `Grep -i "tool_call|call_id"` over `python/packages/ollama/agent_framework_ollama/` → 20 hits, landing directly on line 565 **including the self-incriminating comment** `# Use name of function as call ID since Ollama doesn't provide a call ID`. **Immediate hit.**
The package has 3 source files total. There is essentially no search work.

**4. test-at-pin — fails at pin, but two serious problems.**
Test 1 (`test_parse_duplicate_tool_names_get_unique_call_ids`) fails at pin (both ids are `"search"`), but it additionally asserts `uuid.UUID(str(id))` parses — **it grades the implementation choice, not the behavior**. Any other correct unique-id scheme fails; notably the PR's own description advertises a `f"{name}:{index}:{blake2s}"` scheme that would fail its own merged test.
Test 2 (`test_format_tool_message_strips_unique_suffix`) fails at pin and passes with the fix **only because it uses a `MagicMock`**. The fix's `tool_name = getattr(item, "name", "") or ""` reads `Content.name` on a `function_result`, but `Content.from_function_result` accepts **no `name` parameter** — verified both at pin (`_types.py:812-821`) and at upstream `main` today (line 855-864). Every real function-result content therefore has `name=None`, so the merged fix sends `tool_name=""` to Ollama for all real tool results, breaking result correlation. The mock hides a live regression in the gold patch.
Also: the ollama test file is 576 lines at pin vs the hunk's line-750 anchor, so the test patch needs re-application.

**5. bounded — yes.** ~12 changed source lines.

**6. leak check — LEAK.** `python/packages/ollama/agent_framework_ollama/_chat_client.py:565` carries the comment `# Use name of function as call ID since Ollama doesn't provide a call ID`, which names the exact design flaw at the exact line. Combined with a package containing 3 files, the "locate" phase is void.

**7. verdict — reject.** The bug is real, but the task fails on three counts: an in-code comment at the defect line hands over the location, one test asserts a UUID-specific implementation rather than the behavior, and the gold patch's `_format_tool_message` change is itself incorrect against the framework's real data model (no `name` on `function_result` content, at pin or upstream) — grading against it would reward reproducing a regression.

### Agent A ranking

| PR | verdict | limiting factor |
|---|---|---|
| **7124** | **strong** | none material; single soft grep tell (`ensure_ascii=True` is a lone outlier) |
| **7162** | usable | regression test pins a private two-arg signature; provider name in symptom collapses the file search |
| **6809** | reject (borderline) | no reachable end-user symptom via any bundled client; source hunk context absent at pin |
| **6822** | reject | in-code comment leaks the location; test grades UUID choice; gold patch introduces a real `tool_name=""` regression |

---

## Agent B — schema / middleware / eval cluster (7199, 7200, 7333, 7399)

### PR 7199 — Chat Completions client forwards raw JSON-Schema dicts unwrapped

**1. bug-at-pin — CONFIRMED.**
`python\packages\openai\agent_framework_openai\_chat_completion_client.py:678-682` — `_prepare_options` does `if isinstance(response_format, dict): run_options["response_format"] = response_format`, verbatim, with no shape check. A dict like `{"type": "object", "properties": {...}}` is therefore sent to the Chat Completions API, whose `response_format.type` must be one of `text`/`json_object`/`json_schema` → 400 `Invalid value: 'object'`. The sibling Responses client (`_chat_client.py:722` `_convert_response_format`) already wraps the identical input, so the two clients diverge. Reachable from the declarative loader, which passes `outputSchema.to_json_schema()` as a bare dict (`declarative/_loader.py:464,576,620`).

**2. symptom draft.**
> When I give the OpenAI chat-completions-backed client a plain JSON-Schema dictionary (e.g. `{"type": "object", "properties": {...}, "required": [...]}`) as the `response_format` option, the request fails with an API error saying the value `'object'` is not valid for that parameter. The exact same dictionary works when I use the Responses-API client — it comes back with parsed structured output. I expect both clients to accept the same schema dictionary.

**3. locate-difficulty — TRIVIAL.**
- `Chat Completions API|class OpenAIChatCompletionClient` over `openai/agent_framework_openai/` → **HIT**: 2 files, one is `__init__.py`; lands directly on `_chat_completion_client.py`.
- `response_format` over `**/agent_framework*/**/*.py` → 225 hits / 26 files; buggy file present (8 hits) alongside `_chat_client.py` (31). Partial hit.
- `raw JSON schema|JSON Schema dictionary|json schema dict` (-i) → **MISS** on buggy file, **HIT** on `_chat_client.py:756` (the reference implementation).

**4. test-at-pin — APPLIES CLEANLY.** The three unit tests exercise only `OpenAIChatCompletionClient()._prepare_options`, `Message`, and the `openai_unit_test_env` fixture — all present at pin. They fail at pin (raw dict returned unchanged) and pass with the fix. The pre-existing `test_response_format_dict_passthrough` (`test_openai_chat_completion_client.py:1484`) uses `{"type": "json_schema", ...}`, which the fix explicitly passes through — **no conflict**. The two `param(...)` additions are integration-marked (network) — exclude from grading.

**5. bounded — YES.** ~30 added lines, one static method, one call-site change, one file.

**6. leak check — SIGNIFICANT.** `_chat_client.py:756-772` contains the fix logic *verbatim*, including the comments "Handle raw JSON schemas (e.g. {...})" and "Pop title from schema since OpenAI strict mode rejects unknown keys". Pin tests `test_openai_chat_client.py:2345-2398` already assert that wrapping behavior for the other client. The task reduces to copy-adapt from a file in the same package.

**7. verdict — USABLE.** Real and reachable, cleanly gradeable, but locate is trivial and the correct implementation already exists 40 lines away in a sibling file; this is an easy-tier item, not a reasoning test.

### PR 7200 — `to_json_schema()` doesn't recurse into nested items/properties

**1. bug-at-pin — CONFIRMED.**
`...\python\packages\declarative\agent_framework_declarative\_models.py:240-259` — `to_json_schema()` deep-serializes via `to_dict()` (`_serialization.py:287`, documented deep) and then normalizes **only the top-level properties list**: `prop["type"] = prop.pop("kind", None)`, empty-`enum` removal. `ArrayProperty.items` (`_models.py:153-167`) and `ObjectProperty.properties` (`_models.py:169-208`) survive serialization in declarative shape — `{"kind": ...}` and a *named list* `[{"name":..., "kind":...}]` — and are never touched. Result: nested nodes reach OpenAI with `kind` instead of `type` (and stray `enum: []`), which strict structured outputs reject. Also no `additionalProperties: false` on non-root object nodes.

**2. symptom draft.**
> I declare an agent in YAML with an output schema whose field is a list of records — an array whose entries are objects with their own fields. When the agent runs, the model provider rejects the request, complaining the schema is invalid because a nested node has no type. Schemas with only flat scalar fields at the top level work fine; the failure only appears once a field is an array or a nested record.

**3. locate-difficulty — MODERATE.**
- `outputSchema` over `declarative/agent_framework_declarative/` → **HIT**: exactly 2 files, `_loader.py` and `_models.py`. Good narrowing, but doesn't say which, or which function.
- `output schema|structured output` (-i) over `declarative/` → **MISS** on `_models.py`; hits `_loader.py:386` only (one hop away).
- `nested` (-i) over `declarative/` → **MISS**: 81 hits / 13 files, none in `_models.py`.

Solving still needs understanding the `Property`/`ArrayProperty`/`ObjectProperty` serialization shapes and that nested object properties come back as a *named list*, not a dict.

**4. test-at-pin — 3 of 4 APPLY; 1 INCOMPATIBLE.** `PropertySchema` and the test class exist at pin (see `test_declarative_models.py:285-299`). Tests A/B/C fail at pin (`items == {"kind":"string","enum":[]}` not `{"type":"string"}`; `items["type"]` KeyError; no `additionalProperties`) and pass with the fix. **Test D (`test_property_schema_unexpected_nested_properties_left_untouched`) imports the fix's own new private symbol `_normalize_nested_schemas`** — a correct alternative fix that doesn't define that exact name would fail. Drop test D or rewrite it against `to_json_schema()` public behavior.

**5. bounded — YES.** ~48 net added lines in one source file, two module-level helpers plus a 5-line simplification of the existing loop.

**6. leak check — CLEAN.** `TODO|FIXME|HACK|BUG|known issue|does not recurse|not recursive|workaround` (-i) over `declarative/agent_framework_declarative/` → only unrelated `logger.debug` lines and one generic docstring. No hint at the bug or the fix anywhere in the package.

**7. verdict — STRONG** (after dropping test D). Bug is real, mechanism requires genuine understanding of the declarative model's serialization shape, symptom is fully behavioral, locate is moderate, and no leak exists in-tree.

### PR 7333 — callable-class middleware crashes with `AttributeError`

**1. bug-at-pin — CONFIRMED.**
`...\python\packages\core\agent_framework\_middleware.py:1330-1396` — three f-strings interpolate `middleware.__name__` (lines 1366, 1379, 1393). A callable *instance* has no `__name__`. Worse than a bad message: at line 1366 the `AttributeError` is raised while building the `MiddlewareException`, *inside the `try`*, so the `except Exception` at 1368 sees a non-`MiddlewareException`, `pass`es, and the intended error is silently swallowed — control falls through to line 1393, which raises `AttributeError` again, this time uncaught. Reachable at construction: `categorize_middleware` (`_middleware.py:1428-1437`) calls `_determine_middleware_type` for any `callable(middleware)` that isn't an `AgentMiddleware`/`FunctionMiddleware`/`ChatMiddleware` subclass, and `AgentMiddlewareLayer.__init__` (`_middleware.py:1155`) calls it eagerly.

**2. symptom draft.**
> I wrote my middleware as a small class with an async `__call__` and passed an instance of it when constructing an agent. My signature was wrong, but instead of the clear validation message I get for an equivalent plain-function middleware, agent construction dies with a raw Python attribute error naming my class. The message tells me nothing about what's actually wrong with the middleware; only rewriting it as a function reveals the real complaint.

**3. locate-difficulty — TRIVIAL.**
- `middleware.*validation|validate.*middleware|middleware type` (-i) over `core/agent_framework/` → **HIT**: exactly 1 file, `_middleware.py`.
- `Cannot determine middleware type|must have at least 2 parameters` → **HIT**: 2 files (source + its test).
- `middleware` (-i) over `core/agent_framework/` → 687 hits / 13 files, `_middleware.py` obvious by name.

**4. test-at-pin — APPLIES CLEANLY.** Every referenced symbol is imported at pin (`test_middleware_with_agent.py:8-32`: `Agent`, `MiddlewareException`, `MiddlewareType`, `FunctionInvocationContext`; `MockBaseChatClient` from `conftest.py:135`). No `from __future__ import annotations` in the file, so annotations resolve to real classes and `param_type` detection works. All three tests raise `AttributeError` at pin (escaping `pytest.raises(MiddlewareException)`) and pass with the fix.

**5. bounded — YES.** 4 effective source lines (one `getattr`, three interpolation swaps) plus a docstring reword.

**6. leak check — CLEAN.** `TODO|FIXME|callable class|class instance|__name__` (-i) over `_middleware.py` → only the five legitimate `__name__` uses (lines 1354-1355 annotation inspection, 1366/1379/1393 the bug itself) and an unrelated docstring sample at 295. No comment flags the problem.

**7. verdict — REJECT (low value).** The bug is real and the test is clean, but the observable symptom ("raw attribute error instead of the validation message") all but names the mechanism, locate is trivially a single file, and the fix is 4 lines of `getattr` plumbing — it measures almost nothing. Keep only if you deliberately want an easy floor item.

### PR 7399 — `LocalEvaluator` reports zero-check items as passed

**1. bug-at-pin — CONFIRMED.**
`...\python\packages\core\agent_framework\_evaluation.py:1398-1400` — `item_passed = True` is set before `for result in check_results:`. With `LocalEvaluator()` (no checks), `self._checks` is empty, `asyncio.gather()` returns `[]`, the loop body never runs, so `item_passed` keeps its vacuous `True`. The item is counted `passed`, `scores` is `[]`, `result_counts == {"passed": 1, "failed": 0, "errored": 0}`, `all_passed` is `True` (`_evaluation.py:453`), and `raise_for_status()` (line 467) does not raise (line 479 short-circuits). A quality gate wired to `raise_for_status()` silently passes on zero evidence.

**2. symptom draft.**
> My local evaluation run is assembled from config, and on one environment the check list ended up empty. The run reported every item as passing, the overall run came back green, and the gate I wired to it let the build through — even though each item's score list was empty and nothing was actually measured. A run that evaluated nothing should not be reported as a success.

**3. locate-difficulty — TRIVIAL.**
- `item passes only if|checks pass` (-i) over `python/packages` → **HIT**: 2 lines, both in `_evaluation.py` (1347, 1387) — the exact docstrings the fix rewrites, immediately above the buggy loop.
- `evaluat` (-i) over `core/agent_framework/` → 223 hits / 12 files, `_evaluation.py` dominant at 188. Obvious.
- `no checks|zero checks|empty checks` (-i) over `python/packages` → **MISS** (no matches anywhere).

**4. test-at-pin — ONE INCOMPATIBILITY.** `TestLocalEvaluatorIntegration` (`test_local_eval.py:371`), `_make_item` (line 30), `LocalEvaluator`, `all_passed`, `raise_for_status` all exist. **But `EvalNotPassedError` is NOT in the pin's import block (`test_local_eval.py:11-22`)** — the upstream diff adds no import, so it was already imported at PR 7399's base. The class does exist at `_evaluation.py:69`; the harness must add `EvalNotPassedError` to that import or the test `NameError`s. With the import added, the test fails at pin (`{"passed":1,"failed":0}`, no raise) and passes with the fix.

**5. bounded — YES.** One line of logic (`item_passed = bool(check_results)`) plus two docstring updates.

**6. leak check — CONDITIONAL LEAK.** Nothing in `python/`. But the pin checkout includes `dotnet/`, and `dotnet/src/Microsoft.Agents.AI/Evaluation/AgentEvaluationResults.cs:141` is literally `return result.Metrics.Count > 0;`, with `dotnet/tests/Microsoft.Agents.AI.UnitTests/EvaluationTests.cs:156` commenting "Items with 0 metrics count as failed (the `Metrics.Count > 0` guard in `ItemPassed`)". If the benchmark exposes the whole repo, the intended contract is written down in the sibling implementation. Scope the agent's checkout to `python/` to neutralize this.

**7. verdict — USABLE.** Genuine silent-false-pass bug with a clean one-line fix and a precise test, but locate is trivial and the fix space is narrow; also note a grading hazard — an agent may "fix" it by *raising* on an empty check list instead of failing the item, so grade against the exact `{"passed":0,"failed":1,"errored":0}` + `scores == []` contract.

### Agent B summary

| PR | bug real | symptom behavioral | locate | test applies | bounded | leak | verdict |
|---|---|---|---|---|---|---|---|
| 7199 | yes | yes | trivial | clean | ~30 LOC | fix exists verbatim in sibling file | usable |
| 7200 | yes | yes | **moderate** | 3/4 (drop test D) | ~48 LOC | none | **strong** |
| 7333 | yes | partly (symptom names mechanism) | trivial | clean | 4 LOC | none | reject |
| 7399 | yes | yes | trivial | needs 1 import added | 1 LOC | .NET sibling states the contract | usable |

**Only 7200 meets the "strong" bar.** The common weakness across the other three is that each symptom maps to a single, obviously-named module (`_middleware.py`, `_evaluation.py`, `_chat_completion_client.py`) that the first natural-language grep finds — the pin's file naming is very transparent, which caps locate-difficulty for anything in `core/`. If you need more strong items, prefer bugs in multi-file paths (declarative loader → models → client) over ones that live in a single self-named module.

---

## Agent C — approval / redis / workflows / DevUI cluster (7271, 7470, 7557, 7652)

### PR 7271 — duplicate function call on approval round-trip

**1. bug-at-pin — CONFIRMED (code), but see item 4.**
`_replace_approval_contents_with_results` (`python/packages/core/agent_framework/_tools.py:1907`) rebuilds `existing_call_ids` *inside* the `for msg in messages` loop (pin lines 1930–1936), so the dedupe set only ever sees the message currently being scanned. When a hosting layer replays the stored `function_call` and its `function_approval_request` as **two separate assistant messages**, the request's message contains no `function_call`, the check never fires, and the request expands into a second copy of the same call — only one copy gets matched to the result, so the other goes back to the service unanswered.

**2. symptom draft**
> When I approve a tool the agent asked permission for, the tool block in the conversation comes back marked failed, and my very next message is rejected by the model provider with a 400 saying no output was found for that tool call. Dumping the conversation history that gets sent shows the same tool call listed twice back to back, with only one of the two having a result attached. It only happens when approvals go through a hosted/server-side session; running the same approval-gated tool directly in-process works fine.

**3. locate-difficulty — moderate**
- `No tool output found` → **miss** (1 hit, `packages/orchestrations/tests/test_handoff.py`)
- `duplicate.*(function call|tool call)|(function call|tool call).*duplicate` (-i) → **hit**: 4 files incl. `core/agent_framework/_tools.py`; the matching line is the pin comment `# Don't add the function call if it already exists (would create duplicate)` — i.e. the exact buggy site
- `approve|approval` (-i, packages/**/*.py) → **miss as a locator**: 3098 occurrences / 121 files

**4. test-at-pin — FAILS THE CRITERION.** All symbols exist (`_build_approved_tool_roundtrip` at `test_function_invocation_logic.py:40`, `Content.from_function_call/from_function_result`), and the test hunk applies (offset −285). **But the added test passes at the pin.** Tracing it: msg3 (`assistant [request]`) has no `function_call`, so pin's per-message `existing_call_ids` is empty and the request is restored — the exact assertion the test makes. Both pre- and post-fix produce `["call_reused","call_reused"]` and `[("call_reused","first output"),("call_reused","second output")]`. It is an *anti-over-fix guard* for the new global dedupe, not a regression test. The PR body claims a test failing with `['call_1','call_1'] == ['call_1']`; `gh pr view --json files` confirms the PR touched only these two files, so that test was never merged.
**Pin-incompatibility:** `git apply --check` fails on `_tools.py` hunk #2 — the pin's version carries `# type: ignore[union-attr, operator]` / `[attr-defined]` comments and different line breaks that upstream removed before the PR base.

**5. bounded** — yes, ~35 changed source lines, one function.
**6. leak check** — none naming the bug. Closest is the pre-existing comment `(would create duplicate)` at the buggy site (a locator, not a spoiler). `CHANGELOG.md` duplicate-entries are all older, unrelated PRs.
**7. verdict — reject.** Bug is real and the symptom is excellent, but the PR ships no test that fails at the pin, and its source hunk doesn't apply there; usable only if we author a fresh regression test ourselves.

### PR 7470 — redis: honour a `max_messages` retention limit of zero

**1. bug-at-pin — CONFIRMED.** `RedisHistoryProvider.save_messages` (`python/packages/redis/agent_framework_redis/_history_provider.py`, pin ~line 151) always rpushes, then guards `if self.max_messages is not None: if current_count > self.max_messages: ltrim(key, -max_messages, -1)`. For `max_messages=0` that is `LTRIM key 0 -1` — Redis's "keep the whole list" — so the guard fires on every non-empty save and the trim is a no-op; the one setting meaning "retain nothing" is the only non-`None` setting that never bounds the list. Negatives are worse: `-5` issues `LTRIM key 5 -1`, destroying the five oldest entries each save while the list still grows unbounded. `__init__` (pin ~line 84) validates three other argument combinations but never `max_messages`.

**2. symptom draft**
> I configured the Redis-backed conversation history to keep zero messages, expecting nothing to be retained. Instead every message is still written, and reading the history back returns the whole conversation — the stored list just keeps growing turn after turn, with no error or warning. If I set a negative keep-count instead, it silently throws away the oldest few messages on every save while *still* growing without bound. Only "unlimited" and positive counts behave the way the setting is documented.

**3. locate-difficulty — trivial**
- `retain|retention` (-i, `packages/**/*.py`) → **hit, first result**: `redis/agent_framework_redis/_history_provider.py` (14 files total)
- `oldest.*(trim|delete|remove)|(trim|trimmed).*oldest` (-i) → **hit, single result**: `_history_provider.py:61` — the exact `max_messages` docstring above the buggy code

**4. test-at-pin — clean.** All referenced symbols exist at the pin: `AsyncMock`, `MagicMock`, `patch`, `Message`, `RedisHistoryProvider`, and the `mock_redis_client` fixture (`test_providers.py:49`) which already wires `llen`, `ltrim`, `delete` and `client.pipeline.return_value.__aenter__`. `git apply --check` passes (offsets −32). Discrimination at pin: `test_negative_max_messages_raises` **fails** (no `ValueError`); `test_max_messages_zero_retains_nothing` **fails** (`pipeline` *is* called at pin). `test_max_messages_zero_leaves_stored_history_alone` passes both before and after — it is a guard, not a regression test, which is fine since the other two carry the signal.

**5. bounded** — yes: 1 validation line + a 1-line early return with comment + docstring, ~10 changed source lines.
**6. leak check** — none. No `TODO/FIXME/HACK/workaround` anywhere in `packages/redis`; `max_messages` appears only in the provider, its tests, two samples, and `uv.lock`.
**7. verdict — usable (not strong).** Bug and test are solid and apply cleanly, but the symptom words land on the file on the first grep (trivial locate), so it mostly measures reading comprehension of `LTRIM` semantics rather than localization. **Grader warning:** the PR *body* says `save_messages` calls `delete(key)` for the zero case; the merged diff does **not** — it only returns early. Grade against the diff.

### PR 7557 — preserve all trace contexts in FanInEdgeRunner aggregation

**1. bug-at-pin — code present, but NOT REACHABLE.** `FanInEdgeRunner.send_message` (`python/packages/core/agent_framework/_workflows/_edge_runner.py`, pin ~line 332) collects `[msg.trace_context ...]` / `[msg.source_span_id ...]`, the backward-compat properties on `WorkflowMessage` (`_runner_context.py:52-58`) that return only `trace_contexts[0]` / `source_span_ids[0]` — so any message carrying >1 context loses all but the first. **However, no runtime path at the pin ever produces such a message.** `WorkflowContext.send_message` (`_workflow_context.py:324-337`) unconditionally assigns `msg.trace_contexts = [trace_context]` — exactly one — and `_workflow.py:483` seeds the initial message with `None`. The aggregated multi-context message that `FanInEdgeRunner` builds is handed straight to `_execute_on_target`; it never re-enters `ctx.send_message` and is never checkpointed, so it cannot feed a downstream fan-in. A repo-wide grep for `trace_contexts=` outside `_workflows/` finds **only test files**. The PR's stated trigger ("a prior fan-in aggregation in a nested topology") therefore does not occur.

**2. symptom draft** *(would have to be fabricated — no user can observe this)*
> In a workflow where one join feeds another join, the exported traces are missing span links: the final aggregating step only links back to one upstream branch per incoming message, so most of the original producing spans never appear in its link list. Single-level joins look correct.

**3. locate-difficulty — moderate (if the symptom were real)**
- `fan-in|fan_in|fanin` (-i, `core/agent_framework/`) → **hit**: 9 files incl. `_workflows/_edge_runner.py`
- `span link|span_link|link.*upstream|trace context` (-i, same tree) → **hit**: 6 files incl. `_edge_runner.py`
- Intersecting the two narrows to 4 candidates (`observability.py`, `_workflow_context.py`, `_runner_context.py`, `_edge_runner.py`) — 2 greps plus reading a handful of files.

**4. test-at-pin — clean and discriminating.** Everything referenced exists at the pin: `Executor`, `handler`, `WorkflowContext`, `WorkflowMessage`, `InProcRunnerContext`, `FanInEdgeGroup`, `create_edge_runner`, `State`, `MockMessage`, `MockExecutor` — all already imported by `test_edge.py`; `WorkflowContext._trace_contexts` / `._source_span_ids` exist (`_workflow_context.py:289-290`) and are plumbed `_edge_runner.py:81 → _executor.py:250,266,324`. `git apply --check` passes with no offset on the test file. At the pin the assertion `len(...) == 3` gets 2 → **fails**; with the fix, `zip(strict=False)` pairing gives 2+1=3 in the asserted order → **passes**.

**5. bounded** — yes, ~17 added / 3 removed lines in one block.
**6. leak check** — none. `_runner_context.py:53` says "Get the first trace context for backward compatibility", which is a correct property docstring, not a bug pointer.
**7. verdict — reject.** The code defect is genuine and the test is textbook, but the state it guards against is unproducible through the public runtime at the pin, so there is no honest end-user symptom to write — any report would describe behavior the framework cannot exhibit.

### PR 7652 — duplicate streamed tool calls in DevUI

**1. bug-at-pin — CONFIRMED.** `MessageMapper._map_function_call_content` (`python/packages/devui/agent_framework_devui/_mapper.py`, pin ~line 1294) gates CASE 1 on `if content.call_id and content.name:` only. It has no memory of `context["active_function_calls"]`, so every streamed chunk that repeats the call id and function name re-registers the call (resetting `arguments_chunks`) and emits another `response.output_item.added` — the frontend renders one tool-call card per chunk. CASE 2 still resolves the same `item_id` for all of them, so the argument deltas remain correct while the item list multiplies.

**2. symptom draft**
> Running an agent in the local dev web UI, a single tool invocation shows up as several identical tool-call cards in the chat — one for each streamed piece of the arguments — even though the tool actually executed once. Every duplicated card shows the same call ID and function name, and the argument text is split across them rather than accumulating in one. Agents whose provider only sends the call name and ID on the first chunk render a single card correctly.

**3. locate-difficulty — moderate**
- `duplicate` (-i, `packages/devui`) → **miss**: 6 hits (`_openai/_executor.py` ×2, frontend `.tsx`/`.ts`, `test_mapper.py`), none in `_mapper.py`
- `tool call|function call` (-i, `packages/devui/agent_framework_devui`) → **hit**: 5 files incl. `_mapper.py`, but also the bundled `ui/assets/index.js` and 3 other server modules — needs reading to pick the right one
- Requires understanding that "duplicate card" ⇒ duplicate `response.output_item.added` emission, which no symptom word names.

**4. test-at-pin — clean and discriminating.** `create_test_agent_update` (`test_mapper.py:67`), the `mapper` / `test_request` fixtures, and `Content.from_function_call` all exist at the pin; `git apply --check` passes on both files (source offset −49). At the pin the second update re-enters CASE 1 → 2 `response.output_item.added` events → `assert len(added_events) == 1` **fails**. With the fix, 1 added event, and the delta join `'{"location":"Seattle"}'` holds both before and after. No pin-incompatibility.

**5. bounded** — yes: one condition, 3 changed source lines.
**6. leak check** — none. Nothing in `_mapper.py` or the devui tests hints at repeated metadata; the only nearby comment is the neutral `# This is the first event that establishes the function call`.
**7. verdict — strong.** Real, reachable, purely behavioral symptom; the fix location is not named by any symptom word; the regression test applies cleanly and flips from fail to pass. **Caveat worth noting to the task author:** no first-party client in the repo demonstrably repeats call metadata per chunk (OpenAI chat-completions blanks `id`/`name` on continuation chunks, Anthropic emits `name=""` on `partial_json` deltas), so the symptom draft must state that the provider repeats the metadata — otherwise an agent trying to reproduce with a stock OpenAI client will see nothing wrong.

### Agent C summary

| PR | bug real | reachable | locate | test fails@pin | applies@pin | verdict |
|---|---|---|---|---|---|---|
| 7271 | yes | yes | moderate | **no** (guard test only) | **no** (hunk #2) | reject |
| 7470 | yes | yes | **trivial** | yes (2 of 3) | yes | usable |
| 7557 | yes | **no** | moderate | yes | yes | reject |
| 7652 | yes | yes | moderate | yes | yes | **strong** |

One usable candidate (7652), one weak-but-workable (7470). 7271 could be rescued with a purpose-written regression test plus a re-based source patch; 7557 cannot be rescued without inventing a symptom the framework can't produce.
