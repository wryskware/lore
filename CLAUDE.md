# Lore — Working Rules

Lore is a local context daemon for AI coding agents (Rust). This repo holds the design vault (`design/`) now and the Rust workspace later.

Before using or editing design material, read `design/0_Canon/README.md` and the relevant entries in `design/0_Canon/DECISIONS.md`.

- Written, polished, detailed, or implemented does not mean canonical. Only active accepted ledger entries and their cited sources are binding.
- Treat documents without `design_status` as unclassified exploration.
- Preserve modality when synthesizing; never turn a leaning, proposal, example, or current implementation into a requirement.
- Do not add an accepted decision, mark a document `decided`, or supersede canon without Wrysk's explicit authorization for that choice.
- Research reports under `design/7_Research/` are evidence, not decisions; their raw worker reports may contain unverified claims — the synthesized docs note which claims were parent-verified.
- Hard constraints (D-0003): Windows-native, C#/Unity flagship, local-only embeddings, single authoritative owner of index state.

## Git workflow

- Work directly on `main`; push feature commits there. Commit early, commit often — small, logical units with clear messages.
- **Prefer rebase over merge.** Integrate by rebasing onto `main` (`git pull --rebase`, `git rebase origin/main`) rather than merging, so history stays linear and free of merge commits. Reserve `--no-ff` merges for cases where preserving a branch's shape genuinely matters, and say why.
- Feature branches + `git worktree` only for truly parallel tasks; rebase back promptly. Delete the branch as part of integrating it — a merged or rebased branch left sitting around is not "your call", it is unfinished cleanup. If you remove a worktree but keep its branch, say so explicitly.
- Standard hygiene otherwise: don't mix unrelated changes in one commit; design-vault edits and code changes are separate commits when practical; never rewrite pushed history.
- Run `cargo fmt --all` before every commit that touches Rust code. CI enforces this with `cargo fmt --all --check` and fails the build on unformatted code.
