# Qwen matrix run notes — 2026-08-16

Facts a grader needs that the metrics files don't carry.

## Embedding backend (whole run)

All 30+ cells ran with the daemon's embeddings served by Wrysk's RunPod pod
(TEI 1.9.3, Qwen/Qwen3-Embedding-4B fp16, last-token pooling) through
`scripts/embed-remote-proxy.py` on :8091 — the 5090 cannot hold qwen3.8 and
the local embedder together. Store vectors were NOT regenerated: fingerprint
(model="qwen3-4b", 2560d) unchanged; GGUF-vs-fp16 cosine verified ≥0.9995 on
query/code/prose probes before the run. `config.toml.bak-d0014-local` holds
the local-stack config to restore afterward. batch_max_items 64→32 for TEI's
max_client_batch_size (drain-only knob; irrelevant to query-time behavior).

## Unsteered on-arm

No AGENTS.md nudge, no tool-description changes: on-arm lore usage is
organic adoption (11/15 cells ≥1 call, mostly 1-3 calls). Steering proposals
for round 2: `design/9_Scratch/2026-08-16_round-2-steering-drafts.md`.

## Invalid / superseded cells

- `20260816-003804-qwen-lexomancy-off-T1` and `20260816-013529-...-T1`:
  DEAD, do not grade (0/10k tokens). opencode deadlocked on a headless
  `external_directory` permission ask when the explore subagent's glob
  crossed the junction into Lexomancy-alt; both instances killed by hand.
  Fixed after cell 30 by adding `"permission": {"external_directory":
  "allow"}` to BOTH arm configs (symmetric; inert for git-repo cells — no
  other cell ever hit the ask, since a hit = infinite hang). Valid T1 run =
  the 020x-stamped re-run.
- The four git-repo T5 cells stamped 0009xx-0034xx (`lore-off/on`,
  `terrarium-off/on`) have NO diff.patch — run.ps1's `--output=(...)` pwsh
  parse bug (fixed in 2b5d5f6) discarded the diffs before reset. Their
  wall/token metrics are valid observations; for T5 diff grading use the
  01xx-02xx re-runs. Note qwen T5 run-to-run variance is large
  (lore-on-T5: 563k vs 1.43M input across the two runs).
- Lexomancy T5 cells use the cm capture path (never had the bug); their
  diffs are first-run and good.

## Diff noise

Git-repo T5 diffs include an empty `.loreignore` new-file entry — the
daemon's auto-generated ignore file swept up by `git add -N .`, not agent
work. Ignore it when grading.
