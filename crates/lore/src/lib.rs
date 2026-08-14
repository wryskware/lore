//! Lore daemon internals. The `lore` binary (daemon + CLI) is a thin shell
//! over this library so the pieces are unit-testable.
//!
//! Module ownership (M1 work packages):
//! - [`types`] — shared data model between chunker and store (parent-owned).
//! - [`chunk`] — file → chunks (Markdown heading-tree, tree-sitter code, text windows).
//! - [`store`] — SQLite SearchStore: metadata + FTS5 + vectors, one transaction domain.
//! - [`config`] — optional `config.toml` in the data directory.
//! - [`daemon`] — lifecycle, HTTP API, watcher and indexing.

pub mod chunk;
pub mod config;
pub mod daemon;
pub mod store;
pub mod types;
