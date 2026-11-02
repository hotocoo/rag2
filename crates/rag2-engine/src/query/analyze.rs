//! Evidence analysis: near-duplicate collapse and contradiction screening.

use crate::text::{content_words, has_negation, jaccard, numbers_with_context};
use rag2_config::schema::{ContradictionConfig, Dedup};
use rag2_core::model::{Contradiction, Evidence};
use std::collections::HashSet;

pub const NEGATION_DETECTOR: &str = "negation";
pub const NUMERIC_DETECTOR: &str = "numeric";

/// Collapse exact (normalized-hash) and near duplicates into the better-ranked item, keeping its
/// position and merging the dropped item's signals. Input is best first; so is output.
pub fn dedup(items: Vec<(Evidence, u64)>, cfg: &Dedup) -> Vec<Evidence> {
    if !cfg.enabled {
        return items.into_iter().map(|(e, _)| e).collect();
    }
    let mut kept: Vec<(Evidence, u64, HashSet<String>)> = Vec::with_capacity(items.len());
    for (e, hash) in items {
        let words = content_words(&e.text);
        let dup = kept.iter().position(|(_, h, w)| {
            *h == hash || jaccard(w, &words) >= cfg.near_duplicate_similarity
        });
        match dup {
            Some(i) => kept[i].0.signals.extend(e.signals),
            None => kept.push((e, hash, words)),
        }
    }
    kept.into_iter().map(|(e, _, _)| e).collect()
}

/// Pairs of evidence that likely disagree: overlapping content where exactly one side is negated,
/// or the same quantity (number + unit/subject) with values beyond the relative tolerance.
pub fn contradictions(evidence: &[Evidence], cfg: &ContradictionConfig) -> Vec<Contradiction> {
    if !cfg.enabled {
        return Vec::new();
    }
    let prepared: Vec<_> = evidence
        .iter()
        .map(|e| {
            (
                content_words(&e.text),
                has_negation(&e.text),
                numbers_with_context(&e.text),
            )
        })
        .collect();
    let mut out = Vec::new();
    for i in 0..evidence.len() {
        for j in i + 1..evidence.len() {
            let (wa, na, xa) = &prepared[i];
            let (wb, nb, xb) = &prepared[j];
            let overlap = jaccard(wa, wb);
            if overlap < cfg.min_overlap {
                continue;
            }
            let (a, b) = (evidence[i].chunk_id, evidence[j].chunk_id);
            if na != nb {
                out.push(Contradiction {
                    a,
                    b,
                    detector: NEGATION_DETECTOR.into(),
                    reason: "overlapping statements where only one is negated".into(),
                    confidence: overlap,
                });
                continue;
            }
            let conflict = xa.iter().find_map(|(va, ua)| {
                xb.iter()
                    .find(|(vb, ub)| {
                        !ua.is_empty() && ua == ub && {
                            let scale = va.abs().max(vb.abs()).max(f64::MIN_POSITIVE);
                            ((va - vb).abs() / scale) as f32 > cfg.numeric_tolerance
                        }
                    })
                    .map(|(vb, _)| (va, vb, ua))
            });
            if let Some((va, vb, unit)) = conflict {
                out.push(Contradiction {
                    a,
                    b,
                    detector: NUMERIC_DETECTOR.into(),
                    reason: format!("{va} vs {vb} {unit}"),
                    confidence: overlap,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rag2_core::model::Provenance;
    use rag2_core::{ChunkId, DocumentId};

    fn ev(id: u64, text: &str) -> Evidence {
        Evidence {
            chunk_id: ChunkId(id),
            text: text.into(),
            score: 1.0,
            signals: Vec::new(),
            provenance: Provenance {
                doc_id: DocumentId::new("d").expect("id"),
                doc_version: 1,
                chunk_ordinal: 0,
                start: 0,
                end: 0,
                source: None,
                title: None,
            },
        }
    }

    fn ccfg() -> ContradictionConfig {
        ContradictionConfig {
            enabled: true,
            min_overlap: 0.5,
            numeric_tolerance: 0.01,
            decision: None,
        }
    }

    #[test]
    fn exact_and_near_duplicates_collapse_into_the_first() {
        let d = Dedup {
            enabled: true,
            near_duplicate_similarity: 0.8,
        };
        let out = dedup(
            vec![
                (ev(1, "The pump failed on Monday morning"), 1),
                (ev(2, "the pump failed on monday morning!"), 1),
                (ev(3, "Pump failed Monday morning"), 3),
                (ev(4, "Revenue grew in the third quarter"), 4),
            ],
            &d,
        );
        assert_eq!(
            out.iter().map(|e| e.chunk_id.0).collect::<Vec<_>>(),
            vec![1, 4]
        );
    }

    #[test]
    fn negation_and_numeric_conflicts_are_flagged() {
        let e = [
            ev(1, "The backup generator supports automatic failover"),
            ev(
                2,
                "The backup generator does not support automatic failover",
            ),
            ev(3, "Maximum pump pressure is 120 bar during operation"),
            ev(4, "Maximum pump pressure is 150 bar during operation"),
            ev(5, "Quarterly revenue grew"),
        ];
        let c = contradictions(&e, &ccfg());
        assert!(
            c.iter()
                .any(|x| x.detector == NEGATION_DETECTOR && (x.a.0, x.b.0) == (1, 2))
        );
        assert!(
            c.iter()
                .any(|x| x.detector == NUMERIC_DETECTOR && (x.a.0, x.b.0) == (3, 4))
        );
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn values_within_tolerance_do_not_conflict() {
        let e = [
            ev(1, "Pump pressure is 100.0 bar nominal"),
            ev(2, "Pump pressure is 100.5 bar nominal"),
        ];
        assert!(
            contradictions(
                &e,
                &ContradictionConfig {
                    numeric_tolerance: 0.01,
                    ..ccfg()
                }
            )
            .is_empty()
        );
    }
}
