//! Deterministic synthetic corpora and exact ground truth.

use rag2_vector::simd::{dot, normalized};
use rag2_vector::topk::TopK;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use rayon::prelude::*;

/// A standard normal sample (Box-Muller), avoiding a distributions dependency.
fn gauss(r: &mut StdRng) -> f32 {
    let u1: f32 = r.random_range(f32::EPSILON..1.0);
    let u2: f32 = r.random_range(0.0..1.0);
    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

/// Corpus vectors keyed by id, and held-out query vectors.
pub type Dataset = (Vec<(u64, Vec<f32>)>, Vec<Vec<f32>>);

/// `n` corpus vectors and `q` held-out queries drawn from the same Gaussian mixture.
pub fn clustered(
    n: usize,
    q: usize,
    dim: usize,
    clusters: usize,
    noise: f32,
    seed: u64,
) -> Dataset {
    let mut r = StdRng::seed_from_u64(seed);
    let centres: Vec<Vec<f32>> = (0..clusters.max(1))
        .map(|_| normalized(&(0..dim).map(|_| gauss(&mut r)).collect::<Vec<_>>()))
        .collect();
    let scale = noise / (dim as f32).sqrt();
    let sample = |r: &mut StdRng| {
        let c = &centres[r.random_range(0..centres.len())];
        c.iter().map(|x| x + scale * gauss(r)).collect::<Vec<f32>>()
    };
    let corpus = (0..n as u64).map(|i| (i, sample(&mut r))).collect();
    let queries = (0..q).map(|_| sample(&mut r)).collect();
    (corpus, queries)
}

/// Exact cosine top-k per query.
pub fn exact_top_k(corpus: &[(u64, Vec<f32>)], queries: &[Vec<f32>], k: usize) -> Vec<Vec<u64>> {
    let unit: Vec<(u64, Vec<f32>)> = corpus
        .par_iter()
        .map(|(i, v)| (*i, normalized(v)))
        .collect();
    queries
        .par_iter()
        .map(|q| {
            let q = normalized(q);
            let mut t = TopK::new(k);
            for (id, v) in &unit {
                t.push(*id, dot(v, &q));
            }
            t.into_hits().into_iter().map(|h| h.chunk.0).collect()
        })
        .collect()
}
