//! Rust spec.
//!
//! `impl` and `trait` blocks are containers (their members are worth
//! separate chunks); `struct`/`enum` stay whole because per-field chunks
//! are noise. Doc comments and `#[attr]` items are preceding siblings, which
//! the shared attachment rule already handles.

use tree_sitter::Node;

use super::{LanguageSource, NameOf, Spec, field_name, node_text};

pub(crate) fn spec() -> Spec<'static> {
    Spec {
        language: LanguageSource::Native(|| tree_sitter_rust::LANGUAGE.into()),
        tag: "rust",
        path_only: &["mod_item"],
        containers: &["impl_item", "trait_item"],
        symbols: &[
            "function_item",
            "function_signature_item",
            "struct_item",
            "enum_item",
            "union_item",
            "type_item",
            "const_item",
            "static_item",
            "macro_definition",
            "associated_type",
            "foreign_mod_item",
        ],
        wrappers: &[],
        bodies: &["declaration_list"],
        attachments: &[
            "line_comment",
            "block_comment",
            "attribute_item",
            "inner_attribute_item",
        ],
        trailing_scope: &[],
        name_of: NameOf::Fn(name_of),
    }
}

fn name_of(node: Node<'_>, src: &str) -> Option<String> {
    if node.kind() == "impl_item" {
        // `impl Display for Index` → `Index#Display`, so inherent and trait
        // impls of the same type get distinct symbol paths.
        let ty = node
            .child_by_field_name("type")
            .map(|n| node_text(n, src))?;
        return Some(match node.child_by_field_name("trait") {
            Some(tr) => format!("{ty}#{}", node_text(tr, src)),
            None => ty,
        });
    }
    field_name(node, src)
}
