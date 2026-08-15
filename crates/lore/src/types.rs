//! Shared data model between the chunking pipeline and the SearchStore.
//!
//! Design refs: D-0004 (schema-aware indexing), 3.1 Chunking_and_Ranking
//! (content-addressed chunk identity; heading-tree provenance).

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Stable, content-addressed chunk identity.
///
/// Derived from (project-relative path, structural anchor, chunk text) so
/// re-parsing an unchanged file yields identical IDs and vectors/FTS rows
/// survive re-index (the CodeGraph counter-churn anti-pattern is the thing
/// this avoids).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkId(pub String);

impl ChunkId {
    pub fn derive(path: &str, anchor: &str, text: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(anchor.as_bytes());
        hasher.update(&[0]);
        hasher.update(text.as_bytes());
        ChunkId(hasher.finalize().to_hex().to_string())
    }
}

/// Vault document lifecycle state from `design_status` frontmatter.
/// Absence (unclassified) is represented as `None` at use sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesignStatus {
    Exploration,
    Leaning,
    Decided,
    Deprecated,
}

/// Ranking authority tier (higher = more authoritative). Ordering is the
/// canon requirement (3.1): decided > leaning > exploration/unclassified
/// > deprecated. Exact weights derived from tiers are tuning.
pub fn authority_tier(status: Option<DesignStatus>) -> u8 {
    match status {
        Some(DesignStatus::Decided) => 3,
        Some(DesignStatus::Leaning) => 2,
        Some(DesignStatus::Exploration) | None => 1,
        Some(DesignStatus::Deprecated) => 0,
    }
}

/// Vault-awareness metadata attached to chunks of vault documents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultMeta {
    /// `design_status` frontmatter of the containing file.
    pub design_status: Option<DesignStatus>,
    /// `decision_refs` frontmatter of the containing file (e.g. "D-0004").
    pub decision_refs: Vec<String>,
    /// D-NNNN references appearing in this chunk's own text.
    pub body_decision_refs: Vec<String>,
}

/// Membership in one generated window family.
///
/// The chunker splits an oversized symbol or section into overlapping line
/// windows. Those windows are *one place in one file*, so ranking collapses
/// them to a single result — but only if it can tell them apart from
/// genuinely distinct content that happens to share an anchor (two C#
/// overloads under `Parser.Parse`, two sibling `## Notes` sections). The
/// `#w<n>` anchor suffix cannot carry that: it is a user-authorable string,
/// and two *independently* windowed overloads both spell `Parse#w0`,
/// `Parse#w1`, … Membership is therefore recorded here, at chunk time, by the
/// only code that knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowFamily {
    /// Per-file ordinal of the split span, assigned in emission order. Two
    /// oversized overloads sharing a symbol path get *different* families.
    pub family: u32,
    /// 0-based window ordinal within the family; the `<n>` of the `#w<n>`
    /// anchor suffix this chunk carries.
    pub index: u32,
}

/// Structural identity of a chunk within its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChunkKind {
    /// A code symbol (tree-sitter). `symbol_path` is the dotted path of
    /// enclosing named nodes, e.g. `Lexomancy.Board.Update`.
    Code {
        symbol_kind: String,
        symbol_path: String,
        /// `Some` exactly when this chunk is one window of an oversized
        /// span. Serde-defaulted and omitted when absent, so rows written
        /// before `CHUNK_FORMAT_VERSION` 4 (which have no such key) read back
        /// as `None` and no SQLite migration is needed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<WindowFamily>,
    },
    /// A Markdown heading-tree leaf; `heading_path` is root-to-leaf titles.
    Section {
        heading_path: Vec<String>,
        /// `Some` exactly when this chunk is one window of an oversized
        /// section; same contract as the `window` field of `Code`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<WindowFamily>,
    },
    /// A fallback line window over unknown text; `index` is the 0-based
    /// window ordinal within the file.
    Window { index: u32 },
}

impl ChunkKind {
    /// Canonical anchor string used in [`ChunkId::derive`].
    ///
    /// Deliberately blind to [`WindowFamily`]: the family is bookkeeping
    /// *about* an anchor, not part of it. Keeping it out means adding the
    /// metadata leaves every derived chunk id — and therefore every stored
    /// embedding and FTS row — exactly where it was.
    pub fn anchor(&self) -> String {
        match self {
            ChunkKind::Code { symbol_path, .. } => format!("code:{symbol_path}"),
            ChunkKind::Section { heading_path, .. } => {
                format!("md:{}", heading_path.join(" > "))
            }
            ChunkKind::Window { index } => format!("win:{index}"),
        }
    }

    /// The window family this chunk belongs to, if it is a generated window.
    pub fn window_family(&self) -> Option<WindowFamily> {
        match self {
            ChunkKind::Code { window, .. } | ChunkKind::Section { window, .. } => *window,
            // A text window's ordinal is *not* a family: consecutive windows
            // of a log file are different content, not one split span.
            ChunkKind::Window { .. } => None,
        }
    }
}

/// One indexable unit of a file. Produced by [`crate::chunk`], persisted and
/// searched by [`crate::store`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: ChunkId,
    /// Project-relative UTF-8 path, forward slashes.
    pub path: Utf8PathBuf,
    pub kind: ChunkKind,
    /// Lowercase language tag ("csharp", "rust", "markdown", …); `None` for
    /// unknown text.
    pub language: Option<String>,
    /// Byte span within the file (half-open).
    pub byte_start: u32,
    pub byte_end: u32,
    /// 1-based line span (inclusive).
    pub line_start: u32,
    pub line_end: u32,
    /// The chunk's text as indexed (verbatim file slice; embedding headers
    /// are constructed at embed time, not stored here).
    pub text: String,
    /// Present only for vault documents (Markdown with recognized schema).
    pub vault: Option<VaultMeta>,
}

impl Chunk {
    /// Uniform ID derivation — all chunkers must construct IDs this way.
    pub fn derive_id(path: &Utf8PathBuf, kind: &ChunkKind, text: &str) -> ChunkId {
        ChunkId::derive(path.as_str(), &kind.anchor(), text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_ids_are_stable_and_distinct() {
        let path = Utf8PathBuf::from("src/lib.rs");
        let kind = ChunkKind::Code {
            symbol_kind: "fn".into(),
            symbol_path: "foo".into(),
            window: None,
        };
        let a = Chunk::derive_id(&path, &kind, "fn foo() {}");
        let b = Chunk::derive_id(&path, &kind, "fn foo() {}");
        let c = Chunk::derive_id(&path, &kind, "fn foo() { 1; }");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    /// The whole point of keeping the family out of [`ChunkKind::anchor`]:
    /// re-chunking to *gain* the metadata must not move a single chunk id,
    /// or the format bump would silently discard every embedding.
    #[test]
    fn window_family_metadata_does_not_move_chunk_ids() {
        let path = Utf8PathBuf::from("Assets/Board.cs");
        let bare = ChunkKind::Code {
            symbol_kind: "method_declaration".into(),
            symbol_path: "Board.Update#w1".into(),
            window: None,
        };
        let tagged = ChunkKind::Code {
            symbol_kind: "method_declaration".into(),
            symbol_path: "Board.Update#w1".into(),
            window: Some(WindowFamily {
                family: 7,
                index: 1,
            }),
        };
        assert_eq!(bare.anchor(), tagged.anchor());
        assert_eq!(
            Chunk::derive_id(&path, &bare, "body"),
            Chunk::derive_id(&path, &tagged, "body")
        );
    }

    /// Rows persisted before `CHUNK_FORMAT_VERSION` 4 carry no `window` key;
    /// they must read back as "not a window" rather than failing the whole
    /// query. A `None` family must also serialize back to the old shape.
    #[test]
    fn kind_json_is_backward_and_forward_compatible() {
        let legacy = r##"{"kind":"section","heading_path":["Ranking","#w0"]}"##;
        let kind: ChunkKind = serde_json::from_str(legacy).expect("legacy row parses");
        assert_eq!(kind.window_family(), None);
        assert_eq!(serde_json::to_string(&kind).unwrap(), legacy);

        let tagged = ChunkKind::Section {
            heading_path: vec!["Ranking".into(), "#w0".into()],
            window: Some(WindowFamily {
                family: 0,
                index: 0,
            }),
        };
        let json = serde_json::to_string(&tagged).unwrap();
        assert_eq!(serde_json::from_str::<ChunkKind>(&json).unwrap(), tagged);
    }

    #[test]
    fn authority_ordering_matches_canon() {
        use DesignStatus::*;
        assert!(authority_tier(Some(Decided)) > authority_tier(Some(Leaning)));
        assert!(authority_tier(Some(Leaning)) > authority_tier(Some(Exploration)));
        assert_eq!(authority_tier(Some(Exploration)), authority_tier(None));
        assert!(authority_tier(None) > authority_tier(Some(Deprecated)));
    }
}
