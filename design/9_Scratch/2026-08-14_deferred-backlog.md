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
