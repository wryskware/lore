//! Unknown-text fallback: overlapping line windows (3.1).

use camino::Utf8PathBuf;

use super::common::{Emitter, LineIndex, WindowCaps};
use crate::types::{Chunk, ChunkKind};

/// `language` is `Some` only when a known language failed to parse and we are
/// degrading to windows rather than dropping the file.
pub(crate) fn chunk_text(path: &Utf8PathBuf, src: &str, language: Option<&str>) -> Vec<Chunk> {
    chunk_text_with(path, src, language, WindowCaps::default())
}

/// [`chunk_text`] with explicit geometry — the seam a `windows`-strategy
/// chunker plugin parameterizes. Identical to the default path when `caps` is
/// [`WindowCaps::default`], which is what keeps a file no plugin claims
/// byte-identical to before plugins existed.
pub(crate) fn chunk_text_with(
    path: &Utf8PathBuf,
    src: &str,
    language: Option<&str>,
    caps: WindowCaps,
) -> Vec<Chunk> {
    let lines = LineIndex::new(src);
    let spans: Vec<(usize, usize)> = lines
        .windows(
            1,
            lines.line_count(),
            caps.window_lines,
            caps.max_bytes,
            caps.overlap_lines,
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
