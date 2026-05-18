// SPDX-License-Identifier: Apache-2.0

//! Fused dtype-decode + distance kernels for F16 and BF16 byte buffers.
//!
//! Each kernel iterates element-by-element, decoding two bytes per element
//! to f32 inline, avoiding the intermediate `Vec<f32>` allocation that
//! `cast_to_f32` would require. Callers must run `validate_byte_len` before
//! invoking any function here; the `debug_assert_eq!` guards are safety nets,
//! not primary validation.

use half::{bf16, f16};

// ── F16 kernels ──────────────────────────────────────────────────────────────

/// L2-squared distance between two F16-encoded byte slices.
///
/// Returns `Σ (aᵢ - bᵢ)²` computed in f32 after decoding each element.
pub(crate) fn l2_squared_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    debug_assert_eq!(a.len(), dim * 2);
    debug_assert_eq!(b.len(), dim * 2);
    let mut acc = 0.0_f32;
    for i in 0..dim {
        let off = i * 2;
        let av = f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        let diff = av - bv;
        acc += diff * diff;
    }
    acc
}

/// Cosine distance between two F16-encoded byte slices.
///
/// Computes `1 - dot(a,b) / (‖a‖ · ‖b‖)` in a single pass. If either
/// vector has zero magnitude, returns `1.0` (maximum distance), matching
/// the convention in `nodedb_types::vector_distance::cosine_distance`.
pub(crate) fn cosine_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    debug_assert_eq!(a.len(), dim * 2);
    debug_assert_eq!(b.len(), dim * 2);
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for i in 0..dim {
        let off = i * 2;
        let av = f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
        norm_a += av * av;
        norm_b += bv * bv;
    }
    let denom = (norm_a * norm_b).sqrt();
    if denom < f32::EPSILON {
        return 1.0;
    }
    (1.0 - (dot / denom)).max(0.0)
}

/// Negative inner product between two F16-encoded byte slices.
///
/// Returns `-dot(a, b)`. Negated so that "lower is closer" ordering is
/// consistent with all other distance metrics.
pub(crate) fn neg_inner_product_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    debug_assert_eq!(a.len(), dim * 2);
    debug_assert_eq!(b.len(), dim * 2);
    let mut dot = 0.0_f32;
    for i in 0..dim {
        let off = i * 2;
        let av = f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
    }
    -dot
}

// ── BF16 kernels ─────────────────────────────────────────────────────────────

/// L2-squared distance between two BF16-encoded byte slices.
///
/// Byte-for-byte identical to `l2_squared_f16` except uses
/// `bf16::from_le_bytes` for element decoding.
pub(crate) fn l2_squared_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    debug_assert_eq!(a.len(), dim * 2);
    debug_assert_eq!(b.len(), dim * 2);
    let mut acc = 0.0_f32;
    for i in 0..dim {
        let off = i * 2;
        let av = bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        let diff = av - bv;
        acc += diff * diff;
    }
    acc
}

/// Cosine distance between two BF16-encoded byte slices.
///
/// Same single-pass formula as `cosine_f16`; zero-magnitude returns `1.0`.
pub(crate) fn cosine_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    debug_assert_eq!(a.len(), dim * 2);
    debug_assert_eq!(b.len(), dim * 2);
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for i in 0..dim {
        let off = i * 2;
        let av = bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
        norm_a += av * av;
        norm_b += bv * bv;
    }
    let denom = (norm_a * norm_b).sqrt();
    if denom < f32::EPSILON {
        return 1.0;
    }
    (1.0 - (dot / denom)).max(0.0)
}

/// Negative inner product between two BF16-encoded byte slices.
///
/// Returns `-dot(a, b)` with BF16 element decoding.
pub(crate) fn neg_inner_product_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    debug_assert_eq!(a.len(), dim * 2);
    debug_assert_eq!(b.len(), dim * 2);
    let mut dot = 0.0_f32;
    for i in 0..dim {
        let off = i * 2;
        let av = bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
    }
    -dot
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::DistanceMetric;
    use crate::dtype::cast_from_f32;
    use nodedb_types::vector_dtype::VectorStorageDtype;

    const EPS_F16: f32 = 1e-2;
    const EPS_BF16: f32 = 1e-1;

    // Fixed vector pair shared across all tests.
    const A: [f32; 5] = [0.5, -1.0, 2.5, 0.1, 100.0];
    const B: [f32; 5] = [1.0, 0.5, -1.5, 2.0, 50.0];

    // Reference: cast F32 → dtype → F32, then run the F32 distance kernel.
    fn f32_reference_via_roundtrip(metric: DistanceMetric, dtype: VectorStorageDtype) -> f32 {
        use crate::dtype::cast_to_f32;
        let a_bytes = cast_from_f32(&A, dtype);
        let b_bytes = cast_from_f32(&B, dtype);
        let a_f32 = cast_to_f32(&a_bytes, dtype, 5).expect("round-trip cast must succeed");
        let b_f32 = cast_to_f32(&b_bytes, dtype, 5).expect("round-trip cast must succeed");
        crate::distance::distance(&a_f32, &b_f32, metric)
    }

    // ── F16 × L2 ──────────────────────────────────────────────────────────────

    #[test]
    fn f16_l2_matches_reference() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::F16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::F16);
        let fused = l2_squared_f16(&a_bytes, &b_bytes, 5);
        let reference = f32_reference_via_roundtrip(DistanceMetric::L2, VectorStorageDtype::F16);
        assert!(
            (fused - reference).abs() < EPS_F16,
            "f16 L2: fused={fused}, reference={reference}, diff={}",
            (fused - reference).abs()
        );
    }

    // ── F16 × Cosine ──────────────────────────────────────────────────────────

    #[test]
    fn f16_cosine_matches_reference() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::F16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::F16);
        let fused = cosine_f16(&a_bytes, &b_bytes, 5);
        let reference =
            f32_reference_via_roundtrip(DistanceMetric::Cosine, VectorStorageDtype::F16);
        assert!(
            (fused - reference).abs() < EPS_F16,
            "f16 Cosine: fused={fused}, reference={reference}, diff={}",
            (fused - reference).abs()
        );
    }

    // ── F16 × InnerProduct ────────────────────────────────────────────────────

    #[test]
    fn f16_neg_inner_product_matches_reference() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::F16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::F16);
        let fused = neg_inner_product_f16(&a_bytes, &b_bytes, 5);
        let reference =
            f32_reference_via_roundtrip(DistanceMetric::InnerProduct, VectorStorageDtype::F16);
        assert!(
            (fused - reference).abs() < EPS_F16,
            "f16 NegIP: fused={fused}, reference={reference}, diff={}",
            (fused - reference).abs()
        );
    }

    // ── BF16 × L2 ─────────────────────────────────────────────────────────────

    #[test]
    fn bf16_l2_matches_reference() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::BF16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::BF16);
        let fused = l2_squared_bf16(&a_bytes, &b_bytes, 5);
        let reference = f32_reference_via_roundtrip(DistanceMetric::L2, VectorStorageDtype::BF16);
        assert!(
            (fused - reference).abs() < EPS_BF16,
            "bf16 L2: fused={fused}, reference={reference}, diff={}",
            (fused - reference).abs()
        );
    }

    // ── BF16 × Cosine ─────────────────────────────────────────────────────────

    #[test]
    fn bf16_cosine_matches_reference() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::BF16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::BF16);
        let fused = cosine_bf16(&a_bytes, &b_bytes, 5);
        let reference =
            f32_reference_via_roundtrip(DistanceMetric::Cosine, VectorStorageDtype::BF16);
        assert!(
            (fused - reference).abs() < EPS_BF16,
            "bf16 Cosine: fused={fused}, reference={reference}, diff={}",
            (fused - reference).abs()
        );
    }

    // ── BF16 × InnerProduct ───────────────────────────────────────────────────

    #[test]
    fn bf16_neg_inner_product_matches_reference() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::BF16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::BF16);
        let fused = neg_inner_product_bf16(&a_bytes, &b_bytes, 5);
        let reference =
            f32_reference_via_roundtrip(DistanceMetric::InnerProduct, VectorStorageDtype::BF16);
        assert!(
            (fused - reference).abs() < EPS_BF16,
            "bf16 NegIP: fused={fused}, reference={reference}, diff={}",
            (fused - reference).abs()
        );
    }

    // ── Zero-norm cosine returns 1.0 ─────────────────────────────────────────

    #[test]
    fn f16_zero_norm_cosine_returns_one() {
        let zero = [0.0_f32; 5];
        let zero_bytes = cast_from_f32(&zero, VectorStorageDtype::F16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::F16);
        assert_eq!(
            cosine_f16(&zero_bytes, &b_bytes, 5),
            1.0,
            "f16 cosine of zero vector must be 1.0"
        );
        assert_eq!(
            cosine_f16(&b_bytes, &zero_bytes, 5),
            1.0,
            "f16 cosine against zero vector must be 1.0"
        );
    }

    #[test]
    fn bf16_zero_norm_cosine_returns_one() {
        let zero = [0.0_f32; 5];
        let zero_bytes = cast_from_f32(&zero, VectorStorageDtype::BF16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::BF16);
        assert_eq!(
            cosine_bf16(&zero_bytes, &b_bytes, 5),
            1.0,
            "bf16 cosine of zero vector must be 1.0"
        );
        assert_eq!(
            cosine_bf16(&b_bytes, &zero_bytes, 5),
            1.0,
            "bf16 cosine against zero vector must be 1.0"
        );
    }
}
