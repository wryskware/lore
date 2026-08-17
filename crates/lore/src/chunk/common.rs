//! Shared chunker plumbing: size limits, line index, span → [`Chunk`]
//! emission (trimming, oversize windowing, vault metadata, ID derivation).

use std::collections::HashSet;

use camino::Utf8PathBuf;

use crate::types::{Chunk, ChunkKind, DesignStatus, VaultMeta, WindowFamily};

/// Files larger than this are skipped outright (never parsed, never read
/// into a chunk). 3.1 leaves the number open; 2 MiB comfortably covers
/// hand-written source while excluding generated blobs.
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// Prefix inspected for NUL bytes when sniffing binary content.
pub const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Chunks above this many bytes are split into overlapping line windows.
pub const MAX_CHUNK_BYTES: usize = 4000;

/// Adjacent sibling declarations smaller than this are merged into one chunk
/// (the "merge tiny siblings" rule of 3.1).
pub const TINY_CHUNK_BYTES: usize = 200;

/// A container declaration (type, impl, namespace) at or below this size is
/// indexed whole instead of being split into per-member chunks.
pub const SMALL_CONTAINER_BYTES: usize = 1200;

/// Window height for the unknown-text fallback chunker.
pub const TEXT_WINDOW_LINES: u32 = 80;

/// Byte ceiling for an unknown-text window. The line count is the governing
/// rule; this only guards pathological single-line files (minified assets).
pub const TEXT_WINDOW_MAX_BYTES: usize = 8000;

/// Lines shared between consecutive windows.
pub const WINDOW_OVERLAP_LINES: u32 = 10;

/// The line windower's geometry. [`Default`] is core's, and is what every
/// built-in path uses; a chunker plugin may substitute *tighter* values (see
/// [`crate::plugin`]), never looser ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowCaps {
    pub window_lines: u32,
    /// Byte ceiling per window. Not author-settable: it exists to guard
    /// pathological single-line files, which is core's concern rather than a
    /// per-format one.
    pub max_bytes: usize,
    pub overlap_lines: u32,
}

impl Default for WindowCaps {
    fn default() -> Self {
        Self {
            window_lines: TEXT_WINDOW_LINES,
            max_bytes: TEXT_WINDOW_MAX_BYTES,
            overlap_lines: WINDOW_OVERLAP_LINES,
        }
    }
}

/// UTF-8 byte order mark. Windows editors write it routinely, it is *not*
/// whitespace (U+FEFF is `Cf`, so `trim` leaves it), and it belongs to the
/// file's encoding rather than to its content.
pub(crate) const BOM: &str = "\u{feff}";

/// Byte offsets of line starts, for exact 1-based line spans.
pub(crate) struct LineIndex {
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    pub(crate) fn new(src: &str) -> Self {
        let mut starts = vec![0usize];
        starts.extend(
            src.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self {
            starts,
            len: src.len(),
        }
    }

    /// Number of lines. A trailing newline does not open a new line.
    pub(crate) fn line_count(&self) -> u32 {
        let n = self.starts.len();
        if n > 1 && self.starts[n - 1] == self.len {
            (n - 1) as u32
        } else {
            n as u32
        }
    }

    /// 1-based line containing `byte`.
    pub(crate) fn line_of(&self, byte: usize) -> u32 {
        self.starts.partition_point(|&s| s <= byte).max(1) as u32
    }

    /// Byte offset where 1-based `line` starts.
    pub(crate) fn line_start(&self, line: u32) -> usize {
        self.starts[(line as usize - 1).min(self.starts.len() - 1)]
    }

    /// Byte offset just past 1-based `line`, including its newline.
    pub(crate) fn line_end(&self, line: u32) -> usize {
        let idx = line as usize;
        if idx < self.starts.len() {
            self.starts[idx]
        } else {
            self.len
        }
    }

    /// Splits the inclusive line range `[first, last]` into windows of at
    /// most `max_lines` lines / `max_bytes` bytes, each sharing `overlap`
    /// lines with its predecessor. Always makes forward progress.
    pub(crate) fn windows(
        &self,
        first: u32,
        last: u32,
        max_lines: u32,
        max_bytes: usize,
        overlap: u32,
    ) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let mut start = first;
        loop {
            let mut end = start;
            while end < last {
                let next = end + 1;
                if next - start + 1 > max_lines {
                    break;
                }
                // Never yields an empty window: `end` already holds one line.
                if self.line_end(next) - self.line_start(start) > max_bytes {
                    break;
                }
                end = next;
            }
            out.push((start, end));
            if end >= last {
                return out;
            }
            start = (end + 1).saturating_sub(overlap).max(start + 1);
        }
    }
}

/// File-level vault metadata, cloned onto every chunk of the file.
#[derive(Debug, Clone, Default)]
pub(crate) struct FileVault {
    pub design_status: Option<DesignStatus>,
    pub decision_refs: Vec<String>,
}

/// Structural identity of a chunk before oversize windowing is applied.
pub(crate) enum Tpl<'a> {
    Code {
        symbol_kind: &'a str,
        symbol_path: &'a str,
    },
    Section {
        heading_path: &'a [String],
    },
}

impl Tpl<'_> {
    /// `window` is `Some` only when the span was split. Its index becomes the
    /// `#w<n>` anchor discriminator, which is what keeps the derived IDs of
    /// the parts distinct and stable; the family ordinal rides alongside as
    /// metadata so ranking can recognise the parts without re-deriving
    /// membership from a string anyone may also have typed.
    fn kind(&self, window: Option<WindowFamily>) -> ChunkKind {
        match self {
            Tpl::Code {
                symbol_kind,
                symbol_path,
            } => ChunkKind::Code {
                symbol_kind: (*symbol_kind).to_string(),
                symbol_path: match window {
                    Some(w) => format!("{symbol_path}#w{}", w.index),
                    None => (*symbol_path).to_string(),
                },
                window,
            },
            Tpl::Section { heading_path } => {
                let mut path = heading_path.to_vec();
                if let Some(w) = window {
                    path.push(format!("#w{}", w.index));
                }
                ChunkKind::Section {
                    heading_path: path,
                    window,
                }
            }
        }
    }
}

/// Collects chunks for one file: trims spans, splits oversized ones, derives
/// IDs, and guarantees intra-file ID uniqueness.
pub(crate) struct Emitter<'a> {
    path: Utf8PathBuf,
    language: Option<String>,
    src: &'a str,
    lines: LineIndex,
    vault: Option<FileVault>,
    /// Next [`WindowFamily::family`] ordinal for this file. One counter for
    /// the whole file (not per anchor), so two oversized `Parser.Parse`
    /// overloads land in different families even though their windows spell
    /// the same `#w<n>` suffixes.
    next_family: u32,
    out: Vec<Chunk>,
}

impl<'a> Emitter<'a> {
    pub(crate) fn new(path: &Utf8PathBuf, language: Option<&str>, src: &'a str) -> Self {
        Self {
            path: path.clone(),
            language: language.map(str::to_string),
            src,
            lines: LineIndex::new(src),
            vault: None,
            next_family: 0,
            out: Vec::new(),
        }
    }

    pub(crate) fn with_vault(mut self, vault: FileVault) -> Self {
        self.vault = Some(vault);
        self
    }

    /// Emits `[start, end)` as one chunk, or as overlapping line windows when
    /// it exceeds [`MAX_CHUNK_BYTES`].
    pub(crate) fn push(&mut self, start: usize, end: usize, tpl: Tpl<'_>) {
        let Some((start, end)) = trim_span(self.src, start, end) else {
            return;
        };
        if end - start <= MAX_CHUNK_BYTES {
            self.emit(start, end, tpl.kind(None));
            return;
        }
        let first = self.lines.line_of(start);
        let last = self.lines.line_of(end - 1);
        let windows =
            self.lines
                .windows(first, last, u32::MAX, MAX_CHUNK_BYTES, WINDOW_OVERLAP_LINES);
        // One family per *split span*, claimed even when the span degenerates
        // to a single window (a 4 KB+ single line): the metadata and the
        // `#w<n>` suffix must agree, or ranking's "this is bookkeeping" test
        // would disagree with the anchor it is looking at.
        let family = self.next_family;
        self.next_family += 1;
        for (i, (a, b)) in windows.iter().enumerate() {
            let ws = self.lines.line_start(*a).max(start);
            let we = self.lines.line_end(*b).min(end);
            if let Some((ws, we)) = trim_span(self.src, ws, we) {
                self.emit(
                    ws,
                    we,
                    tpl.kind(Some(WindowFamily {
                        family,
                        index: i as u32,
                    })),
                );
            }
        }
    }

    /// Emits `[start, end)` verbatim under `kind`, without windowing.
    pub(crate) fn push_exact(&mut self, start: usize, end: usize, kind: ChunkKind) {
        if let Some((start, end)) = trim_span(self.src, start, end) {
            self.emit(start, end, kind);
        }
    }

    fn emit(&mut self, start: usize, end: usize, kind: ChunkKind) {
        let text = &self.src[start..end];
        let vault = self.vault.as_ref().map(|v| VaultMeta {
            design_status: v.design_status,
            decision_refs: v.decision_refs.clone(),
            body_decision_refs: decision_refs(text),
        });
        self.out.push(Chunk {
            id: Chunk::derive_id(&self.path, &kind, text),
            path: self.path.clone(),
            kind,
            language: self.language.clone(),
            byte_start: start as u32,
            byte_end: end as u32,
            line_start: self.lines.line_of(start),
            line_end: self.lines.line_of(end - 1),
            text: text.to_string(),
            vault,
        });
    }

    /// Finishes the file, disambiguating any colliding IDs (identical anchor
    /// *and* identical text, e.g. two identical merged runs).
    pub(crate) fn finish(mut self) -> Vec<Chunk> {
        let mut seen: HashSet<crate::types::ChunkId> = HashSet::new();
        for chunk in &mut self.out {
            let mut dup = 1u32;
            while !seen.insert(chunk.id.clone()) {
                chunk.kind = disambiguate(&chunk.kind, dup);
                chunk.id = Chunk::derive_id(&chunk.path, &chunk.kind, &chunk.text);
                dup += 1;
            }
        }
        self.out
    }
}

/// Adds a `#d<n>` discriminator. Window membership is *preserved*: two
/// identical windows of two different families collide on anchor and text, and
/// disambiguating them must not also erase the family that keeps them apart.
fn disambiguate(kind: &ChunkKind, dup: u32) -> ChunkKind {
    match kind {
        ChunkKind::Code {
            symbol_kind,
            symbol_path,
            window,
        } => ChunkKind::Code {
            symbol_kind: symbol_kind.clone(),
            symbol_path: format!("{symbol_path}#d{dup}"),
            window: *window,
        },
        ChunkKind::Section {
            heading_path,
            window,
        } => {
            let mut path = heading_path.clone();
            path.push(format!("#d{dup}"));
            ChunkKind::Section {
                heading_path: path,
                window: *window,
            }
        }
        ChunkKind::Window { index } => ChunkKind::Window { index: *index },
    }
}

/// Shrinks a span to its non-whitespace content so `byte_*`/`line_*` are
/// exact and chunk text stays a verbatim file slice.
///
/// A leading [`BOM`] is stepped over here rather than in each chunker: it is
/// encoding, not content, and every chunk any chunker emits passes through
/// this one function. Without it the mark rides inside the first chunk of
/// every code and plain-text file a Windows editor saved, polluting both the
/// embedded text and the FTS row. Only a span that starts at byte 0 can carry
/// it, and the offset moves rather than the file, so spans stay exact against
/// the original bytes. (Markdown steps over it earlier as well, because its
/// frontmatter fence and first heading have to be recognised *through* it.)
pub(crate) fn trim_span(src: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let start = match start == 0 && src.starts_with(BOM) {
        true => BOM.len(),
        false => start,
    };
    if start >= end || end > src.len() {
        return None;
    }
    let slice = &src[start..end];
    let lead = slice.len() - slice.trim_start().len();
    let tail = slice.len() - slice.trim_end().len();
    if lead + tail >= slice.len() {
        return None;
    }
    Some((start + lead, end - tail))
}

/// Unique `D-NNNN` references in `text`, in order of first appearance.
pub(crate) fn decision_refs(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 6 <= bytes.len() {
        if bytes[i] == b'D'
            && bytes[i + 1] == b'-'
            && bytes[i + 2..i + 6].iter().all(u8::is_ascii_digit)
            && !i.checked_sub(1).is_some_and(|p| is_word_byte(bytes[p]))
            && !bytes.get(i + 6).copied().is_some_and(is_word_byte)
        {
            let found = &text[i..i + 6];
            if !out.iter().any(|r| r == found) {
                out.push(found.to_string());
            }
            i += 6;
            continue;
        }
        i += 1;
    }
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
