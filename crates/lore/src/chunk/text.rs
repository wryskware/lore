//! Unknown-text fallback: overlapping line windows (3.1).

use camino::Utf8PathBuf;

use super::common::{
    Emitter, LineIndex, TEXT_WINDOW_LINES, TEXT_WINDOW_MAX_BYTES, WINDOW_OVERLAP_LINES,
};
use crate::types::{Chunk, ChunkKind};

/// `language` is `Some` only when a known language failed to parse and we are
/// degrading to windows rather than dropping the file.
pub(crate) fn chunk_text(path: &Utf8PathBuf, src: &str, language: Option<&str>) -> Vec<Chunk> {
    let lines = LineIndex::new(src);
    let spans: Vec<(usize, usize)> = lines
        .windows(
            1,
            lines.line_count(),
            TEXT_WINDOW_LINES,
            TEXT_WINDOW_MAX_BYTES,
            WINDOW_OVERLAP_LINES,
        )
        .into_iter()
        .map(|(first, last)| (lines.line_start(first), lines.line_end(last)))
        .collect();

    let mut emitter = Emitter::new(path, language, src);
    for (index, (start, end)) in spans.into_iter().enumerate() {
        emitter.push_exact(
            start,
            end,
            ChunkKind::Window {
                index: index as u32,
            },
        );
    }
    emitter.finish()
}
