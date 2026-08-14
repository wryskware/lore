//! `POST /v1/expand` — widen a search hit into surrounding file context.
//!
//! The text comes from **disk, not the index**. `search` is deliberately
//! token-lean, and the whole reason an agent calls `expand` is to read what
//! is actually there right now; serving the stored chunk would hand back a
//! snapshot that may predate the last three edits. The index is used only to
//! locate the chunk (path + line span).
//!
//! When disk and index disagree — file deleted, moved, rewritten shorter, or
//! no longer UTF-8 — the stored chunk text is returned instead of an error.
//! An agent asking "show me more of this" is better served by the stale text
//! it already has a citation for than by a 404 it cannot act on. The response
//! is still self-describing: `file_lines` equals the returned span's end, so
//! a caller can see that no wider context was available.

use camino::Utf8Path;
use lore_core::ExpandResponse;

use crate::store::{Project, Store, StoreError};
use crate::types::{Chunk, ChunkId};

/// Context lines applied when the request does not say.
pub const DEFAULT_CONTEXT_LINES: u32 = 20;

/// Ceiling on requested context. `expand` is a reading aid, not a file dump;
/// past a couple hundred lines the caller should ask for the file.
pub const MAX_CONTEXT_LINES: u32 = 200;

/// `Ok(None)` means the chunk id is unknown in this project.
pub fn execute(
    store: &mut Store,
    project: &Project,
    chunk_id: &str,
    context_lines: Option<u32>,
) -> Result<Option<ExpandResponse>, StoreError> {
    let Some(chunk) = store.get_chunk(project.id, &ChunkId(chunk_id.to_string()))? else {
        return Ok(None);
    };
    let context = context_lines
        .unwrap_or(DEFAULT_CONTEXT_LINES)
        .min(MAX_CONTEXT_LINES);
    let absolute = project.root.join(&chunk.path);
    Ok(Some(widen(&chunk, &absolute, context)))
}

fn widen(chunk: &Chunk, absolute: &Utf8Path, context: u32) -> ExpandResponse {
    let Ok(bytes) = std::fs::read(absolute) else {
        tracing::debug!(path = %chunk.path, "expand: file unreadable; serving stored chunk");
        return stored(chunk);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return stored(chunk);
    };

    let lines: Vec<&str> = text.lines().collect();
    let file_lines = lines.len() as u32;
    // The file changed under the index (shorter now, or the chunk predates a
    // rewrite): the stored span no longer addresses anything real.
    if chunk.line_start == 0 || chunk.line_start > file_lines {
        return stored(chunk);
    }

    let start = chunk.line_start.saturating_sub(context).max(1);
    let end = chunk.line_end.saturating_add(context).min(file_lines);
    let slice = &lines[(start - 1) as usize..=(end - 1) as usize];

    ExpandResponse {
        path: chunk.path.to_string(),
        line_start: start,
        line_end: end,
        text: slice.join("\n"),
        file_lines,
    }
}

fn stored(chunk: &Chunk) -> ExpandResponse {
    ExpandResponse {
        path: chunk.path.to_string(),
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        text: chunk.text.clone(),
        file_lines: chunk.line_end,
    }
}
