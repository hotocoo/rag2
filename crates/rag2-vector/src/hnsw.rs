//! HNSW over `hnsw_rs`. `hnsw_rs` has no delete, so removals and replacements tombstone the old
//! graph node and searches filter tombstones out. When tombstones pass the configured ratio,
//! [`HnswIndex::needs_rebuild`] says so and the owner rebuilds from stored vectors.

use crate::simd::{dot, normalized};
use hnsw_rs::prelude::{Distance, FilterT, Hnsw};
use parking_lot::RwLock;
use rag2_core::ChunkId;
use rag2_core::interfaces::VectorIndex;
use rag2_core::model::Hit;
use rag2_core::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Upper bound `hnsw_rs` accepts for `m` (it aborts the process above it, so we refuse first).
const HNSW_RS_MAX_M: usize = 256;
/// Graph layers; `hnsw_rs` caps this at its own `NB_LAYER_MAX` (16).
const HNSW_LAYERS: usize = 16;

/// Tuning for [`HnswIndex`], set from the active profile.
#[derive(Debug, Clone, Copy)]
pub struct HnswConfig {
    /// Neighbours per node per layer.
    pub m: usize,
    /// Candidate list size while building.
    pub ef_construction: usize,
    /// Default candidate list size while searching (a query may override it).
    pub ef_search: usize,
    /// Allocation hint: expected number of vectors.
    pub capacity_hint: usize,
    /// Tombstoned share of graph nodes above which a rebuild is due.
    pub max_tombstone_ratio: f32,
}

#[derive(Default)]
struct Ids {
    to_node: HashMap<u64, usize>,
    to_id: HashMap<usize, u64>,
    dead: HashSet<usize>,
}

/// `1 - cos` for unit vectors, clamped: `anndists::DistDot` asserts `dot <= 1` and float rounding
/// on a self-match breaks that, which would abort the process under `panic = "abort"`.
#[derive(Clone, Copy, Default)]
pub struct UnitCosine;

impl Distance<f32> for UnitCosine {
    fn eval(&self, a: &[f32], b: &[f32]) -> f32 {
        (1.0 - dot(a, b)).clamp(0.0, 2.0)
    }
}

struct Live<'a>(&'a HashSet<usize>);

impl FilterT for Live<'_> {
    fn hnsw_filter(&self, node: &usize) -> bool {
        !self.0.contains(node)
    }
}

pub struct HnswIndex {
    dim: usize,
    cfg: HnswConfig,
    graph: Hnsw<'static, f32, UnitCosine>,
    ids: RwLock<Ids>,
    next: AtomicUsize,
}

impl HnswIndex {
    pub fn new(dim: usize, cfg: HnswConfig) -> Result<Self> {
        if dim == 0
            || cfg.m < 2
            || cfg.m > HNSW_RS_MAX_M
            || cfg.ef_construction == 0
            || cfg.ef_search == 0
            || !(0.0..=1.0).contains(&cfg.max_tombstone_ratio)
        {
            return Err(Error::InvalidInput(format!(
                "hnsw needs dim > 0, 2 <= m <= {HNSW_RS_MAX_M}, ef > 0, tombstone ratio in [0, 1]"
            )));
        }
        let graph = Hnsw::new(
            cfg.m,
            cfg.capacity_hint.max(1),
            HNSW_LAYERS,
            cfg.ef_construction,
            UnitCosine,
        );
        Ok(HnswIndex {
            dim,
            cfg,
            graph,
            ids: RwLock::new(Ids::default()),
            next: AtomicUsize::new(0),
        })
    }

    /// True when enough nodes are tombstoned that a rebuild would pay for itself.
    pub fn tombstones_due(&self) -> bool {
        let ids = self.ids.read();
        let total = ids.to_id.len() + ids.dead.len();
        total > 0 && ids.dead.len() as f32 / total as f32 > self.cfg.max_tombstone_ratio
    }

    fn check(&self, v: &[f32]) -> Result<()> {
        if v.len() != self.dim {
            return Err(Error::InvalidInput(format!(
                "vector has {} dimensions, index has {}",
                v.len(),
                self.dim
            )));
        }
        if v.iter().any(|x| !x.is_finite()) {
            return Err(Error::InvalidInput(
                "vector contains NaN or infinity".into(),
            ));
        }
        Ok(())
    }

    /// Insert many at once across rayon threads (initial build / rebuild).
    pub fn insert_batch(&self, items: &[(u64, Vec<f32>)]) -> Result<()> {
        for (_, v) in items {
            self.check(v)?;
        }
        let prepared: Vec<(usize, Vec<f32>)> = {
            let mut ids = self.ids.write();
            items
                .iter()
                .map(|(id, v)| {
                    let node = self.next.fetch_add(1, Ordering::Relaxed);
                    if let Some(old) = ids.to_node.insert(*id, node) {
                        ids.to_id.remove(&old);
                        ids.dead.insert(old);
                    }
                    ids.to_id.insert(node, *id);
                    (node, normalized(v))
                })
                .collect()
        };
        let refs: Vec<(&[f32], usize)> = prepared.iter().map(|(n, v)| (v.as_slice(), *n)).collect();
        self.graph.parallel_insert_slice(&refs);
        Ok(())
    }
}

impl VectorIndex for HnswIndex {
    fn kind(&self) -> &'static str {
        "hnsw"
    }

    fn needs_rebuild(&self) -> bool {
        self.tombstones_due()
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn len(&self) -> usize {
        self.ids.read().to_node.len()
    }

    fn upsert(&self, id: u64, vector: &[f32]) -> Result<()> {
        self.check(vector)?;
        let v = normalized(vector);
        let node = self.next.fetch_add(1, Ordering::Relaxed);
        // Insert into the graph first, then publish: a search never sees an id whose node is
        // missing, and the old node keeps answering until the new one is in.
        self.graph.insert_slice((&v, node));
        let mut ids = self.ids.write();
        if let Some(old) = ids.to_node.insert(id, node) {
            ids.to_id.remove(&old);
            ids.dead.insert(old);
        }
        ids.to_id.insert(node, id);
        Ok(())
    }

    fn remove(&self, id: u64) -> Result<bool> {
        let mut ids = self.ids.write();
        let Some(node) = ids.to_node.remove(&id) else {
            return Ok(false);
        };
        ids.to_id.remove(&node);
        ids.dead.insert(node);
        Ok(true)
    }

    fn search(&self, query: &[f32], k: usize, ef: Option<usize>) -> Result<Vec<Hit>> {
        self.check(query)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        let q = normalized(query);
        let ef = ef.unwrap_or(self.cfg.ef_search).max(k);
        let ids = self.ids.read();
        let live = Live(&ids.dead);
        let found = if ids.dead.is_empty() {
            self.graph.search(&q, k, ef)
        } else {
            self.graph.search_filter(&q, k, ef, Some(&live))
        };
        Ok(found
            .into_iter()
            .filter_map(|n| {
                // Nodes inserted but not yet published have no id: skip them.
                ids.to_id.get(&n.d_id).map(|&id| Hit {
                    chunk: ChunkId(id),
                    score: 1.0 - n.distance,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngExt, SeedableRng, rngs::StdRng};

    fn cfg() -> HnswConfig {
        HnswConfig {
            m: 16,
            ef_construction: 100,
            ef_search: 64,
            capacity_hint: 1000,
            max_tombstone_ratio: 0.2,
        }
    }

    fn vecs(n: usize, dim: usize) -> Vec<(u64, Vec<f32>)> {
        let mut r = StdRng::seed_from_u64(7);
        (0..n as u64)
            .map(|i| (i, (0..dim).map(|_| r.random_range(-1.0..1.0)).collect()))
            .collect()
    }

    #[test]
    fn every_vector_finds_itself_first() {
        let ix = HnswIndex::new(16, cfg()).expect("index");
        let data = vecs(500, 16);
        ix.insert_batch(&data).expect("batch");
        let found = data
            .iter()
            .filter(|(id, v)| ix.search(v, 1, None).expect("search")[0].chunk.0 == *id)
            .count();
        assert!(found >= 495, "self-recall {found}/500");
        assert!((ix.search(&data[0].1, 1, None).expect("search")[0].score - 1.0).abs() < 1e-4);
    }

    #[test]
    fn removed_and_replaced_vectors_are_never_returned() {
        let ix = HnswIndex::new(16, cfg()).expect("index");
        let data = vecs(200, 16);
        ix.insert_batch(&data).expect("batch");
        assert!(ix.remove(5).expect("remove"));
        assert!(
            ix.search(&data[5].1, 10, None)
                .expect("search")
                .iter()
                .all(|h| h.chunk.0 != 5)
        );
        ix.upsert(6, &data[7].1).expect("replace");
        let hits = ix.search(&data[6].1, 200, Some(400)).expect("search");
        assert_eq!(hits.iter().filter(|h| h.chunk.0 == 6).count(), 1);
        assert_eq!(ix.len(), 199);
    }

    #[test]
    fn rebuild_is_due_past_the_tombstone_ratio() {
        let ix = HnswIndex::new(16, cfg()).expect("index");
        ix.insert_batch(&vecs(10, 16)).expect("batch");
        ix.remove(0).expect("remove");
        ix.remove(1).expect("remove");
        assert!(!ix.needs_rebuild());
        ix.remove(2).expect("remove");
        assert!(ix.needs_rebuild());
    }

    #[test]
    fn invalid_parameters_are_refused_instead_of_aborting() {
        assert!(HnswIndex::new(16, HnswConfig { m: 300, ..cfg() }).is_err());
        assert!(HnswIndex::new(0, cfg()).is_err());
    }
}
