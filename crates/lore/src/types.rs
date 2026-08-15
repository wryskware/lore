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

/// What kind of corpus a source (a row in `projects`) is.
///
/// `repo` is everything v1 indexes. `session` is the tier-2 session ledger
/// (D-0006/D-0008), whose writer lands at M3 — the field exists now so the
/// schema, the store filter seam and the ranking cap can be expressed without
/// another migration. `issue` is expected later; unknown spellings read back
/// as `Repo` rather than failing an index pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Repo,
    Session,
}

impl SourceKind {
    /// Canonical on-disk / on-the-wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Repo => "repo",
            SourceKind::Session => "session",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "repo" => SourceKind::Repo,
            "session" => SourceKind::Session,
            _ => return None,
        })
    }
}

/// **Declared** ranking authority tier (higher = more authoritative), derived
/// from frontmatter alone. Ordering is the canon requirement (3.1):
/// decided > leaning > exploration/unclassified > deprecated. Exact weights
/// derived from tiers are tuning.
///
/// This is what a document *claims*. What ranking actually uses is
/// [`crate::authority::effective`], which validates the claim against the
/// project's ledger and the file's path.
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

/// Structural identity of a chunk within its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChunkKind {
    /// A code symbol (tree-sitter). `symbol_path` is the dotted path of
    /// enclosing named nodes, e.g. `Lexomancy.Board.Update`.
    Code {
        symbol_kind: String,
        symbol_path: String,
    },
    /// A Markdown heading-tree leaf; `heading_path` is root-to-leaf titles.
    Section { heading_path: Vec<String> },
    /// A fallback line window over unknown text; `index` is the 0-based
    /// window ordinal within the file.
    Window { index: u32 },
}

impl ChunkKind {
    /// Canonical anchor string used in [`ChunkId::derive`].
    pub fn anchor(&self) -> String {
        match self {
            ChunkKind::Code { symbol_path, .. } => format!("code:{symbol_path}"),
            ChunkKind::Section { heading_path } => {
                format!("md:{}", heading_path.join(" > "))
            }
            ChunkKind::Window { index } => format!("win:{index}"),
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
        };
        let a = Chunk::derive_id(&path, &kind, "fn foo() {}");
        let b = Chunk::derive_id(&path, &kind, "fn foo() {}");
        let c = Chunk::derive_id(&path, &kind, "fn foo() { 1; }");
        assert_eq!(a, b);
        assert_ne!(a, c);
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
