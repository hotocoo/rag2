//! Retrieval confidence and its calibration.
//!
//! Raw confidence is the mean of the available signals: best dense similarity (when dense ran),
//! query term coverage of the top evidence, and retriever agreement on the top result. It is a
//! heuristic score, not a probability, until `calibration.slope/intercept` are fitted on labelled
//! queries (Platt scaling); the defaults (1, 0) leave it unchanged.

use crate::api::ConfidenceDetail;
use crate::text::content_words;
use rag2_config::schema::Calibration;
use rag2_core::model::{Evidence, RetrieverKind};
use std::collections::{BTreeSet, HashSet};

/// Keeps logit finite at 0 and 1.
const LOGIT_EPS: f32 = 1e-4;

pub fn detail(
    query: &str,
    evidence: &[Evidence],
    ran: &BTreeSet<RetrieverKind>,
    coverage_top_n: usize,
) -> ConfidenceDetail {
    if evidence.is_empty() {
        return ConfidenceDetail::default();
    }
    let dense_similarity = evidence
        .iter()
        .flat_map(|e| e.signals.iter())
        .filter(|s| s.retriever == RetrieverKind::Dense && s.hop == 0)
        .map(|s| s.score.clamp(0.0, 1.0))
        .reduce(f32::max);
    let q = content_words(query);
    let seen: HashSet<String> = evidence
        .iter()
        .take(coverage_top_n)
        .flat_map(|e| content_words(&e.text))
        .collect();
    let term_coverage = if q.is_empty() {
        0.0
    } else {
        q.iter().filter(|w| seen.contains(*w)).count() as f32 / q.len() as f32
    };
    let agreeing: BTreeSet<RetrieverKind> = evidence[0]
        .signals
        .iter()
        .filter(|s| s.hop == 0)
        .map(|s| s.retriever)
        .collect();
    let agreement = if ran.is_empty() {
        0.0
    } else {
        agreeing.intersection(ran).count() as f32 / ran.len() as f32
    };
    let parts: Vec<f32> = dense_similarity
        .into_iter()
        .chain([term_coverage, agreement])
        .collect();
    let raw = parts.iter().sum::<f32>() / parts.len() as f32;
    ConfidenceDetail {
        raw,
        dense_similarity,
        term_coverage,
        agreement,
    }
}

/// `sigmoid(slope * logit(raw) + intercept)`.
pub fn calibrate(raw: f32, c: &Calibration) -> f32 {
    let x = raw.clamp(LOGIT_EPS, 1.0 - LOGIT_EPS);
    let logit = (x / (1.0 - x)).ln();
    1.0 / (1.0 + (-(c.slope * logit + c.intercept)).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_calibration_preserves_the_raw_score() {
        let c = Calibration {
            slope: 1.0,
            intercept: 0.0,
        };
        for x in [0.1f32, 0.5, 0.9] {
            assert!((calibrate(x, &c) - x).abs() < 1e-4);
        }
        let sharper = Calibration {
            slope: 3.0,
            intercept: 0.0,
        };
        assert!(calibrate(0.8, &sharper) > 0.95);
        assert!(calibrate(0.2, &sharper) < 0.05);
    }
}
