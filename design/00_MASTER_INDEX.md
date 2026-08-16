# Lore Design Vault — Master Index

**Design authority:** read [[0_Canon/README]] and [[0_Canon/DECISIONS]] before treating any document as binding. Written material is tentative by default; only accepted ledger entries create canon.

## Sections

- `0_Canon/` — authority model and the decision ledger (D-0001…D-0009).
- `1_Architecture/` — [[1_Architecture/1.1_Overview|1.1 Overview]]: daemon topology, subsystem inventory, storage seam.
- `2_Memory/` — [[2_Memory/2.1_Memory_Model|2.1 Memory Model]]: two-tier memory (vault + session ledger).
- `3_Retrieval/` — [[3_Retrieval/3.1_Chunking_and_Ranking|3.1 Chunking and Ranking]]: chunk identity, file classes, embedding config, ranking.
- `4_Interfaces/` — [[4_Interfaces/4.1_MCP_Surface|4.1 MCP Surface]]: tools, exclusions.
- `5_Implementation/` — [[5_Implementation/5.1_Milestones|5.1 Milestones]]: M0–M4, indexing scope, benchmark posture. Work-package briefs and design notes in `packages/`, review records and handoffs in `reviews/`.
- `6_Evaluation/` — end-to-end evaluation rounds: protocols, answer keys, results, reports.
- `7_Research/` — the 2026-08 research phase: [[7_Research/00_summary|summary]], [[7_Research/01_landscape|landscape]], [[7_Research/02_feature-matrix|feature matrix]], raw worker reports in `7_Research/raw/`.
- `99_Scratch/` — **throwaway working files only**, and never binding: drafts and notes that will certainly be deleted or superseded soon. Retrieval deliberately demotes this folder. Design documents — even exploratory ones — belong in the relevant numbered folder; so do formal reports. If a scratch file turns out to be worth keeping, move it out.

## Project state

Lore is **implemented through M1** — the repo root holds the Rust workspace, and this vault holds its design. The vault is the daemon's own dogfood corpus.
