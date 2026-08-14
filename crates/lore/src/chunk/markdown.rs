//! Markdown chunker: heading-tree leaves with `heading_path` provenance and
//! vault-schema awareness (3.1; D-0004).
//!
//! * Every chunk of a `.md` file carries [`VaultMeta`]; a file without
//!   frontmatter is *unclassified*, i.e. `design_status: None`.
//! * A heading's chunk runs from its own line to the next heading of any
//!   level. For a leaf that is the whole section; for a heading with
//!   subheadings that is its intro text, which is emitted only when it is
//!   substantial enough to be worth a vector on its own.
//! * Text before the first heading is a chunk with an empty `heading_path`.
//! * The frontmatter block itself is never a chunk.

use camino::Utf8PathBuf;

use super::common::{Emitter, FileVault, Tpl, decision_refs, trim_span};
use crate::types::{Chunk, DesignStatus};

/// A container heading's intro must be at least this many bytes (beyond the
/// heading line) to earn its own chunk; below that it is heading noise.
const MIN_SECTION_INTRO_BYTES: usize = 24;

pub(crate) fn chunk_markdown(path: &Utf8PathBuf, src: &str) -> Vec<Chunk> {
    let (vault, body_start) = parse_frontmatter(src);
    let headings = scan_headings(src, body_start);
    let mut emitter = Emitter::new(path, Some("markdown"), src).with_vault(vault);

    let first = headings.first().map_or(src.len(), |h| h.start);
    emitter.push(body_start, first, Tpl::Section { heading_path: &[] });

    let mut stack: Vec<(u8, String)> = Vec::new();
    for (i, heading) in headings.iter().enumerate() {
        while stack.last().is_some_and(|(lvl, _)| *lvl >= heading.level) {
            stack.pop();
        }
        let next = headings.get(i + 1);
        let end = next.map_or(src.len(), |h| h.start);
        let path: Vec<String> = stack
            .iter()
            .map(|(_, title)| title.clone())
            .chain(std::iter::once(heading.title.clone()))
            .collect();

        let has_children = next.is_some_and(|n| n.level > heading.level);
        let intro = trim_span(src, heading.body_start, end).map_or(0, |(s, e)| e - s);
        if !has_children || intro >= MIN_SECTION_INTRO_BYTES {
            emitter.push(
                heading.start,
                end,
                Tpl::Section {
                    heading_path: &path,
                },
            );
        }
        stack.push((heading.level, heading.title.clone()));
    }
    emitter.finish()
}

struct Heading {
    level: u8,
    title: String,
    /// Byte offset of the `#` line.
    start: usize,
    /// Byte offset just past the heading line.
    body_start: usize,
}

/// ATX headings outside fenced code blocks. Setext (`===`) headings are not
/// recognized — a documented gap.
fn scan_headings(src: &str, from: usize) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut pos = from;
    while pos < src.len() {
        let end = src[pos..]
            .find('\n')
            .map_or(src.len(), |offset| pos + offset + 1);
        let line = &src[pos..end];
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next();
        match (fence, marker) {
            (Some((ch, len)), Some(c)) if c == ch => {
                let run = trimmed.chars().take_while(|x| *x == ch).count();
                if run >= len && trimmed[run..].trim().is_empty() {
                    fence = None;
                }
            }
            (None, Some(c @ ('`' | '~'))) => {
                let run = trimmed.chars().take_while(|x| *x == c).count();
                if run >= 3 {
                    fence = Some((c, run));
                }
            }
            (None, Some('#')) => {
                let level = trimmed.chars().take_while(|c| *c == '#').count();
                let rest = &trimmed[level..];
                if (1..=6).contains(&level)
                    && (rest.trim().is_empty() || rest.starts_with([' ', '\t']))
                {
                    out.push(Heading {
                        level: level as u8,
                        title: rest.trim().trim_end_matches('#').trim().to_string(),
                        start: pos,
                        body_start: end,
                    });
                }
            }
            _ => {}
        }
        pos = end;
    }
    out
}

/// Minimal YAML subset: top-level `key: value`, block sequences (`  - item`)
/// and inline sequences (`[a, b]`). Enough for `design_status` and
/// `decision_refs`; anything else in the block is ignored. Returns the vault
/// metadata and the byte offset where the document body starts.
fn parse_frontmatter(src: &str) -> (FileVault, usize) {
    let mut vault = FileVault::default();
    let first_break = match src.find('\n') {
        Some(i) => i,
        None => return (vault, 0),
    };
    if src[..first_break].trim_end() != "---" {
        return (vault, 0);
    }
    let mut pos = first_break + 1;
    let mut key = String::new();
    while pos < src.len() {
        let end = src[pos..]
            .find('\n')
            .map_or(src.len(), |offset| pos + offset + 1);
        let line = &src[pos..end];
        pos = end;
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            return (vault, pos);
        }
        let indented = line.starts_with([' ', '\t']);
        let body = trimmed.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if let Some(item) = body.strip_prefix("- ") {
            if key == "decision_refs" {
                push_refs(&mut vault.decision_refs, item);
            }
            continue;
        }
        let Some((name, value)) = body.split_once(':') else {
            continue;
        };
        if indented {
            continue; // nested mapping; out of scope for this subset
        }
        key = name.trim().to_string();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "design_status" => vault.design_status = parse_status(&clean(value)),
            "decision_refs" => push_refs(&mut vault.decision_refs, value),
            _ => {}
        }
    }
    // Unterminated frontmatter: treat the file as plain Markdown.
    (FileVault::default(), 0)
}

fn parse_status(value: &str) -> Option<DesignStatus> {
    match value.to_ascii_lowercase().as_str() {
        "exploration" => Some(DesignStatus::Exploration),
        "leaning" => Some(DesignStatus::Leaning),
        "decided" => Some(DesignStatus::Decided),
        "deprecated" => Some(DesignStatus::Deprecated),
        _ => None,
    }
}

fn push_refs(out: &mut Vec<String>, raw: &str) {
    let value = raw.trim();
    let value = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    for part in value.split(',') {
        let cleaned = clean(part);
        if cleaned.is_empty() {
            continue;
        }
        let refs = decision_refs(&cleaned);
        if refs.is_empty() {
            if !out.contains(&cleaned) {
                out.push(cleaned);
            }
        } else {
            for found in refs {
                if !out.contains(&found) {
                    out.push(found);
                }
            }
        }
    }
}

fn clean(value: &str) -> String {
    value
        .trim()
        .trim_matches(['"', '\''])
        .trim_start_matches("[[")
        .trim_end_matches("]]")
        .trim()
        .to_string()
}
