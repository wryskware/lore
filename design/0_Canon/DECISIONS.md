---
design_status: decided
last_reviewed: 2026-08-14
---

# Lore Decision Ledger

Append-only. Newest entries at the bottom. Schema per [[README]].

## D-0001 — Vault authority and certainty model

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** All Lore design documentation and planning work
- **Decided by:** Wrysk (instruction to structure this vault like the Lexomancy design vault)
- **Decision:** This vault adopts the Lexomancy authority model: tentative-by-default documents, `design_status` lifecycle frontmatter, local certainty callouts, an append-only user-authorized decision ledger, and modality preservation in all synthesis.
- **Rationale:** The model is field-tested against authority laundering — the exact failure mode a design vault consumed by AI agents must resist.
- **Consequences:** Agents consult this ledger before treating any document as binding; promotion requires Wrysk's explicit authorization.
- **Supersedes:** None
- **Canonical sources:** [[README]]

## D-0002 — Build Lore: a new integrated Rust context daemon

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Project direction; repository purpose
- **Decided by:** Wrysk
- **Decision:** Build Lore as a new local context daemon in Rust — modern idioms, current crates — rather than adopting or forking an existing system. This repository (`wryskware/lore`) is the project home.
- **Rationale:** A five-report research phase ([[../7_Research/00_summary]]) established that the commodity layer (tree-sitter, BM25, local embeddings, watchers, SQLite) is solved everywhere, while the capabilities that motivated the project — GPU-quality retrieval over code plus design vaults, and vault-integrated decision memory with a single-owner daemon — exist nowhere. The last adopt candidate (CodeGraph) failed source audit ([[../7_Research/raw/D_codegraph|D report]]).
- **Consequences:** Surveyed projects serve as pattern sources (copy list and avoid list in [[../7_Research/01_landscape]]); planning proceeds in this vault.
- **Supersedes:** None
- **Canonical sources:** [[../7_Research/00_summary]]

## D-0003 — Hard platform constraints

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** All architecture and dependency choices
- **Decided by:** Wrysk
- **Decision:** Lore runs Windows-native (no WSL requirement). C#/Unity is the flagship language target. Embeddings are local-only — no cloud embedding providers (local GPU via an OpenAI-compatible endpoint is the intended default). One authoritative owner of index state; multi-process indexing free-for-alls of the CCE variety are disqualifying by construction.
- **Rationale:** Primary use is Lexomancy (Unity/C#) developed on Windows 11 with an RTX 5090; CCE's multi-process architecture crashed the machine on 2026-08-14 during this project's own research phase.
- **Consequences:** Cross-platform support beyond Windows is welcome but never at the expense of Windows behavior; cloud-embedding code paths may exist only as generic OpenAI-compatible endpoints pointed at local servers.
- **Supersedes:** None
- **Canonical sources:** [[../7_Research/00_summary]]

## D-0004 — v0.1 is a retrieval-first vertical slice with schema-aware seeds

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Milestone scoping (W-planning round 1, Q1)
- **Decided by:** Wrysk
- **Decision:** v0.1 delivers the daemon + repo registration + watcher + Markdown/code indexing + hybrid search over MCP, dogfooded on this repo and the Lexomancy vault. The indexer understands `design_status` frontmatter and D-NNNN references from day one, but no further memory machinery ships in v0.1.
- **Rationale:** The single-owner daemon is the hard systems part and great search is immediately useful daily; schema awareness at parse time is cheap and shapes the data model early.
- **Consequences:** Memory features build on an already-working index; v0.1 is judged as a grep/CCE replacement, not a memory system.
- **Supersedes:** None
- **Canonical sources:** [[../1_Architecture/1.1_Overview]]

## D-0005 — No graph subsystem; structural queries are out of scope

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Subsystem inventory
- **Decided by:** Wrysk
- **Decision:** Lore builds no code-graph subsystem. Semantic + lexical retrieval carries navigation; extra graph tool calls are sharply diminishing returns when the goal is saving tokens. codebase-memory-mcp may keep running beside Lore for structural queries as long as it earns its keep.
- **Rationale:** Research showed graph is the most commodity-duplicated, least-differentiating subsystem, and tree-sitter-grade C# call graphs are actively misleading in Unity code (bare-name `Update()` collisions). Compiler-grade C# (Roslyn/SCIP helper) remains a possible future entry, gated on demonstrated need.
- **Consequences:** No graph tables, no graph MCP tools, no tree-sitter call extraction anywhere in Lore.
- **Supersedes:** None
- **Canonical sources:** [[../7_Research/01_landscape]]

## D-0006 — Two-tier memory: repo-resident lore docs + session ledger

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Memory architecture gestalt (schema details remain open)
- **Decided by:** Wrysk
- **Decision:** Tier 1 — "lore" docs/memories live in the repo as first-class human-readable Markdown (the vault); the DB is only a derived, rebuildable index; there is no separate MCP write API for durable memories — agents write files, canon flows through the promotion gate. Tier 2 — a session/thread ledger (CCE's good idea): threads record heavily-compacted execution summaries, indexed for recall, answering "where'd we leave off / what's left / did we do this already" and pointing back to thread names. The ledger is working memory for the developer and agents, not canon, and may live outside the repo. Agents should post a short summary before signing off rather than relying on transcript mining.
- **Rationale:** Portability and human-readability for durable knowledge; day-to-day continuity needs a cheaper, noisier channel that should not pollute the repo.
- **Consequences:** `2_Memory/` designs two subsystems with different storage, trust, and retention rules; session summaries get their own capture convention and index.
- **Supersedes:** None
- **Canonical sources:** [[../2_Memory/2.1_Memory_Model]]

## D-0007 — Interface shape: loopback HTTP daemon, thin MCP proxy, CLI on the same API

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Process interfaces (planning round 1, Q5/Q6)
- **Decided by:** Wrysk
- **Decision:** The daemon exposes one versioned loopback HTTP API (axum). A thin `lore-mcp` stdio binary proxies MCP to it; the CLI uses the same API. No client ever touches index state. Embeddings: v0.1 consumes an external OpenAI-compatible endpoint from config (lexical search degrades gracefully when absent); daemon-managed llama-server is a later convenience.
- **Rationale:** The one architectural pattern every surveyed project validated (codesearch hub) or died without (CCE); external embedding endpoint keeps GPU process management out of the critical path.
- **Consequences:** Transport alternatives (named pipes) slot behind the same client later; `lore status`/`lore index` are HTTP calls.
- **Supersedes:** None
- **Canonical sources:** [[../1_Architecture/1.1_Overview]]

## D-0008 — Session ledger v1: data-dir Markdown, sign-off convention plus hook net

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Tier-2 memory storage and capture (planning round 2, Q1/Q2)
- **Decided by:** Wrysk
- **Decision:** Session summaries live in the daemon's data dir (e.g. `%LOCALAPPDATA%\lore\<project>\sessions\`) as Markdown files, one per session, indexed like everything else. Capture is agent-authored: convention instructs agents to post a short summary via `session_log` before signing off, backed by a Stop-hook safety net that nudges/captures when an agent forgets. No full transcript mining.
- **Rationale:** Zero repo noise; Markdown keeps the "DB is only an index" principle for tier 2; agent-authored summaries beat mined ones.
- **Consequences:** A later `lore export-sessions` can promote keepers into the vault. Wrysk flags the storage location as the kind of choice real users should eventually weigh in on — treat as default, not dogma.
- **Supersedes:** None
- **Canonical sources:** [[../2_Memory/2.1_Memory_Model]]

## D-0009 — Early end-to-end benchmarks on established OSS repos with free/local models

- **Date:** 2026-08-14
- **Status:** Accepted
- **Scope:** Testing/benchmark strategy (planning round 2, Q5)
- **Decided by:** Wrysk
- **Decision:** Build benchmark/e2e tests early, not after the fact. Fixture corpora: a few mid-size repos from established OSS projects — a ubiquitous Python library, a JS/TS library, and a C# project — for language coverage. Driving models: gpt-5.6-luna while it is free-tier, and/or a local distilled Qwen code 27B on the RTX 5090; possibly a cheap capable API model (e.g. DeepSeek) later.
- **Rationale:** Real coding tasks against known repos beat retrieval-only metrics (research benchmark-skepticism findings). Free/local models make repeated runs costless.
- **Consequences:** Caveat recorded: luna-free measures capability, not cost savings — token-reduction claims need a model where tokens actually cost something. Bench harness lands early in the milestone plan.
- **Supersedes:** None
- **Canonical sources:** [[../5_Implementation/5.1_Milestones]]

## D-0010 — Ledger supersession is a bare ID list; qualified prose is partial

- **Date:** 2026-08-15
- **Status:** Accepted
- **Scope:** Decision-ledger parsing semantics (`parse_ledger`, effective authority)
- **Decided by:** Wrysk (option choice during first dogfood session)
- **Decision:** A `Supersedes` field retires a decision only when its value is a bare ID list ("D-0004." / "D-0002, D-0003" / "D-0002 and D-0003", decoration tolerated). Any other token — a possessive ("D-0002's … clause only"), a qualifier ("in part"), a negation ("None (extends D-0015)") — makes the field a partial supersession that retires nothing; the named entry stays active.
- **Rationale:** Real ledgers supersede parts of decisions in qualified prose, and the surviving entry remains the canonical statement of what stands. Harvesting any D-NNNN mention retired 6 of 16 Lexomancy decisions against the ledger's own words (found dogfooding, 2026-08-15). The failure directions are asymmetric: under-retiring leaves stale canon ranked high but visible; over-retiring silently demotes valid canon and every document citing it.
- **Consequences:** A full supersession must be written as a bare ID list — document this in ledger conventions when the promotion-gate docs land (M2). Partial supersessions need no special syntax.
- **Supersedes:** None
- **Canonical sources:** `crates/lore/src/authority.rs` (`bare_supersedes_list`); the Lexomancy ledger's D-0008/D-0010/D-0014/D-0015/D-0016 entries as the motivating corpus

## D-0011 — Round-1 e2e corpora are Wrysk's own repos; OSS fixtures deferred

- **Date:** 2026-08-15
- **Status:** Accepted
- **Scope:** E2E benchmark fixture corpora (amends D-0009)
- **Decided by:** Wrysk ("no decisions set in stone about the corpora")
- **Decision:** Benchmark round 1 runs on Wrysk's own three repos — lore, latent-music-terrarium, and Lexomancy — instead of OSS fixture repos. They cover the same language spread (Rust, TS+Python, C#, two styles of Markdown vault/docs), double as the dogfood targets D-0004 mandates, and allow parent-verified answer keys. OSS fixture corpora remain a later option for a reproducibility-focused round (no vault-familiarity confound), deferred rather than dropped.
- **Rationale:** Round 1's job is shaking out the daemon and measuring retrieval deltas on corpora whose ground truth we can verify by hand; reproducibility against public repos matters once there is something worth publishing.
- **Consequences:** `design/6_Evaluation/2026-08-15_e2e-round-1-plan.md` is the round-1 protocol; the bench harness lives in `bench/`.
- **Supersedes:** D-0009's fixture-corpora clause only ("a few mid-size repos from established OSS projects"); its early-benchmarks posture and driving-model choices stand.
- **Canonical sources:** [[../6_Evaluation/2026-08-15_e2e-round-1-plan]]

## D-0012 — Authority is repository-opt-in via a committed profile config

- **Date:** 2026-08-15
- **Status:** Accepted
- **Scope:** Authority activation, repo-side configuration, behavior modes
- **Decided by:** Wrysk (authority-profiles grilling session, 2026-08-15)
- **Decision:** Authority semantics activate only through a repo-committed `.lore.toml` `[authority]` table (`profile = "lore-v1"`; `behavior = off | annotate | rank`, defaulting to `annotate`). Repositories without the file receive neutral retrieval with **no** `design_status`/`decision_refs` parsing at all; enabling a profile later triggers a re-index of the repo's Markdown. Unknown profiles or malformed config fail visibly (surfaced in `lore status` and refresh diagnostics) while the repo indexes neutrally. `annotate` computes and exposes authority metadata without touching result ordering; `rank` additionally applies the authority weights. Lore and Lexomancy commit `behavior = "rank"` to preserve dogfood behavior.
- **Rationale:** Repositories that never adopted the vault workflow should not pay parsing cycles or acquire accidental path/frontmatter semantics (independent review, [[../5_Implementation/reviews/2026-08-15_authority-profiles-review-handoff]]); the always-on default was already mispricing latent-music-terrarium inside the round-1 bench corpus. Annotation's value (visible canon-vs-scratch labels) is far better evidenced than reranking's, so the conservative half is the default.
- **Consequences:** The `adr`/MADR-based conventional profile remains a leaning; query-level authority intent (`ignore|prefer|require`) and ranking-weight validation are deferred pending authority-sensitive e2e evidence; multi-root ID-namespace resolution is deferred and tracked as a known limitation.
- **Supersedes:** D-0004's schema-awareness clause only ("understands `design_status` frontmatter and D-NNNN references from day one" — parsing is now profile-gated); its retrieval-first vertical-slice scoping stands.
- **Canonical sources:** [[../5_Implementation/reviews/2026-08-15_authority-profiles-review-handoff]]

## D-0013 — `lore-v1` recognizes per-file decision records

- **Date:** 2026-08-15
- **Status:** Accepted
- **Scope:** `lore-v1` decision-record format
- **Decided by:** Wrysk (authority-profiles grilling session, 2026-08-15)
- **Decision:** In addition to the mono ledger, `lore-v1` recognizes one decision record per file at `**/0_Canon/decisions/D-NNNN-<slug>.md`, using the identical field grammar and D-0010 supersession semantics. The filename's `D-NNNN` prefix is authoritative for identity; a heading that disagrees with the filename, or a duplicate ID (across files or against the mono ledger), is a surfaced violation excluded from the active set. Per-file records receive the same pinned ledger tier as the mono ledger. The mono ledger remains fully valid; each vault migrates (or doesn't) at its own pace.
- **Rationale:** Small immutable files beat one growing monolith for review, diffing, and the accepted-records-are-substantively-immutable rule; additive recognition avoids coupling a Lexomancy vault migration to a Lore refactor wave.
- **Consequences:** New decisions may be authored per-file once parser support lands; if both dogfood vaults migrate voluntarily, mono-ledger deprecation becomes a `lore-v2` candidate.
- **Supersedes:** None (extends D-0001's ledger convention).
- **Canonical sources:** [[../5_Implementation/reviews/2026-08-15_authority-profiles-review-handoff]]
## D-0014 — Default embedding stack: Qwen3-Embedding-4B on standalone llama-server

- **Date:** 2026-08-16
- **Status:** Accepted
- **Scope:** Embedding model + serving stack for the reference setup (D-0003's local-embeddings constraint made concrete). D-0012/D-0013 are reserved by the authority-profiles branch, hence the gap.
- **Decided by:** Wrysk (verdict after the embed-model retrieval bench and the indexing-throughput session)
- **Decision:** The default embedding model is **Qwen3-Embedding-4B (Q8_0 GGUF, 2560 dims, last pooling, Apache-2.0)** served by a **standalone llama.cpp `llama-server` CUDA build** — not Ollama, which was only ever the ambient convenience default and was never canon. Documents embed unprefixed; queries use the card-sanctioned instruct prefix ("Given a natural language query, retrieve relevant code snippets or documentation passages"). `max_embed_bytes = 3584` under a 16384-token server context. Serving flags that are load-bearing for throughput: `--kv-unified` (without it llama.cpp shreds pooled variable-length batches into ~one-sequence decodes), `--no-cache-prompt`, `-b/-ub 2048`, `--parallel 16`.
- **Rationale:** Bench (2026-08-15, `bench/retrieval/`, three corpora, hand-verified answer keys): hit@10 .92/.92/.87 and C#-semantic 0.69 vs the nomic incumbent's 0.15 — the flagship-language gap is the decision. jina-code-1.5b was the C# runner-up but is CC-BY-NC and weak on design docs; qwen3-8b adds ~nothing for 2x cost. Throughput session (2026-08-16) root-caused the drain cost and settled the flags: ~11.4k tok/s honest steady-state on the 5090.
- **Consequences:** Fingerprint change forces a one-off full re-embed (~15-16 min for ~40k chunks at the tuned flags). Costs accepted: ~8.2 GB resident VRAM while the server runs, 2560-dim vectors (~2.6x vector-scan cost, ~265 ms search p50 at 34k chunks, +7 ms query-embed p50). The daemon still never manages the server process (D-0007); the launcher script and flags live in the repo as operational defaults, not frozen canon — retune freely without a new decision, but a *model* change is a new decision.
- **Supersedes:** None
- **Canonical sources:** [[../7_Research/raw/C_embeddings]]; `bench/retrieval/README.md`; bench summary artifact (claude.ai/code/artifact/72ce25a8-661f-4885-a94d-42f84951fb06)

## D-0015 — Ingestion inverts: snapshot-manifest push, one observer per machine

- **Date:** 2026-08-16
- **Status:** Accepted
- **Scope:** Ingestion architecture (resolves the fork recorded in issue #18 and the scoping brief; touches D-0003 and D-0007)
- **Decided by:** Wrysk (ingestion-inversion grilling session, 2026-08-16)
- **Decision:** Content reaches the index by **push**: a client sends a snapshot, and the index owner never reads a filesystem it does not own. The push unit is a **full manifest** (`path, content-hash, size`, ignore-filtered); deletion is absence from the manifest, guarded — a push deleting more than 50% *and* more than 100 files is rejected absent an explicit per-invocation CLI override, and a tripped guard is surfaced in `status`. Flow: manifest → daemon returns needed paths → uploads land in a per-push staging area → one transaction upserts, deletes, and advances the project generation; readers see old state until the flip. Writers are serialized by a per-project **push lease** with a monotonically increasing epoch, TTL + heartbeat, **takeover** on conflict (stale pushes rejected with a named error; epoch churn visible in `status`); push-session handles are unguessable and bound to project + epoch, checked in the publishing transaction. Exactly **one filesystem observer per machine**: the local daemon owns walk/watch and doubles as the forwarder to a remote daemon; agent clients keep read-only verbs. Locally the walker feeds the snapshot interface **in-process using the wire-message types** from `lore-core`; the receiving push routes ship in the public daemon (loopback-only by default) so a self-hosted remote daemon is a first-class deployment. Watcher debounce moves to ~20–30s (retrieval is front-loaded at task start; `lore index` remains the immediate path) and a receiving daemon enforces a hard minimum push interval, rejecting hotter clients. Ignore evaluation is trusted client-side with server-side backstop scanning; git repos default to git-tracked-files-only manifests; credential hard-excludes are pattern-based only (no entropy scanning) and not overridable by `.loreignore`. A remote store retains compressed current-generation file text to serve `expand` context; the store must additionally be able to open with an **externally supplied key held only in memory** (encrypted-at-rest stores; key distribution is a deployment concern). Refused by name: incremental mutation/event-stream protocols, and multi-generation content-addressed blob stores with reference-counted GC — the store keeps current generation only, keyed `(project, path)`; staging cleanup is session-directory deletion.
- **Rationale:** Snapshots make the concurrency hazards of decoupling daemon from filesystem structurally impossible rather than carefully avoided: no delete events to lose, idempotent pushes, races degraded to loud rejection by epoch fencing (D-0003's single-owner principle surviving the wire). The indexer was already a diff machine; only the observation source changes, so local behavior is user-invisible. Debounce values follow round-1 bench evidence that agent retrieval happens at task start, not mid-edit.
- **Consequences:** `lore add` becomes "create project + start pushing"; `.loreignore` evaluation moves pusher-side; ledger/authority parsing operates on pushed content; `expand` reads disk locally and retained text remotely. Guard thresholds, debounce default, and the server floor are operational defaults tunable without a new decision; protocol shape changes need one. Implementation proceeds in a worktree in parallel with bench round 2, which runs a pinned binary and commit.
- **Supersedes:** D-0003's local-only-embeddings clause is scoped rather than retired — a deployment embeds where its index lives, and the local reference setup stands; D-0007 stands and gains the push surface as part of the same versioned API.
- **Canonical sources:** [[../1_Architecture/1.2_Ingestion]]; [[../1_Architecture/2026-08-16_ingestion-inversion-decision-brief]]

## D-0016 — Project identity: the registry binds the declared name

- **Date:** 2026-08-16
- **Status:** Accepted
- **Scope:** Project identity and request scoping (resolves the hold recorded in the project-scoping brief, 2026-08-16)
- **Decided by:** Wrysk (ingestion-inversion grilling session, 2026-08-16)
- **Decision:** A project's identity is its **registry-bound declared name**: `lore add` binds the `[project]` name from `.lore.toml` (writing one when absent, per the existing naming behavior) and **rejects duplicate names** at registration. Requests name projects by that identity; cwd-containment resolution is demoted to a local discovery convenience, never identity. A declared name is **never an authorization identity** — any deployment serving more than one trusting party must map caller identity to project access in front of the daemon (issue #18's scope).
- **Rationale:** The hold existed because the ingestion fork could overturn a premature choice; D-0015 settles that fork, and inversion makes path-based identity moot on any remote hop while leaving the declared-name mechanism cheap and collision-checked locally.
- **Consequences:** The wire contract's project identifier (already mandatory per the shipped scoping work) resolves against the registry binding. The scoping brief's Resolution section is closed by this entry.
- **Supersedes:** None
- **Canonical sources:** [[../4_Interfaces/2026-08-16_project-scoping-decision-brief]]; [[../1_Architecture/1.2_Ingestion]]

## D-0017 — Manifest basis for git repos: git-aware, not tracked-only

- **Date:** 2026-08-17
- **Status:** Accepted
- **Scope:** The git-repo manifest basis (amends one D-0015 clause)
- **Decided by:** Wrysk (option (b), 2026-08-17)
- **Decision:** A git repo's manifest basis is **git-aware**: tracked files plus untracked files not excluded by gitignore (`git ls-files --cached --others --exclude-standard` semantics), intersected with lore's ignore rules. The credential hard-excludes still apply on top and remain non-overridable by `.loreignore`. Non-git projects are unchanged (walker + hard-excludes).
- **Rationale:** Tracked-only made a brand-new file invisible to the index until `git add` — a freshness regression in exactly the agent workflow Lore serves, discovered at implementation time. `.gitignore` is where secrets actually live, so the practical leak protection is preserved; the only content git-aware admits that tracked-only refused is untracked-un-gitignored files, which the pre-D-0015 walker indexed anyway and which the hard-excludes still screen.
- **Consequences:** The pusher-side basis implementation targets this; D-0015's secrecy layering is otherwise unchanged.
- **Supersedes:** D-0015's git-tracked-files-only clause only ("git repos default to git-tracked-files-only manifests"); every other D-0015 clause stands.
- **Canonical sources:** [[../1_Architecture/1.2_Ingestion]]

## D-0018 — Pre-release simplicity: no migrations, no compat machinery (expires at first release)

- **Date:** 2026-08-17
- **Status:** Accepted
- **Scope:** Engineering posture **until the first tagged release only** — this entry is explicitly temporal and does not bind beyond that point
- **Decided by:** Wrysk (2026-08-17)
- **Decision:** Until the first tagged release: (1) **no schema migrations are authored** — the store schema is one flattened definition, and on a schema-version mismatch the daemon rebuilds the store outright with a loud log, because the index is derived data (repos are the source of truth; vault and session Markdown live beside the store, not in it); (2) **no API version negotiation or compatibility machinery** — everything pins at v1. The handshake's version-equality check stays (it catches same-machine stale-binary skew with a named error), and additive wire hygiene stays as cheap discipline, but neither is a compatibility promise. **Expiry: this posture ends at the first tagged release.** Migration and versioning policy must then be decided anew; nothing here carries forward as precedent, and any document citing this entry inherits its expiry.
- **Rationale:** Pre-release, every consumer lives on this machine and a full rebuild costs minutes; authoring migrations buys nothing and carries real failure modes (the v5 rebuild had to guard against an FK-cascade that would have deleted every chunk in the store).
- **Consequences:** Store migrations V1–V5 collapse into a single schema definition with the v5 AUTOINCREMENT guarantee baked in; their tests of migration *paths* are deleted, their tests of resulting *properties* (id non-reuse, reference integrity) are kept.
- **Supersedes:** None
- **Canonical sources:** `crates/lore/src/store/schema.rs`

## D-0019 — Declared names fold ASCII case

- **Date:** 2026-08-17
- **Status:** Accepted
- **Scope:** Extends D-0016's name identity (uniqueness and lookup)
- **Decided by:** Wrysk (2026-08-17)
- **Decision:** Declared project names are unique and resolved **ASCII-case-insensitively**, with the same folding on every platform — never the Windows-only folding root paths use, because a name is the identity that will someday travel between machines. Registering `Lore` while `lore` is bound to another root is refused (the refusal shows the stored spelling); `--project LORE` resolves `lore`; re-registering the *same* root under a case-variant is a rename, not a collision. Display always preserves the typed case. Opaque project keys stay byte-exact.
- **Rationale:** A case-variant collision is human error, not intent; Windows-first users expect forgiving lookup; platform-dependent folding of an identity would be a latent cross-machine bug.
- **Consequences:** ASCII folding only for now — full Unicode case-folding is a contract change to take deliberately if non-ASCII names ever matter.
- **Supersedes:** None (extends D-0016).
- **Canonical sources:** `crates/lore/src/registry.rs` (`name_key`)

## D-0020 — One ignore evaluator; `.loreignore` is sovereign

- **Date:** 2026-08-17
- **Status:** Accepted
- **Scope:** Ignore/exclusion semantics for observation and push. Retires the git-aware basis; also amends two D-0015 clauses (the non-overridable credential excludes and the receiver's hard-exclude content backstop — both named below), while every other D-0015 clause stands.
- **Decided by:** Wrysk ("go ahead with your simplification", 2026-08-17, after flagging D-0017's layering: ".loreignore is source of truth; gitignore is a courtesy to the user, not a correctness requirement"; amended same day, in session: defaults must be a file the user can open and chose to install, never compiled-in behavior)
- **Decision:** Exactly **one ignore evaluation** decides what gets observed, inside the walker's evaluator, with rule sources stacked lowest to highest: (1) an **optional user-level loreignore** — a plain loreignore file at the user config location, present only if the user installed it; a commented template (dot-file rule, credential patterns, ecosystem noise — all visible, all editable) ships as a `lore setup` asset, installed only by explicit invocation and never overwriting an existing file; absent means this source is empty. (2) the repo's own **`.gitignore`**, honored as a courtesy through the same evaluator — no git subprocess, no tracked/untracked distinction, no `core.excludesFile`, no global gitignore; it sits *above* the user level because a repo's declarations about itself outrank a user's machine-wide defaults. (3) the project's **`.loreignore`, sovereign** — it *inherits* the lower sources (silence keeps a user-level or gitignore rule standing) and its re-includes beat everything below. **The binary compiles in no ignore opinions at all**: out of the box, lore observes everything except the `.git/` floor and the daemon's own data dir — dotfiles and un-gitignored `.env` included. Untracked files are observed like any other file. The receiving daemon performs **structural validation only** (malformed paths, and `.git/` components as lore's own metadata floor); what a manifest may otherwise contain is the pusher's business. Retired with this: the git-aware basis and its subprocess, `[ingest] allow_secret_paths` (re-inclusion is an ordinary `!` line), the receiver's content backstop and its `refused` reporting, non-overridable credential excludes, compiled-in defaults of any kind, `.loreignore` auto-generation, and the `lore init` subcommand (`lore add` keeps writing only `.lore.toml`; hand-written `.loreignore` files work as before). A system-level source (`/etc`-style) is explicitly out of scope for now.
- **Rationale:** D-0017's implementation stacked five interacting rule systems (hard excludes, credential excludes, git basis, `.loreignore`, an exact-path override key), each pair with its own precedence quirk — ~300 lines of subprocess machinery to mimic git, plus a global-gitignore machine-state dependency the walker refuses everywhere else. One evaluator with uniform precedence deletes nearly all of it. Compiled-in defaults were rejected in the same breath: rules a user cannot open in a file are hidden behavior, and "that info isn't present in your repo" — so the default knowledge ships as an inspectable template the user chooses to install, not as evaluator behavior. Accepted trades, stated openly: out of the box nothing is protectively ignored, and a `.loreignore` re-include can admit a credential file — once remote push exists that means a bad ignore file can ship one. Both are consistent with the D-0015-session position that secrets hygiene is best-effort user responsibility and encryption-at-rest is the substantive measure.
- **Consequences:** Implementation is mostly deletion (`daemon/basis.rs`, watcher `check-ignore` batching, `allow_secret_paths`, refuse-and-report). `lore setup` becomes the single host-side command; `lore add` the single repo-side one. **Working rule adopted for this repo: one evaluator, uniform precedence — a new rule system needs a decision, not a code path.** Pre-release data escape hatch recorded in `schema.rs`: surgical work on a backup of the raw db, or decide migration support when it earns its way in.
- **Supersedes:** D-0017
- **Canonical sources:** [[../1_Architecture/1.2_Ingestion]]

## D-0021 — Symlinks do not implicitly extend a project

- **Date:** 2026-08-17
- **Status:** Accepted
- **Scope:** Link traversal during observation (symbolic links and, identically, Windows directory junctions). Extends D-0020's descent-versus-rule-source distinction; amends no D-0020 or D-0015 clause.
- **Decided by:** Wrysk (2026-08-17, adopting the strict-rule proposal in session over the agent's `!`-re-include opt-in, with "never make `follow_symlinks` an option" stated as a hard constraint)
- **Decision:** **Filesystem topology does not define project topology.** A link is a descent boundary with **no override**: a link to a directory is not descended into; a link to a file does not index its target through the link (that would place one content at two logical paths under a store keyed `(project, path)`); a link whose target escapes the project root is emphatically not followed, so that a link can never turn a project into an index of `~/secrets`, a company mount, or a vendored SDK. Creating, deleting or retargeting a link is processed as an ordinary filesystem event so that stale indexed state reconciles — what is not done is traversing the new target. The **project root may itself be a link**: it is canonicalized at registration, and the physical root is what is walked, watched, and used for containment. Windows junctions take the identical semantic to symlinks throughout (the platform reports one as the other, and no code distinguishes them). Declined links are **reported, never silently dropped**. There is **no `follow_symlinks` option and no `.lore.toml` traversal key**, and a `!` re-include in `.loreignore` does **not** rescue a link — deliberately unlike the nested-repository boundary, whose hatch it superficially resembles.
- **Rationale:** A `!` on a vendored repository and a `!` on a link are different acts: the repository's bytes are already under the root, so the only question is whose content they are and the owner may answer "mine"; a link's bytes are elsewhere, so re-including it does not reclassify content but **extends the project's extent to another part of the filesystem**. `follow_symlinks = true` would unleash cycles, duplicate aliases (two sibling links to one tree are not a cycle, and `walkdir`'s loop detection would not catch them), out-of-root escapes, ambiguous `.loreignore` semantics for content reachable at two logical paths, physical-versus-logical event paths, and separately armed watches on targets — all at once. The rule also dissolves rather than solves the hardest sub-problem: with nothing ever followed there is no out-of-root subtree to watch, so `ReadDirectoryChangesW`'s refusal to traverse reparse points stops mattering. It is the nested-repository boundary's own principle — be deliberate about whose content belongs to the project rather than walking whatever the filesystem connects — applied to the other way a filesystem connects things.
- **Consequences:** The walker keeps `follow_links(false)`; the reporting half shipped ahead of this entry (`Walk::links`, `PassSummary::links_skipped`, a warning naming the paths) and is true under this rule rather than merely compatible with it. The **split-brain case is pinned by test**: a watcher event whose path traverses a link must never reach the incremental index when the full walker would reject it, and must instead reconcile stale rows — `observe_paths` already confirms every named file by listing its parent through the same root-rooted walker, so a path behind a link is absent from the micro-manifest and, being in scope, deleted. The backlog item "the walker does not follow junctions/symlinks" is **closed by decision, not by implementation**. `Lexomancy-bench`'s `!design/`, `!tools/`, `!Lexomancy/` lines are permanently inert and should be deleted; that bench's arrangement (both slots retrieving from the separately registered `Lexomancy` project) is now policy rather than a limitation. Recorded as **direction and not design**: if external content is ever genuinely wanted, the shape to reach for is an explicit additional source root (`[[sources]]` with a declared `mount`), never a blanket follow; an intermediate `follow_symlinks = "within-project"` mode is conceivable but is not implemented and is never to become the unrestricted boolean.
- **Supersedes:** None (extends D-0020).
- **Canonical sources:** [[../1_Architecture/2026-08-17_link-traversal-decision-brief]]; [[../1_Architecture/1.2_Ingestion]]; `crates/lore/src/daemon/walk.rs`

## D-0022 — A project's extent is declared, not discovered

- **Date:** 2026-08-17
- **Status:** Accepted
- **Scope:** How a project comes to include content outside its registered root. Supplies what D-0021 displaced; amends no D-0021, D-0020 or D-0015 clause.
- **Decided by:** Wrysk (2026-08-17, "make it so" — implementing the `[[sources]]`/`mount` shape named in the D-0021 session as the explicit-configuration answer to wanting external source included)
- **Decision:** A project's extent is a **declared list of source roots** in its committed `.lore.toml`, never something inferred from the filesystem. Each `[[sources]]` entry names a `path` **relative to the project root** and a `mount` — the logical prefix its files carry. Absent the table, a project is its own root with no prefix, which is what every project was before this existed. The source that *is* the project root mounts at the top level (empty prefix) and only it may; every external root requires a `mount` name. **Every path the store, the wire and search deal in is logical** — `<mount>/<path within that root>` — so one file has exactly one address in a store keyed `(project, path)`. **Roots are independent**: ignore rules travel *down* a source root and never *between* roots, so within a root a nested `.loreignore` composes on top of that root's own (most local winning, as `.gitignore` nests) while across roots nothing is inherited in either direction. The table is validated and a bad one is **refused whole**, falling back to the project root alone with the error reported in `lore status` — never falling back to indexing nothing. Refused: an absolute `path` (the file is committed and travels), an external source at the top level, two sources sharing a `mount` (ASCII-folded), overlapping roots, a multi-component `mount`, and the project root declared twice.
- **Rationale:** D-0021 was right to refuse links but left the need behind them unserved. A declaration is auditable, gives every file one logical path, and does not depend on how a particular machine happens to be wired — which is the whole difference from following a link. This does **not** reopen D-0020: that decision deleted `[ingest] allow_secret_paths` because *exclusion rules* belong in a file at a precedence the user can argue with, and its working rule ("a new rule system needs a decision, not a code path") is about rule systems. `[[sources]]` declares no rules; it says which directories the project is — the same category as the repository and link boundaries, both already outside the ignore stack. Roots are independent because a mounted tree is somebody else's directory that this project happens to name, and reaching into it would be the declaring project crossing a boundary it does not own; it is also the cheap shape, since each root then gets its own walk with its own matcher stack, which is what the `ignore` crate does naturally. Every validation rule closes a concrete way to index one tree twice or two trees at one address — the duplicate-content failure D-0021 refuses for links, arriving through configuration instead.
- **Consequences:** Independence is only real because the walker no longer reads ignore files above a root; that change shipped alongside this entry and is load-bearing for it, since a mount at `../shared-engine` would otherwise inherit from `..` and above. The machine-wide `loreignore` still reaches every root, because it is added explicitly as the lowest rung rather than discovered by walking upward. `lore status` reports the extent — only for a project that is more than its own root — and shouts about a refused table. Extent is deliberately **not stored**: every pass reads `.lore.toml` fresh, so removing a mount makes its paths resolve to nothing, which makes them absent from the manifest and therefore deleted rather than stranded. **Known and not yet built:** a mounted root is not live-watched — `Watches` is still one root per project — so mounts refresh on a full scan or `lore index` but not on edit. Unlike the link case this is tractable (a declared mount is a real directory a watch can be armed on) and is the next piece of work, not a limitation of the design. Also unaddressed: mounting a tree that is itself a registered project indexes that content twice, once under each project; the daemon does not refuse this, because refusing a user's explicit declaration would be the layering D-0020 deleted.
- **Supersedes:** None (supplies D-0021's displaced case).
- **Canonical sources:** [[../1_Architecture/2026-08-17_link-traversal-decision-brief]]; `crates/lore/src/sources.rs`
