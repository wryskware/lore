---
design_status: exploration
last_reviewed: 2026-08-14
---

# Deferred backlog

Items deliberately deferred out of the M1 review fix waves. Tracked here
instead of GitHub issues (solo repo). Source: session-1 review + package
briefs; details in the cited docs.

- **Discovery version negotiation (S1#4).** Scalar `api_version` +
  hard-coded `/v1` in `Handshake::base_url` defeat /v1+/v2 coexistence.
  Advertise supported versions/endpoints; make the route data-driven.
- **Engine-neutral store trait (S1#6).** `StoreHandle` holds concrete
  `Store`; `StoreError` leaks rusqlite types. Define the trait + neutral
  error around what daemon/search/embed actually use; policy above the seam.
  Interface-level only — no second engine until M4 earns it.
- **Embed worker parked via query-side health demotion.** Residual from the
  embed fix wave (ef650a2): needs a proper wake/recovery path.
- **Loopback-without-auth, before M3 `session_log`.** A write endpoint
  changes the threat model; decide auth posture before it ships.
- **BOM handling in code/text chunkers.** Markdown steps over the BOM;
  code/plain-text chunkers leak it into the first chunk.
- **ATX trailing-`#` trimming.** `scan_headings` trims unconditionally, so
  `# Learning C#` loses its `#` in heading paths/anchors.
- **`path_glob` search filter.** 4.1 was amended (S1#8) to document the
  implemented literal prefix; a real glob (`Assets/**/Tests/*.cs`, globset,
  SQL prefix pushdown, Windows case-folding parity in both arms) is still
  wanted for Unity workflows.

## Dogfood findings (2026-08-15, first daemon session)

- **Transient "authority declaration not honored" warnings on first scan.**
  During a full scan that (re)indexes files, per-file authority is evaluated
  before the ledger row lands, so validly-cited `decided` docs get a WARN
  that the same pass's recompute immediately reverses. End state is correct;
  the log misleads. Defer or suppress the warning until after ledger parse.
- **Zombie daemon.** The 2026-08-14 daemon (pre-fix-wave binary) was found
  alive but wedged: process running ~24 h, handshake stale, HTTP unresponsive.
  Cause unknown; possibly an already-fixed hang. Watch for recurrence on the
  current binary before investigating.
- ~~Ledger parser retired partially-superseded decisions~~ — fixed same day
  (bare-ID-list rule, Wrysk's call; see D-0010 and `authority.rs`).

## Residuals from the package-3 merge (2026-08-15)

- **Unreadable-ledger degradation is untested.** `refresh_authority` keeps
  the stored active set when a ledger read fails (so a transient IO error
  cannot mass-demote the vault), but no test exercises it — making a file
  reliably unreadable on Windows needs platform locking machinery.
- **Pre-V2 duplicate display names are left in place.** Uniqueness is
  enforced for new registrations only; `resolve_project("shared")` returns
  the first match while project keys reach both. Acceptable migration
  behavior; a `lore rename` affordance would let a user clean up.
- **`registry::bootstrap` key backfill is per-row, not atomic.** Safe as
  written (only allocates unheld keys); worth folding into
  `apply_project_set` if bootstrap ever grows.
- **Authority multiplier dominance is only proven at fixture scale.** The
  ordering (0.7 scratch cap below neutral, etc.) is tested at the ranks the
  fixtures produce, not against arbitrary RRF gaps on large corpora.
