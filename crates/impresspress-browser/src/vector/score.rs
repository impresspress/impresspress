//! Vector scoring helpers. SIMD and portable paths share the same `top_k`
//! ranking logic; only the per-pair score function differs.

use wafer_core::interfaces::vector::service::DistanceMetric;

/// Compute a similarity score for `a` and `b` under the given metric.
/// Higher is better for `Cosine` and `DotProduct`; lower is better for
/// `Euclidean`. Callers normalise via `score_for_ranking` below.
pub fn pair_score(a: &[f32], b: &[f32], metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::DotProduct => dot(a, b),
        DistanceMetric::Cosine => {
            let na = norm(a);
            let nb = norm(b);
            if na == 0.0 || nb == 0.0 {
                0.0
            } else {
                dot(a, b) / (na * nb)
            }
        }
        DistanceMetric::Euclidean => {
            let mut s = 0.0_f32;
            for i in 0..a.len() {
                let d = a[i] - b[i];
                s += d * d;
            }
            s.sqrt()
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    use core::arch::wasm32::*;
    let n = a.len();
    let chunks = n / 4;
    let mut acc = f32x4_splat(0.0);
    for i in 0..chunks {
        let va = unsafe { v128_load(a.as_ptr().add(i * 4) as *const v128) };
        let vb = unsafe { v128_load(b.as_ptr().add(i * 4) as *const v128) };
        acc = f32x4_add(acc, f32x4_mul(va, vb));
    }
    let mut s = f32x4_extract_lane::<0>(acc)
        + f32x4_extract_lane::<1>(acc)
        + f32x4_extract_lane::<2>(acc)
        + f32x4_extract_lane::<3>(acc);
    for i in (chunks * 4)..n {
        s += a[i] * b[i];
    }
    s
}

#[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut s = 0.0_f32;
    for i in 0..a.len() {
        s += a[i] * b[i];
    }
    s
}

fn norm(v: &[f32]) -> f32 {
    let mut s = 0.0_f32;
    for x in v {
        s += x * x;
    }
    s.sqrt()
}

/// Convert a metric-specific score to a "higher is better" ranking score so
/// `top_k` is uniform across metrics.
pub fn rank_score(raw: f32, metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::Cosine | DistanceMetric::DotProduct => raw,
        DistanceMetric::Euclidean => -raw,
    }
}

/// Score every candidate against `query`, return the top-k id+score pairs
/// ordered by descending rank score (using `rank_score` so all metrics agree
/// "higher is better").
pub fn top_k(
    query: &[f32],
    candidates: &[(String, Vec<f32>)],
    k: usize,
    metric: DistanceMetric,
) -> Vec<(String, f32)> {
    top_k_borrowed(
        query,
        candidates.iter().map(|(id, v)| (id.as_str(), v.as_slice())),
        k,
        metric,
    )
}

/// Borrowed-input variant of [`top_k`]. Hot-path callers can stream
/// `(&str, &[f32])` pairs without cloning ids/vectors first; ids are only
/// allocated for the (at most `k`) survivors of the truncate step.
pub fn top_k_borrowed<'a, I>(
    query: &[f32],
    candidates: I,
    k: usize,
    metric: DistanceMetric,
) -> Vec<(String, f32)>
where
    I: IntoIterator<Item = (&'a str, &'a [f32])>,
{
    if k == 0 {
        return Vec::new();
    }
    // Score first (cheap String slice for ids), then truncate, then own.
    let mut scored: Vec<(&str, f32)> = candidates
        .into_iter()
        .map(|(id, v)| {
            let raw = pair_score(query, v, metric);
            (id, rank_score(raw, metric))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
        .into_iter()
        .map(|(id, s)| (id.to_string(), s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_product_basic() {
        let s = pair_score(
            &[1.0, 2.0, 3.0],
            &[4.0, 5.0, 6.0],
            DistanceMetric::DotProduct,
        );
        assert!((s - 32.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![0.3, 0.4, 0.5];
        let s = pair_score(&v, &v, DistanceMetric::Cosine);
        assert!((s - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let s = pair_score(&[1.0, 0.0], &[0.0, 1.0], DistanceMetric::Cosine);
        assert!(s.abs() < 1e-5);
    }

    #[test]
    fn cosine_zero_vector_is_zero() {
        let s = pair_score(&[0.0, 0.0], &[1.0, 1.0], DistanceMetric::Cosine);
        assert_eq!(s, 0.0);
    }

    #[test]
    fn euclidean_distance() {
        let s = pair_score(&[0.0, 0.0], &[3.0, 4.0], DistanceMetric::Euclidean);
        assert!((s - 5.0).abs() < 1e-5);
    }

    #[test]
    fn rank_score_inverts_euclidean() {
        assert_eq!(rank_score(5.0, DistanceMetric::Euclidean), -5.0);
        assert_eq!(rank_score(0.7, DistanceMetric::Cosine), 0.7);
    }

    #[test]
    fn top_k_orders_by_descending_rank_score() {
        let candidates = vec![
            ("a".to_string(), vec![1.0, 0.0]),
            ("b".to_string(), vec![0.5, 0.5]),
            ("c".to_string(), vec![-1.0, 0.0]),
        ];
        let q = vec![1.0, 0.0];
        let top = top_k(&q, &candidates, 2, DistanceMetric::Cosine);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "a"); // cos = 1
        assert_eq!(top[1].0, "b"); // cos ≈ 0.707
        assert!((top[0].1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn top_k_caps_at_input_length() {
        let candidates = vec![("a".to_string(), vec![1.0])];
        let top = top_k(&[1.0], &candidates, 10, DistanceMetric::DotProduct);
        assert_eq!(top.len(), 1);
    }

    #[test]
    fn top_k_zero_returns_empty() {
        let candidates = vec![("a".to_string(), vec![1.0])];
        let top = top_k(&[1.0], &candidates, 0, DistanceMetric::DotProduct);
        assert!(top.is_empty());
    }
}
