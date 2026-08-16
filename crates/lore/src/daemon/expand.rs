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
//!
//! The chunk is named by a full id **or a prefix of one** (git-style). A
//! 64-character blake3 id costs a search hit ~16 tokens it spends twice, and
//! the id is only ever a handle to hand back — so renderers print a short one
//! and this is the half that makes that safe.

use camino::Utf8Path;
use lore_core::{ExpandResponse, MIN_CHUNK_ID_PREFIX};

use crate::store::{ChunkLookup, Project, Store, StoreError};
use crate::types::Chunk;

/// Context lines applied when the request does not say.
pub const DEFAULT_CONTEXT_LINES: u32 = 20;

/// Ceiling on requested context. `expand` is a reading aid, not a file dump;
/// past a couple hundred lines the caller should ask for the file.
pub const MAX_CONTEXT_LINES: u32 = 200;

/// How many colliding ids an ambiguous prefix reports. Enough to pick from,
/// few enough that an error message stays an error message.
pub const MAX_PREFIX_CANDIDATES: usize = 8;

/// Why a chunk could not be read. Every variant's `Display` ends in the thing
/// the caller should do next, because these all reach an agent verbatim.
#[derive(Debug, thiserror::Error)]
pub enum ExpandError {
    #[error(
        "`{prefix}` is not a chunk id: chunk ids are hexadecimal; pass the chunk_id from a search result"
    )]
    NotHex { prefix: String },
    #[error(
        "chunk id prefix `{prefix}` is too short: expand needs at least {MIN_CHUNK_ID_PREFIX} \
         hexadecimal characters; pass the chunk_id from a search result"
    )]
    TooShort { prefix: String },
    #[error(
        "chunk id prefix `{prefix}` matches several chunks in `{project}`: {}; pass one of \
         them in full", candidates.join(", ")
    )]
    Ambiguous {
        prefix: String,
        project: String,
        candidates: Vec<String>,
    },
    #[error(
        "unknown chunk `{prefix}` in project `{project}`; chunk ids change when the file \
         changes, so run search again to get a current one"
    )]
    Unknown { prefix: String, project: String },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Read a chunk named by a full id or by any prefix of at least
/// [`MIN_CHUNK_ID_PREFIX`] characters.
///
/// Disambiguation is scoped to the request's project, which is what makes a
/// short prefix workable at all: the collision space is one repository's
/// chunks, not the machine's.
pub fn execute(
    store: &mut Store,
    project: &Project,
    chunk_id: &str,
    context_lines: Option<u32>,
) -> Result<ExpandResponse, ExpandError> {
    // Lowercased rather than refused: ids are printed lowercase, and a
    // capitalized paste is a copy artifact, not a different chunk.
    let prefix = chunk_id.trim().to_ascii_lowercase();
    if !prefix.chars().all(|c| c.is_ascii_hexdigit()) || prefix.is_empty() {
        return Err(ExpandError::NotHex { prefix });
    }
    if prefix.len() < MIN_CHUNK_ID_PREFIX {
        return Err(ExpandError::TooShort { prefix });
    }

    let chunk = match store.find_chunk_by_prefix(project.id, &prefix, MAX_PREFIX_CANDIDATES)? {
        ChunkLookup::Found(chunk) => *chunk,
        ChunkLookup::Unknown => {
            return Err(ExpandError::Unknown {
                prefix,
                project: project.name.clone(),
            });
        }
        ChunkLookup::Ambiguous(candidates) => {
            return Err(ExpandError::Ambiguous {
                prefix,
                project: project.name.clone(),
                candidates,
            });
        }
    };

    let context = context_lines
        .unwrap_or(DEFAULT_CONTEXT_LINES)
        .min(MAX_CONTEXT_LINES);
    let absolute = project.root.join(&chunk.path);
    Ok(widen(&chunk, &absolute, context))
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
