//! Bounded best-k collector: a min-heap of size k, so a scan costs O(n log k) and never allocates
//! past k entries.

use rag2_core::ChunkId;
use rag2_core::model::Hit;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

#[derive(PartialEq)]
struct Scored(f32, u64);

impl Eq for Scored {}

impl PartialOrd for Scored {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Scored {
    // Ties break on the smaller id so results are deterministic.
    fn cmp(&self, o: &Self) -> Ordering {
        self.0.total_cmp(&o.0).then_with(|| o.1.cmp(&self.1))
    }
}

pub struct TopK {
    k: usize,
    heap: BinaryHeap<Reverse<Scored>>,
}

impl TopK {
    pub fn new(k: usize) -> Self {
        TopK {
            k,
            heap: BinaryHeap::with_capacity(k + 1),
        }
    }

    #[inline]
    pub fn push(&mut self, id: u64, score: f32) {
        if self.k == 0 || score.is_nan() {
            return;
        }
        if self.heap.len() < self.k {
            self.heap.push(Reverse(Scored(score, id)));
        } else if let Some(Reverse(min)) = self.heap.peek()
            && Scored(score, id) > *min
        {
            self.heap.pop();
            self.heap.push(Reverse(Scored(score, id)));
        }
    }

    pub fn merge(mut self, other: TopK) -> TopK {
        for Reverse(Scored(s, id)) in other.heap {
            self.push(id, s);
        }
        self
    }

    /// Best first.
    pub fn into_hits(self) -> Vec<Hit> {
        let mut v: Vec<Scored> = self.heap.into_iter().map(|Reverse(s)| s).collect();
        v.sort_by(|a, b| b.cmp(a));
        v.into_iter()
            .map(|Scored(score, id)| Hit {
                chunk: ChunkId(id),
                score,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_best_k_best_first_with_deterministic_ties() {
        let mut t = TopK::new(3);
        for (id, s) in [
            (1, 0.1),
            (2, 0.9),
            (3, 0.5),
            (4, 0.9),
            (5, f32::NAN),
            (6, 0.7),
        ] {
            t.push(id, s);
        }
        let ids: Vec<u64> = t.into_hits().iter().map(|h| h.chunk.0).collect();
        assert_eq!(ids, vec![2, 4, 6]);
    }

    #[test]
    fn merge_combines_partial_scans() {
        let mut a = TopK::new(2);
        a.push(1, 0.3);
        a.push(2, 0.8);
        let mut b = TopK::new(2);
        b.push(3, 0.9);
        let ids: Vec<u64> = a.merge(b).into_hits().iter().map(|h| h.chunk.0).collect();
        assert_eq!(ids, vec![3, 2]);
    }
}
