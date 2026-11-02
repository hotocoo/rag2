//! A tenant's vector indexes, one per embedder. Small tenants get the exact flat index; crossing
//! `flat_below` (or accumulating too many HNSW tombstones) rebuilds an HNSW index from storage and
//! swaps it in atomically.

use parking_lot::RwLock;
use rag2_config::schema::VectorIndexConfig;
use rag2_core::interfaces::VectorIndex;
use rag2_core::{ChunkId, Result, TenantId};
use rag2_storage::Storage;
use rag2_vector::{FlatConfig, FlatIndex, HnswConfig, HnswIndex};
use std::collections::HashMap;
use std::sync::Arc;

/// Vectors read from storage per page while (re)building.
const REBUILD_PAGE: usize = 4_096;

pub fn hnsw_config(c: &VectorIndexConfig, capacity: usize) -> HnswConfig {
    HnswConfig {
        m: c.hnsw_m,
        ef_construction: c.hnsw_ef_construction,
        ef_search: c.hnsw_ef_search,
        capacity_hint: capacity,
        max_tombstone_ratio: c.hnsw_max_tombstone_ratio,
    }
}

/// Build the right index for `n` vectors and fill it from storage.
pub fn build_from_storage(
    storage: &Storage,
    t: &TenantId,
    embedder: &str,
    dim: usize,
    cfg: &VectorIndexConfig,
) -> Result<Arc<dyn VectorIndex>> {
    let mut all: Vec<(u64, Vec<f32>)> = Vec::new();
    let mut after: Option<ChunkId> = None;
    loop {
        let page = storage.scan_vectors(t, embedder, after, REBUILD_PAGE)?;
        let Some(last) = page.last() else { break };
        after = Some(last.0);
        all.extend(
            page.into_iter()
                .filter(|(_, v)| v.len() == dim)
                .map(|(id, v)| (id.0, v)),
        );
    }
    if all.len() < cfg.flat_below {
        let ix = FlatIndex::new(
            dim,
            FlatConfig {
                parallel_chunk_rows: cfg.flat_parallel_chunk_rows,
            },
        )?;
        for (id, v) in &all {
            ix.upsert(*id, v)?;
        }
        Ok(Arc::new(ix))
    } else {
        let ix = HnswIndex::new(dim, hnsw_config(cfg, all.len()))?;
        ix.insert_batch(&all)?;
        Ok(Arc::new(ix))
    }
}

#[derive(Default)]
pub struct TenantVectors {
    by_embedder: RwLock<HashMap<String, Arc<dyn VectorIndex>>>,
}

impl TenantVectors {
    pub fn get(&self, embedder: &str) -> Option<Arc<dyn VectorIndex>> {
        self.by_embedder.read().get(embedder).cloned()
    }

    pub fn set(&self, embedder: &str, ix: Arc<dyn VectorIndex>) {
        self.by_embedder.write().insert(embedder.to_string(), ix);
    }

    pub fn embedders(&self) -> Vec<String> {
        self.by_embedder.read().keys().cloned().collect()
    }

    /// Whether `ix` should be rebuilt under `cfg`: a flat index that outgrew `flat_below`, or an
    /// index reporting degradation.
    pub fn needs_rebuild(ix: &dyn VectorIndex, cfg: &VectorIndexConfig) -> bool {
        (ix.kind() == "flat" && ix.len() >= cfg.flat_below) || ix.needs_rebuild()
    }
}
