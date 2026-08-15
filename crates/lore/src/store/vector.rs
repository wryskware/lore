//! Vector encoding and brute-force scoring helpers.
//!
//! Vectors are stored as `dims * 4` bytes of little-endian `f32`,
//! **L2-normalized on write**. Because both stored vectors and query vectors
//! are unit length, cosine similarity == dot product, so scoring is a plain
//! dot product with no per-row norm.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// The L2 norm, or `None` for a vector that cannot produce a meaningful
/// cosine.
///
/// This is the **single** definition of "usable vector". The embed worker
/// screens vectors before handing them to [`super::Store::upsert_embeddings`],
/// which rejects an unusable one for the whole transaction; a second predicate
/// that disagreed by even one epsilon would let a batch abort forever.
fn usable_norm(v: &[f32]) -> Option<f32> {
    if v.is_empty() || v.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    (norm.is_finite() && norm > f32::EPSILON).then_some(norm)
}

/// Would [`normalize`] — and therefore a store write — accept this vector?
pub(crate) fn is_usable(v: &[f32]) -> bool {
    usable_norm(v).is_some()
}

/// L2-normalize, rejecting vectors that cannot produce a meaningful cosine.
pub(crate) fn normalize(v: &[f32]) -> Option<Vec<f32>> {
    let norm = usable_norm(v)?;
    Some(v.iter().map(|x| x / norm).collect())
}

pub(crate) fn encode(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Dot product of a query vector against a raw stored blob, without
/// allocating a decoded copy per row. `None` on a dimension mismatch.
pub(crate) fn dot_blob(query: &[f32], blob: &[u8]) -> Option<f32> {
    if blob.len() != query.len() * 4 {
        return None;
    }
    let (words, _rest) = blob.as_chunks::<4>();
    let acc = query
        .iter()
        .zip(words)
        .map(|(q, b)| q * f32::from_le_bytes(*b))
        .sum();
    Some(acc)
}

/// A scored row id. `Ord` uses `total_cmp` so NaN cannot panic the heap;
/// ties break on rowid so results are deterministic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scored {
    pub(crate) score: f32,
    pub(crate) rowid: i64,
}

impl Eq for Scored {}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.rowid.cmp(&self.rowid))
    }
}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Bounded min-heap: keeps the `k` highest-scoring entries pushed so far, so
/// a full-corpus scan costs O(n log k) time and O(k) memory rather than
/// sorting every row.
pub(crate) struct TopK {
    k: usize,
    heap: BinaryHeap<std::cmp::Reverse<Scored>>,
}

impl TopK {
    pub(crate) fn new(k: usize) -> Self {
        Self {
            k,
            heap: BinaryHeap::with_capacity(k.saturating_add(1).min(1024)),
        }
    }

    pub(crate) fn push(&mut self, item: Scored) {
        if self.k == 0 {
            return;
        }
        self.heap.push(std::cmp::Reverse(item));
        if self.heap.len() > self.k {
            self.heap.pop();
        }
    }

    /// Best first.
    pub(crate) fn into_sorted(self) -> Vec<Scored> {
        let mut v: Vec<Scored> = self.heap.into_iter().map(|r| r.0).collect();
        v.sort_by(|a, b| b.cmp(a));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_makes_unit_vectors() {
        let n = normalize(&[3.0, 4.0]).unwrap();
        assert!((n[0] - 0.6).abs() < 1e-6);
        assert!((n[1] - 0.8).abs() < 1e-6);
        assert!(normalize(&[0.0, 0.0]).is_none());
        assert!(normalize(&[f32::NAN, 1.0]).is_none());
        assert!(normalize(&[]).is_none());
    }

    /// The screening predicate and the write path must agree on every input,
    /// including the ones straddling the epsilon threshold.
    #[test]
    fn is_usable_agrees_with_normalize_at_the_threshold() {
        let below = f32::EPSILON / 2.0;
        let at = f32::EPSILON;
        let above = f32::EPSILON * 2.0;
        let cases: [(&str, Vec<f32>); 9] = [
            ("empty", vec![]),
            ("zero", vec![0.0, 0.0]),
            ("nan", vec![f32::NAN, 1.0]),
            ("inf", vec![f32::INFINITY]),
            ("below epsilon", vec![below, 0.0]),
            ("at epsilon", vec![at, 0.0]),
            ("above epsilon", vec![above, 0.0]),
            ("tiny denormal", vec![1e-10, 0.0]),
            ("unit", vec![1.0, 0.0]),
        ];
        for (label, v) in cases {
            assert_eq!(
                is_usable(&v),
                normalize(&v).is_some(),
                "screening and write disagree on {label}"
            );
        }
        assert!(!is_usable(&[below, 0.0]));
        assert!(!is_usable(&[at, 0.0]), "the store's bound is exclusive");
        assert!(is_usable(&[above, 0.0]));
    }

    #[test]
    fn encode_decode_roundtrips_through_dot() {
        let v = normalize(&[1.0, 0.0, 0.0]).unwrap();
        let blob = encode(&v);
        assert_eq!(blob.len(), 12);
        assert_eq!(dot_blob(&v, &blob), Some(1.0));
        assert_eq!(dot_blob(&[1.0, 0.0], &blob), None);
    }

    #[test]
    fn topk_keeps_best_in_order() {
        let mut top = TopK::new(2);
        for (i, s) in [0.1f32, 0.9, 0.5, 0.7].iter().enumerate() {
            top.push(Scored {
                score: *s,
                rowid: i as i64,
            });
        }
        let out = top.into_sorted();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rowid, 1);
        assert_eq!(out[1].rowid, 3);
    }
}
