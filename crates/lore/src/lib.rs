//! Lore daemon internals. The `lore` binary (daemon + CLI) is a thin shell
//! over this library so the pieces are unit-testable.
//!
//! Module ownership (M1 work packages):
//! - [`types`] — shared data model between chunker and store (parent-owned).
//! - [`chunk`] — file → chunks (Markdown heading-tree, tree-sitter code, text windows).
//! - [`authority`] — declared vs effective authority: ledger parsing and the
//!   effective-tier policy ranking actually uses.
//! - [`registry`] — the `projects.toml` source manifest and stable project keys.
//! - [`store`] — SQLite SearchStore: metadata + FTS5 + vectors, one transaction domain.
//! - [`config`] — optional `config.toml` in the data directory.
//! - [`repo_config`] — the repo-committed `.lore.toml` that opts a repository
//!   into authority semantics at all (D-0012).
//! - [`embed`] — local embedding client, health and the backfill worker.
//! - [`daemon`] — lifecycle, HTTP API, watcher and indexing.
//! - [`setup`] — the agent-side assets `lore setup` installs into a coding
//!   agent host, and the never-clobber rule that governs updating them.

pub mod authority;
pub mod chunk;
pub mod config;
pub mod daemon;
pub mod embed;
pub mod registry;
pub mod repo_config;
pub mod setup;
pub mod sources;
pub mod store;
pub mod types;
