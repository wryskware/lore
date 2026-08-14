//! Chunking pipeline — file content → [`crate::types::Chunk`]s.
//!
//! Strategies per 3.1 Chunking_and_Ranking: tree-sitter symbol chunks for
//! code (C# flagship), heading-tree leaves for Markdown with vault schema
//! awareness, line windows for unknown text, skip binary.
//!
//! Implementation lands in work package 3 (task #3).
