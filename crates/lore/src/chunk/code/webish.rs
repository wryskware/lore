//! JavaScript / TypeScript / TSX spec.
//!
//! The three grammars share a node vocabulary; the kind lists below are a
//! superset (TS-only kinds are simply absent from JS trees), so one spec
//! body serves all three with a different grammar and language tag.

use tree_sitter::Node;

use super::{LanguageSource, NameOf, Spec, child_of_kind, field_name, node_text};

pub(crate) fn javascript() -> Spec<'static> {
    make(|| tree_sitter_javascript::LANGUAGE.into(), "javascript")
}

pub(crate) fn typescript() -> Spec<'static> {
    make(
        || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "typescript",
    )
}

/// TSX parses `.tsx`; the language tag stays `typescript`.
pub(crate) fn tsx() -> Spec<'static> {
    make(|| tree_sitter_typescript::LANGUAGE_TSX.into(), "typescript")
}

fn make(language: fn() -> tree_sitter::Language, tag: &'static str) -> Spec<'static> {
    Spec {
        language: LanguageSource::Native(language),
        tag,
        path_only: &["internal_module", "module"],
        containers: &[
            "class_declaration",
            "abstract_class_declaration",
            "interface_declaration",
        ],
        symbols: &[
            "function_declaration",
            "generator_function_declaration",
            "method_definition",
            "method_signature",
            "abstract_method_signature",
            "public_field_definition",
            "field_definition",
            "property_signature",
            "lexical_declaration",
            "variable_declaration",
            "type_alias_declaration",
            "enum_declaration",
        ],
        wrappers: &["export_statement", "ambient_declaration"],
        bodies: &["class_body", "interface_body", "enum_body", "object_type"],
        attachments: &["comment", "decorator"],
        trailing_scope: &[],
        name_of: NameOf::Fn(name_of),
    }
}

fn name_of(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        // `export const summarize = (hits) => ...`
        "lexical_declaration" | "variable_declaration" => {
            let declarator = child_of_kind(node, "variable_declarator")?;
            field_name(declarator, src)
                .or_else(|| child_of_kind(declarator, "identifier").map(|n| node_text(n, src)))
        }
        _ => field_name(node, src),
    }
}
