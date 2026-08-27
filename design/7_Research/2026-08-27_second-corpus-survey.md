# Second-corpus survey for the retrieval-recall eval

*2026-08-27. Research report — evidence, not a decision. Produced by a
GPT-5.6 research worker (web survey with cited sources); the parent
verified the report's structure and reasoning but has NOT independently
verified the per-dataset claims (schemas, licenses, commit-pinning).
Verify the chosen dataset's card directly before building an adapter.
Raw worker output preserved in the session scratchpad.*

## Why

`bench/rcb/retrieval_eval.py` measures lore's retrieval against
hand-authored gold evidence spans, on exactly one corpus
(microsoft/agent-framework, Python, ~20 tasks). Tuning ranking against
one repo risks overfitting; a second corpus is wanted, ideally one that
already ships gold answer keys instead of hand-authoring more (Wrysk,
2026-08-27: "perhaps one thats already used in someones existing bench
and has answer keys / golden lists").

## Ranked recommendation (worker's, endorsed by parent)

1. **SWE Atlas Codebase QnA** (ScaleAI) — the only released corpus found
   combining exact commits, genuine architectural how/why questions,
   **non-Python coverage** (Go, TypeScript, C, Python over 11 real repos:
   kitty, scapy, k6, grafana, minio, trufflehog, …), Apache-2.0 benchmark
   data, and reference answers containing usable line citations
   (`kitty/remote_control.py (lines 317-358)`). 124 expert-authored
   tasks. Adapter work: parse citations from answer prose, check out the
   pinned commit, reject citations that don't resolve, exclude
   runtime-only tasks, audit a 20–40-task slice.
   https://huggingface.co/datasets/ScaleAI/SWE-Atlas-QnA
2. **SWE-QA-Pro** (TIGER-Lab, MIT) — cleanest, lowest-judgment adapter:
   260 questions, 10 per repo across 26 long-tail Python projects, full
   commit ids, consistent file/line citations. Drawback: adds repo
   diversity but zero language diversity.
3. **If C# must be in corpus two: RepoProbe's C# slice**
   (Tencent-Hunyuan, data CC BY 4.0) — 3 pinned C# repos
   (modelcontextprotocol/csharp-sdk, TUnit, SwarmUI) with real questions,
   answers and grading checklists but **no evidence spans** — a manual
   span-annotation pass is required, which is still far cheaper than
   authoring questions from scratch.

Also notable: **ContextBench** ships structured
`{file, start_line, end_line}` gold context — almost exactly our schema —
but its queries are issue-resolution statements, not codebase questions;
usable only as a separate issue-to-context track, not mixed into QA
recall. **Code-QA-Bench** (arXiv 2605.29277) looks nearly ideal on paper
but has no released artifacts yet — watchlist.

Screened out (wrong genre, no pinning, or unverifiable licensing):
CodeQA, RepoEval/RepoCoder, RepoBench, CrossCodeEval, CodeRAG-Bench,
CoIR, LongCodeQA, CORE-Bench, Agent Retrieval Bench, RepoQA,
CodeQueries, CoReQA, StackRepoQA, HiFiRepoQA. Full table with links in
the raw report.

## Roll-your-own fallbacks (assessed, not preferred)

- Hand-authoring ~20 C# tasks: best targets Serilog (minimum effort),
  Orleans (strongest architectural questions), Avalonia (largest; scope
  to a subsystem).
- **Lore itself: rejected as the second corpus** — maintainer questions
  share lore's own vocabulary, and tuning retrieval on the repo that
  implements the retrieval overfits. Keep for smoke/regression only.
- A private Unity project (Lexomancy): great contamination resistance,
  zero reusable gold, unpublishable results. Best as a later private
  holdout, not corpus two.

## Cross-thread hazard flagged by the parent

If **SWE-QA-Pro-Bench** becomes the second eval corpus, then
**SWE-QA-Pro-SFT-Trajectories** (same authors, same repos) cannot be
scouter training data without contaminating the eval — and those
trajectories are currently the best-shaped public SFT supplement for the
LoRA track. Picking **SWE Atlas** for eval keeps the two uses disjoint.
This coupling should be part of the corpus decision.

## Decision needed (Wrysk)

Which second corpus to adapt first. Parent's lean: SWE Atlas (language
diversity + keeps SWE-QA-Pro trajectories free for training), with the
RepoProbe C# slice as a later third corpus if C# coverage is wanted
before Lexomancy-style private holdouts.
