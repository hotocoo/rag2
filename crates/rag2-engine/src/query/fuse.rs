//! Rank fusion across retrievers (and hops).

use rag2_config::schema::{Fusion, FusionMethod};
use rag2_core::ChunkId;
use rag2_core::model::{Hit, RetrieverKind, Signal};
use std::collections::HashMap;

/// One retriever's ranked list.
pub struct RankedList {
    pub retriever: RetrieverKind,
    pub hop: u32,
    pub weight: f32,
    pub hits: Vec<Hit>,
}

#[derive(Debug, Clone)]
pub struct Fused {
    pub chunk: ChunkId,
    pub score: f32,
    pub signals: Vec<Signal>,
}

/// Fuse lists into one ranking, best first; ties break on chunk id for determinism.
pub fn fuse(lists: &[RankedList], f: &Fusion) -> Vec<Fused> {
    let mut acc: HashMap<ChunkId, Fused> = HashMap::new();
    for l in lists {
        let (lo, hi) = l
            .hits
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), h| {
                (lo.min(h.score), hi.max(h.score))
            });
        for (i, h) in l.hits.iter().enumerate() {
            let rank = i as u32 + 1;
            let contribution = match f.method {
                FusionMethod::Rrf => l.weight / (f.rrf_k + rank as f32),
                FusionMethod::Weighted => {
                    let norm = if hi > lo {
                        (h.score - lo) / (hi - lo)
                    } else {
                        1.0
                    };
                    l.weight * norm
                }
            };
            let e = acc.entry(h.chunk).or_insert_with(|| Fused {
                chunk: h.chunk,
                score: 0.0,
                signals: Vec::new(),
            });
            e.score += contribution;
            e.signals.push(Signal {
                retriever: l.retriever,
                rank,
                score: h.score,
                hop: l.hop,
            });
        }
    }
    let mut out: Vec<Fused> = acc.into_values().collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.cmp(&b.chunk)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(r: RetrieverKind, ids: &[u64], weight: f32) -> RankedList {
        RankedList {
            retriever: r,
            hop: 0,
            weight,
            hits: ids
                .iter()
                .enumerate()
                .map(|(i, id)| Hit {
                    chunk: ChunkId(*id),
                    score: 10.0 - i as f32,
                })
                .collect(),
        }
    }

    fn cfg(method: FusionMethod) -> Fusion {
        Fusion {
            method,
            rrf_k: 60.0,
            candidate_pool_factor: 3,
        }
    }

    #[test]
    fn rrf_rewards_agreement_between_retrievers() {
        let lists = [
            list(RetrieverKind::Dense, &[1, 2, 3], 1.0),
            list(RetrieverKind::Lexical, &[3, 4, 1], 1.0),
        ];
        let f = fuse(&lists, &cfg(FusionMethod::Rrf));
        assert_eq!(f[0].chunk, ChunkId(1));
        assert_eq!(f[1].chunk, ChunkId(3));
        assert_eq!(f[0].signals.len(), 2);
    }

    #[test]
    fn weights_shift_the_ranking() {
        let lists = [
            list(RetrieverKind::Dense, &[1], 1.0),
            list(RetrieverKind::Lexical, &[2], 3.0),
        ];
        assert_eq!(fuse(&lists, &cfg(FusionMethod::Rrf))[0].chunk, ChunkId(2));
        assert_eq!(
            fuse(&lists, &cfg(FusionMethod::Weighted))[0].chunk,
            ChunkId(2)
        );
    }

    #[test]
    fn weighted_fusion_normalizes_each_list() {
        let a = RankedList {
            retriever: RetrieverKind::Dense,
            hop: 0,
            weight: 1.0,
            hits: vec![
                Hit {
                    chunk: ChunkId(1),
                    score: 0.9,
                },
                Hit {
                    chunk: ChunkId(2),
                    score: 0.1,
                },
            ],
        };
        let b = RankedList {
            retriever: RetrieverKind::Lexical,
            hop: 0,
            weight: 1.0,
            hits: vec![
                Hit {
                    chunk: ChunkId(2),
                    score: 40.0,
                },
                Hit {
                    chunk: ChunkId(1),
                    score: 39.0,
                },
            ],
        };
        let f = fuse(&[a, b], &cfg(FusionMethod::Weighted));
        // chunk 1: 1.0 + 0.0, chunk 2: 0.0 + 1.0 -> tie broken by id.
        assert_eq!(f[0].chunk, ChunkId(1));
        assert!((f[0].score - f[1].score).abs() < 1e-6);
    }
}
