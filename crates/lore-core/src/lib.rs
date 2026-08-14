//! Shared types for the Lore daemon HTTP API.
//!
//! Every client (CLI subcommands, the `lore-mcp` thin proxy) talks to the
//! daemon exclusively through these types over versioned loopback HTTP
//! (design: D-0007). Nothing here touches index state.

use serde::{Deserialize, Serialize};

/// API version negotiated on every request; bump on breaking changes.
pub const API_VERSION: u32 = 1;

/// Daemon health/status snapshot returned by `GET /v1/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub api_version: u32,
    pub daemon_version: String,
}
