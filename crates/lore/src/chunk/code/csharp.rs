//! C# spec — the flagship target (D-0003).
//!
//! Notes on the grammar (tree-sitter-c-sharp 0.23):
//! * XML doc comments are ordinary `comment` siblings; attributes are
//!   `attribute_list` children of the declaration, so both end up inside the
//!   declaration's span once leading comments are attached.
//! * A `file_scoped_namespace_declaration` does **not** contain the types
//!   that follow it — it is a marker whose scope is the rest of the file,
//!   hence `trailing_scope`.
//! * Top-level statements are `global_statement` wrappers; one of them may
//!   wrap a `local_function_statement`, which is a real symbol.
//! * Preprocessor directives (`#nullable`, `#if`, `#region`) appear as
//!   ordinary siblings and fall through to filler chunks.

use tree_sitter::Node;

use super::{LanguageSource, NameOf, Spec, child_of_kind, field_name, node_text};

pub(crate) fn spec() -> Spec<'static> {
    Spec {
        language: LanguageSource::Native(|| tree_sitter_c_sharp::LANGUAGE.into()),
        tag: "csharp",
        path_only: &["namespace_declaration"],
        containers: &[
            "class_declaration",
            "struct_declaration",
            "record_declaration",
            "record_struct_declaration",
            "interface_declaration",
            "enum_declaration",
        ],
        symbols: &[
            "method_declaration",
            "constructor_declaration",
            "destructor_declaration",
            "property_declaration",
            "indexer_declaration",
            "operator_declaration",
            "conversion_operator_declaration",
            "delegate_declaration",
            "field_declaration",
            "event_field_declaration",
            "event_declaration",
            "enum_member_declaration",
            "local_function_statement",
        ],
        wrappers: &["global_statement"],
        bodies: &["declaration_list", "enum_member_declaration_list"],
        attachments: &["comment", "attribute_list"],
        trailing_scope: &["file_scoped_namespace_declaration"],
        name_of: NameOf::Fn(name_of),
    }
}

fn name_of(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        // `private readonly Dictionary<..> _tiles = new();`
        "field_declaration" | "event_field_declaration" => {
            let declaration = child_of_kind(node, "variable_declaration")?;
            let declarator = child_of_kind(declaration, "variable_declarator")?;
            field_name(declarator, src)
                .or_else(|| child_of_kind(declarator, "identifier").map(|n| node_text(n, src)))
        }
        _ => field_name(node, src),
    }
}
