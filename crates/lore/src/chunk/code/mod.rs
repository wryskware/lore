//! Language-agnostic tree-sitter chunker.
//!
//! One cursor walk, driven by a per-language [`Spec`] of node-kind sets. No
//! call or reference extraction anywhere (D-0005) — declarations only.
//!
//! Granularity rules (3.1 "symbol-level nodes with leading comments attached;
//! merge tiny siblings, split oversized with overlap"):
//!
//! * A container (type, impl block, namespace) at or below
//!   [`SMALL_CONTAINER_BYTES`] is indexed whole — small types read better as
//!   one unit than as a header plus three two-line members.
//! * A larger container yields a *header* chunk (doc comments, attributes,
//!   the declaration line, and any leading tiny members such as fields) that
//!   stops where its first substantial member begins, plus one chunk per
//!   member. Member bodies are therefore never indexed twice.
//! * Namespace-like containers ([`Spec::path_only`]) contribute a path
//!   segment only; they never emit a header chunk of their own.
//! * Runs of adjacent members below [`TINY_CHUNK_BYTES`] merge into one
//!   chunk, capped at [`MAX_CHUNK_BYTES`].
//! * Anything that is not a recognized declaration (using directives,
//!   imports, top-level statements, preprocessor lines) is kept as a
//!   `statements` filler chunk so no file content is silently dropped.

mod csharp;
mod python;
mod rust;
mod webish;

use camino::Utf8PathBuf;
use tree_sitter::{Language, Node, Parser};

use super::common::{
    Emitter, MAX_CHUNK_BYTES, SMALL_CONTAINER_BYTES, TINY_CHUNK_BYTES, Tpl, trim_span,
};
use crate::types::Chunk;

/// Per-language node-kind vocabulary. Kind lists are cheap supersets; an
/// unknown kind simply falls through to filler.
pub(crate) struct Spec {
    /// Grammar constructor.
    pub language: fn() -> Language,
    /// Value for [`Chunk::language`].
    pub tag: &'static str,
    /// Containers that only contribute a path segment (namespaces, modules).
    pub path_only: &'static [&'static str],
    /// Containers whose members become chunks of their own.
    pub containers: &'static [&'static str],
    /// Leaf declarations worth a chunk.
    pub symbols: &'static [&'static str],
    /// Nodes wrapping a declaration (`export`, decorators, C# global stmts).
    pub wrappers: &'static [&'static str],
    /// Body nodes searched for members of a container.
    pub bodies: &'static [&'static str],
    /// Preceding siblings that belong to the declaration that follows them.
    pub attachments: &'static [&'static str],
    /// Declarations that scope every *following* sibling rather than their
    /// own children (C# file-scoped namespaces).
    pub trailing_scope: &'static [&'static str],
    /// Declaration name extraction.
    pub name_of: fn(Node<'_>, &str) -> Option<String>,
}

/// Returns the spec for a lowercase language tag, if it is one we parse.
pub(crate) fn spec_for(tag: &str) -> Option<Spec> {
    match tag {
        "csharp" => Some(csharp::spec()),
        "rust" => Some(rust::spec()),
        "python" => Some(python::spec()),
        "javascript" => Some(webish::javascript()),
        "typescript" => Some(webish::typescript()),
        "tsx" => Some(webish::tsx()),
        _ => None,
    }
}

/// Chunks source text with the given language spec. Returns `None` when the
/// grammar refuses the source, so the caller can fall back to text windows.
pub(crate) fn chunk_code(spec: &Spec, path: &Utf8PathBuf, src: &str) -> Option<Vec<Chunk>> {
    let mut parser = Parser::new();
    parser.set_language(&(spec.language)()).ok()?;
    let tree = parser.parse(src, None)?;
    let mut emitter = Emitter::new(path, Some(spec.tag), src);
    let root = tree.root_node();
    let items = collect_items(spec, src, root, "");
    emit_items(spec, src, &items, &mut emitter);
    Some(emitter.finish())
}

/// One candidate chunk within a scope.
#[derive(Clone)]
struct Item<'t> {
    /// Span start, including attached comments/attributes.
    start: usize,
    end: usize,
    /// Declaration node (`None` for filler runs).
    node: Option<Node<'t>>,
    symbol_path: String,
    symbol_kind: String,
    container: bool,
}

impl Item<'_> {
    fn size(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    fn is_filler(&self) -> bool {
        self.node.is_none()
    }
}

fn collect_items<'t>(spec: &Spec, src: &str, scope: Node<'t>, prefix: &str) -> Vec<Item<'t>> {
    let mut cursor = scope.walk();
    let children: Vec<Node<'t>> = scope.named_children(&mut cursor).collect();
    let (starts, absorbed) = attach_leading(spec, src, &children);
    let mut items: Vec<Item<'t>> = Vec::new();
    let mut scope_prefix = prefix.to_string();
    let mut filler_ordinal = 0u32;

    for (i, node) in children.iter().enumerate() {
        if absorbed[i] {
            continue; // part of the declaration that follows it
        }
        let start = starts[i];
        let inner = unwrap(spec, *node);
        let kind = inner.kind();

        if spec.trailing_scope.contains(&kind) {
            let name = name_of(spec, inner, src);
            scope_prefix = dotted(prefix, &name);
            continue;
        }

        let container = spec.containers.contains(&kind) || spec.path_only.contains(&kind);
        if container || spec.symbols.contains(&kind) {
            let name = name_of(spec, inner, src);
            items.push(Item {
                start,
                end: node.end_byte(),
                node: Some(inner),
                symbol_path: dotted(&scope_prefix, &name),
                symbol_kind: kind.to_string(),
                container,
            });
            continue;
        }

        match items.last_mut() {
            Some(last) if last.is_filler() => last.end = node.end_byte(),
            _ => {
                items.push(Item {
                    start,
                    end: node.end_byte(),
                    node: None,
                    symbol_path: dotted(&scope_prefix, &format!("#s{filler_ordinal}")),
                    symbol_kind: "statements".to_string(),
                    container: false,
                });
                filler_ordinal += 1;
            }
        }
    }
    items
}

fn emit_items(spec: &Spec, src: &str, items: &[Item<'_>], emitter: &mut Emitter<'_>) {
    let mut i = 0;
    while i < items.len() {
        let item = &items[i];
        if item.container && item.size() > SMALL_CONTAINER_BYTES {
            emit_container(spec, src, item, emitter);
            i += 1;
            continue;
        }
        if item.size() < TINY_CHUNK_BYTES {
            let mut j = i + 1;
            let mut end = item.end;
            while j < items.len()
                && items[j].size() < TINY_CHUNK_BYTES
                && items[j].end - item.start <= MAX_CHUNK_BYTES
            {
                end = items[j].end;
                j += 1;
            }
            let merged = j > i + 1;
            emitter.push(
                item.start,
                end,
                Tpl::Code {
                    symbol_kind: if merged { "group" } else { &item.symbol_kind },
                    symbol_path: &item.symbol_path,
                },
            );
            i = j;
            continue;
        }
        emitter.push(
            item.start,
            item.end,
            Tpl::Code {
                symbol_kind: &item.symbol_kind,
                symbol_path: &item.symbol_path,
            },
        );
        i += 1;
    }
}

fn emit_container(spec: &Spec, src: &str, item: &Item<'_>, emitter: &mut Emitter<'_>) {
    let node = item.node.expect("containers carry their node");
    let members = collect_items(spec, src, body_scope(spec, node), &item.symbol_path);
    let whole = Tpl::Code {
        symbol_kind: &item.symbol_kind,
        symbol_path: &item.symbol_path,
    };
    if members.is_empty() {
        emitter.push(item.start, item.end, whole);
        return;
    }
    if spec.path_only.contains(&node.kind()) {
        emit_items(spec, src, &members, emitter);
        return;
    }
    // Header runs up to the first substantial member; leading tiny members
    // (fields, constants) ride along with the declaration they belong to.
    match members
        .iter()
        .position(|m| m.container || m.size() >= TINY_CHUNK_BYTES)
    {
        None => emitter.push(item.start, item.end, whole),
        Some(k) => {
            if trim_span(src, item.start, members[k].start).is_some() {
                emitter.push(item.start, members[k].start, whole);
            }
            emit_items(spec, src, &members[k..], emitter);
        }
    }
}

/// Extends each declaration's span backwards over the comments/attributes
/// that immediately precede it (no blank line in between), and reports which
/// siblings were consumed that way. A comment that attaches to nothing — a
/// file header followed by a blank line, a trailing note — stays behind as
/// its own content so no bytes are silently dropped.
fn attach_leading(spec: &Spec, src: &str, children: &[Node<'_>]) -> (Vec<usize>, Vec<bool>) {
    let mut starts: Vec<usize> = children.iter().map(Node::start_byte).collect();
    let mut absorbed = vec![false; children.len()];
    for i in 0..children.len() {
        if spec.attachments.contains(&children[i].kind()) {
            continue;
        }
        let mut j = i;
        while j > 0 {
            let prev = children[j - 1];
            if !spec.attachments.contains(&prev.kind()) {
                break;
            }
            let gap = &src[prev.end_byte()..children[j].start_byte()];
            if !gap.trim().is_empty() || gap.matches('\n').count() > 1 {
                break;
            }
            absorbed[j - 1] = true;
            starts[i] = prev.start_byte();
            j -= 1;
        }
    }
    (starts, absorbed)
}

fn unwrap<'t>(spec: &Spec, node: Node<'t>) -> Node<'t> {
    if !spec.wrappers.contains(&node.kind()) {
        return node;
    }
    for field in ["declaration", "definition", "value"] {
        if let Some(inner) = node.child_by_field_name(field) {
            return unwrap(spec, inner);
        }
    }
    let mut cursor = node.walk();
    let inner = node
        .named_children(&mut cursor)
        .find(|c| !spec.attachments.contains(&c.kind()));
    match inner {
        Some(inner) => unwrap(spec, inner),
        None => node,
    }
}

fn body_scope<'t>(spec: &Spec, node: Node<'t>) -> Node<'t> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| spec.bodies.contains(&c.kind()))
        .unwrap_or(node)
}

fn name_of(spec: &Spec, node: Node<'_>, src: &str) -> String {
    (spec.name_of)(node, src).unwrap_or_else(|| node.kind().to_string())
}

fn dotted(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

/// Text of `node`, collapsed to a single line (names never span lines in
/// practice; generic parameter lists occasionally do).
pub(crate) fn node_text(node: Node<'_>, src: &str) -> String {
    src[node.byte_range()]
        .split_whitespace()
        .collect::<String>()
}

/// `node.child_by_field_name("name")`, the common case.
pub(crate) fn field_name(node: Node<'_>, src: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(n, src))
        .filter(|n| !n.is_empty())
}

/// First descendant of `kind` among the node's named children.
pub(crate) fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}
