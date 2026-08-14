//! Python spec. Decorators arrive as a `decorated_definition` wrapper, so
//! the chunk span covers them without any special attachment handling.

use tree_sitter::Node;

use super::{Spec, field_name};

pub(crate) fn spec() -> Spec {
    Spec {
        language: || tree_sitter_python::LANGUAGE.into(),
        tag: "python",
        path_only: &[],
        containers: &["class_definition"],
        symbols: &["function_definition"],
        wrappers: &["decorated_definition"],
        bodies: &["block"],
        attachments: &["comment"],
        trailing_scope: &[],
        name_of: |node: Node<'_>, src: &str| field_name(node, src),
    }
}
