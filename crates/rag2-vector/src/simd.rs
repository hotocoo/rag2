//! Distance kernels. Written so LLVM vectorizes them (NEON on aarch64, AVX2 with
//! `-C target-cpu=native` on x86_64): fixed-width lanes with independent accumulators break the
//! floating-point dependency chain that would otherwise serialize the reduction.

/// Independent accumulators per dot product; 4x NEON / 2x AVX2 registers of f32.
const LANES: usize = 16;

/// Dot product of two equal-length slices.
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = [0f32; LANES];
    let (ca, ra) = a.as_chunks::<LANES>();
    let (cb, rb) = b.as_chunks::<LANES>();
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..LANES {
            acc[i] = x[i].mul_add(y[i], acc[i]);
        }
    }
    let tail: f32 = ra.iter().zip(rb).map(|(x, y)| x * y).sum();
    acc.iter().sum::<f32>() + tail
}

/// Scale `v` to unit length. A zero vector stays zero (it matches nothing).
pub fn normalized(v: &[f32]) -> Vec<f32> {
    let norm = dot(v, v).sqrt();
    if norm > 0.0 && norm.is_finite() {
        v.iter().map(|x| x / norm).collect()
    } else {
        vec![0.0; v.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn dot_matches_the_naive_sum(v in prop::collection::vec(-10f32..10.0, 0..200)) {
            let w: Vec<f32> = v.iter().rev().copied().collect();
            let naive: f32 = v.iter().zip(&w).map(|(a, b)| a * b).sum();
            let tol = 1e-3 * (1.0 + naive.abs());
            prop_assert!((dot(&v, &w) - naive).abs() <= tol);
        }
    }

    #[test]
    fn normalized_vectors_have_unit_length_and_zero_stays_zero() {
        let n = normalized(&[3.0, 4.0]);
        assert!((dot(&n, &n) - 1.0).abs() < 1e-6);
        assert_eq!(normalized(&[0.0, 0.0]), vec![0.0, 0.0]);
    }
}
