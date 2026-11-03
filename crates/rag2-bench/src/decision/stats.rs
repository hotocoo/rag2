//! Metrics shared by the decision suites: latency percentiles, AUROC, precision-targeted
//! thresholds, and a bounded worker pool that keeps input order.

use anyhow::Result;
use hdrhistogram::Histogram;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Significant figures kept by the latency histograms.
const HIST_SIGFIG: u8 = 3;
const US_PER_MS: f64 = 1_000.0;

#[derive(Serialize, Clone, Default)]
pub struct Latency {
    pub n: u64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub max_ms: f64,
}

/// Percentiles of microsecond samples, reported in milliseconds.
pub fn latency(us: &[u64]) -> Result<Latency> {
    let mut h = Histogram::<u64>::new(HIST_SIGFIG)?;
    for s in us {
        h.record((*s).max(1))?;
    }
    let ms = |q: f64| h.value_at_quantile(q) as f64 / US_PER_MS;
    Ok(Latency {
        n: h.len(),
        mean_ms: h.mean() / US_PER_MS,
        p50_ms: ms(0.50),
        p95_ms: ms(0.95),
        p99_ms: ms(0.99),
        p999_ms: ms(0.999),
        max_ms: h.max() as f64 / US_PER_MS,
    })
}

/// Area under the ROC curve (Mann-Whitney U, ties count half). `None` without both classes.
pub fn auroc(scored: &[(f64, bool)]) -> Option<f64> {
    let pos = scored.iter().filter(|s| s.1).count();
    let neg = scored.len() - pos;
    if pos == 0 || neg == 0 {
        return None;
    }
    let mut v: Vec<(f64, bool)> = scored.to_vec();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Average 1-based ranks over tied scores.
    let mut rank_sum_pos = 0.0;
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1].0 == v[i].0 {
            j += 1;
        }
        let avg = (i + j + 2) as f64 / 2.0;
        rank_sum_pos += avg * v[i..=j].iter().filter(|s| s.1).count() as f64;
        i = j + 1;
    }
    let u = rank_sum_pos - (pos * (pos + 1)) as f64 / 2.0;
    Some(u / (pos * neg) as f64)
}

#[derive(Serialize, Clone)]
pub struct Threshold {
    pub threshold: f64,
    pub precision: f64,
    pub recall: f64,
}

/// The lowest score threshold whose positive calls reach `target` precision (the most recall at
/// that precision). `None` when no threshold reaches it.
pub fn threshold_for_precision(scored: &[(f64, bool)], target: f64) -> Option<Threshold> {
    let pos = scored.iter().filter(|s| s.1).count();
    if pos == 0 {
        return None;
    }
    let mut v: Vec<(f64, bool)> = scored.to_vec();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut best = None;
    let mut tp = 0usize;
    for (i, (s, y)) in v.iter().enumerate() {
        tp += usize::from(*y);
        // Only cut between distinct scores.
        if v.get(i + 1).is_some_and(|n| n.0 == *s) {
            continue;
        }
        let precision = tp as f64 / (i + 1) as f64;
        if precision >= target {
            best = Some(Threshold {
                threshold: *s,
                precision,
                recall: tp as f64 / pos as f64,
            });
        }
    }
    best
}

/// Run `f` over `items` on `workers` threads; results keep input order.
pub fn fan_out<T: Sync, R: Send>(
    workers: usize,
    items: &[T],
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..workers.clamp(1, items.len().max(1)))
            .map(|_| {
                sc.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(x) = items.get(i) else { break };
                        mine.push((i, f(x)));
                    }
                    mine
                })
            })
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().expect("bench worker panicked"))
            .collect()
    });
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auroc_ranks_and_ties() {
        let perfect = [(0.9, true), (0.8, true), (0.2, false), (0.1, false)];
        assert_eq!(auroc(&perfect), Some(1.0));
        let inverted = [(0.1, true), (0.9, false)];
        assert_eq!(auroc(&inverted), Some(0.0));
        let tied = [(0.5, true), (0.5, false)];
        assert_eq!(auroc(&tied), Some(0.5));
        assert_eq!(auroc(&[(0.5, true)]), None);
    }

    #[test]
    fn threshold_reaches_target_with_most_recall() {
        let s = [
            (0.95, true),
            (0.9, true),
            (0.8, false),
            (0.7, true),
            (0.6, false),
            (0.5, false),
        ];
        let t = threshold_for_precision(&s, 0.75).expect("reachable");
        assert_eq!(t.threshold, 0.7);
        assert!((t.recall - 1.0).abs() < 1e-12);
        assert!(threshold_for_precision(&[(0.9, false), (0.1, true)], 0.99).is_none());
    }

    #[test]
    fn fan_out_keeps_order() {
        let v: Vec<usize> = (0..100).collect();
        assert_eq!(
            fan_out(7, &v, |x| x * 2),
            v.iter().map(|x| x * 2).collect::<Vec<_>>()
        );
    }
}
