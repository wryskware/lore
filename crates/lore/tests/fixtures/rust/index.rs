//! Toy index used as a chunker fixture.

use std::collections::BTreeMap;
use std::fmt;

/// Maximum documents held in memory before a flush is forced.
pub const FLUSH_THRESHOLD: usize = 512;

/// A document as stored by the toy index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Project-relative path.
    pub path: String,
    /// Raw body text.
    pub body: String,
}

/// In-memory inverted index over [`Document`] bodies.
#[derive(Debug, Default)]
pub struct Index {
    docs: Vec<Document>,
    postings: BTreeMap<String, Vec<usize>>,
    dirty: bool,
}

impl Index {
    /// Creates an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a document, tokenizing its body on ASCII whitespace.
    ///
    /// Returns the assigned document id.
    pub fn insert(&mut self, doc: Document) -> usize {
        let id = self.docs.len();
        for token in doc.body.split_whitespace() {
            let key = token.to_ascii_lowercase();
            self.postings.entry(key).or_default().push(id);
        }
        self.docs.push(doc);
        self.dirty = self.docs.len() >= FLUSH_THRESHOLD;
        id
    }

    /// Returns document ids containing `term`, in insertion order.
    pub fn search(&self, term: &str) -> Vec<usize> {
        let key = term.to_ascii_lowercase();
        match self.postings.get(&key) {
            Some(ids) => {
                let mut out = ids.clone();
                out.dedup();
                out
            }
            None => Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

impl fmt::Display for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Index({} docs, {} terms)", self.docs.len(), self.postings.len())
    }
}

/// Anything that can be folded into an [`Index`].
pub trait Indexable {
    /// Converts `self` into a document.
    fn into_document(self) -> Document;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_search() {
        let mut idx = Index::new();
        idx.insert(Document {
            path: "a.txt".into(),
            body: "alpha beta".into(),
        });
        assert_eq!(idx.search("ALPHA"), vec![0]);
    }
}
