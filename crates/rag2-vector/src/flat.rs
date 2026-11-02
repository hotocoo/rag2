//! Exact cosine search over a contiguous row-major matrix. The recall reference for every other
//! index, and the right choice for small tenants where a scan beats graph traversal.

use crate::simd::{dot, normalized};
use crate::topk::TopK;
use parking_lot::RwLock;
use rag2_core::interfaces::VectorIndex;
use rag2_core::model::Hit;
use rag2_core::{Error, Result};
use rayon::prelude::*;
use std::collections::HashMap;

/// Tuning for [`FlatIndex`], set from the active profile.
#[derive(Debug, Clone, Copy)]
pub struct FlatConfig {
    /// Rows per rayon task when a scan is split across threads. Scans of fewer rows than this run
    /// on the calling thread (no task overhead on small tenants).
    pub parallel_chunk_rows: usize,
}

struct Rows {
    /// `ids.len() * dim` floats, unit-normalized, row `i` belongs to `ids[i]`.
    data: Vec<f32>,
    ids: Vec<u64>,
    slot: HashMap<u64, usize>,
}

pub struct FlatIndex {
    dim: usize,
    cfg: FlatConfig,
    rows: RwLock<Rows>,
}

impl FlatIndex {
    pub fn new(dim: usize, cfg: FlatConfig) -> Result<Self> {
        if dim == 0 || cfg.parallel_chunk_rows == 0 {
            return Err(Error::InvalidInput(
                "flat index needs dim > 0 and parallel_chunk_rows > 0".into(),
            ));
        }
        Ok(FlatIndex {
            dim,
            cfg,
            rows: RwLock::new(Rows {
                data: Vec::new(),
                ids: Vec::new(),
                slot: HashMap::new(),
            }),
        })
    }

    fn check_dim(&self, v: &[f32]) -> Result<()> {
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
}

impl VectorIndex for FlatIndex {
    fn kind(&self) -> &'static str {
        "flat"
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn len(&self) -> usize {
        self.rows.read().ids.len()
    }

    fn upsert(&self, id: u64, vector: &[f32]) -> Result<()> {
        self.check_dim(vector)?;
        let v = normalized(vector);
        let mut r = self.rows.write();
        match r.slot.get(&id).copied() {
            Some(i) => r.data[i * self.dim..(i + 1) * self.dim].copy_from_slice(&v),
            None => {
                let i = r.ids.len();
                r.data.extend_from_slice(&v);
                r.ids.push(id);
                r.slot.insert(id, i);
            }
        }
        Ok(())
    }

    fn remove(&self, id: u64) -> Result<bool> {
        let mut r = self.rows.write();
        let Some(i) = r.slot.remove(&id) else {
            return Ok(false);
        };
        // Swap the last row into the hole so the matrix stays dense.
        let last = r.ids.len() - 1;
        if i != last {
            let d = self.dim;
            r.data.copy_within(last * d..(last + 1) * d, i * d);
            let moved = r.ids[last];
            r.ids[i] = moved;
            r.slot.insert(moved, i);
        }
        r.ids.truncate(last);
        r.data.truncate(last * self.dim);
        Ok(true)
    }

    fn search(&self, query: &[f32], k: usize, _ef: Option<usize>) -> Result<Vec<Hit>> {
        self.check_dim(query)?;
        let q = normalized(query);
        let r = self.rows.read();
        let d = self.dim;
        let scan = |rows: &[f32], ids: &[u64]| {
            let mut t = TopK::new(k);
            for (row, &id) in rows.chunks_exact(d).zip(ids) {
                t.push(id, dot(row, &q));
            }
            t
        };
        let chunk = self.cfg.parallel_chunk_rows;
        let top = if r.ids.len() <= chunk {
            scan(&r.data, &r.ids)
        } else {
            r.data
                .par_chunks(chunk * d)
                .zip(r.ids.par_chunks(chunk))
                .map(|(rows, ids)| scan(rows, ids))
                .reduce(|| TopK::new(k), TopK::merge)
        };
        Ok(top.into_hits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx() -> FlatIndex {
        FlatIndex::new(
            3,
            FlatConfig {
                parallel_chunk_rows: 2,
            },
        )
        .expect("index")
    }

    #[test]
    fn finds_the_nearest_by_cosine_and_ignores_magnitude() {
        let ix = idx();
        ix.upsert(1, &[1.0, 0.0, 0.0]).expect("upsert");
        ix.upsert(2, &[0.0, 1.0, 0.0]).expect("upsert");
        ix.upsert(3, &[10.0, 1.0, 0.0]).expect("upsert");
        let hits = ix.search(&[1.0, 0.01, 0.0], 2, None).expect("search");
        assert_eq!(
            hits.iter().map(|h| h.chunk.0).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn upsert_replaces_and_remove_keeps_the_matrix_dense() {
        let ix = idx();
        for id in 1..=5 {
            ix.upsert(id, &[id as f32, 1.0, 0.0]).expect("upsert");
        }
        ix.upsert(2, &[0.0, 0.0, 1.0]).expect("replace");
        assert_eq!(ix.len(), 5);
        assert!(ix.remove(1).expect("remove"));
        assert!(!ix.remove(1).expect("remove twice"));
        assert_eq!(ix.len(), 4);
        let top = ix.search(&[0.0, 0.0, 1.0], 1, None).expect("search");
        assert_eq!(top[0].chunk.0, 2);
        // Row 5 was swapped into the hole; it must still be found.
        let five = ix.search(&[5.0, 1.0, 0.0], 1, None).expect("search");
        assert_eq!(five[0].chunk.0, 5);
    }

    #[test]
    fn refuses_wrong_dimensions_and_non_finite_values() {
        let ix = idx();
        assert!(ix.upsert(1, &[1.0, 2.0]).is_err());
        assert!(ix.upsert(1, &[f32::NAN, 0.0, 0.0]).is_err());
        assert!(ix.search(&[1.0], 1, None).is_err());
    }
}
