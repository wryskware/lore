//! `lore-mcp` — the stdio MCP proxy in front of the Lore daemon (D-0007).
//!
//! The whole crate is a translator: MCP tool call in, one loopback HTTP request
//! to the daemon, rendered text out. It links no store, no chunker, no embedder
//! and no watcher, so an editor can spawn as many of these as it likes without
//! any of them competing for ownership of index state.
//!
//! Exposed as a library as well as a binary purely so the golden harness in
//! `tests/mcp_golden.rs` can drive the real server over an in-memory duplex.

pub mod daemon;
pub mod render;
pub mod server;

pub use daemon::{DaemonClient, DaemonError, Endpoint};
pub use server::LoreServer;
