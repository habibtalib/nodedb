// SPDX-License-Identifier: Apache-2.0

//! Dtype-aware distance dispatch shim.
//!
//! Routes byte-encoded vector pairs to the correct distance kernel based on
//! dtype and metric. F16 / BF16 inputs on the three hot metrics (L2, Cosine,
//! InnerProduct) use fused decode-and-compute kernels from `typed_scalar`,
//! eliminating the intermediate `Vec<f32>` allocation. Rare metrics (Manhattan,
//! Chebyshev, Hamming, Jaccard, Pearson) fall through to `cast_to_f32` +
//! `distance()` — correctness is identical and they are not on the hot path.
//! The F32 path is always a straight cast + delegate (no conversion needed).

use nodedb_types::vector_distance::DistanceMetric;
use nodedb_types::vector_dtype::VectorStorageDtype;

use crate::dtype::{DtypeError, cast_to_f32, validate_byte_len};

/// Error type for [`distance_typed`].
#[derive(thiserror::Error, Debug)]
pub enum DistanceError {
    /// The two input buffers encode different numbers of dimensions.
    #[error("distance: dim mismatch (a: {a_dim}, b: {b_dim})")]
    DimMismatch { a_dim: usize, b_dim: usize },

    /// A byte buffer has the wrong length for the given dtype and dim.
    #[error("distance: dtype byte-length error: {0}")]
    Dtype(#[from] DtypeError),
}

/// Compute distance between two byte-encoded vectors of the given dtype.
///
/// Both buffers must encode `dim` dimensions in `dtype`.
///
/// - **F16 / BF16 + L2 / Cosine / InnerProduct**: fused decode-and-compute via
///   [`typed_scalar`] — no intermediate `Vec<f32>` allocation.
/// - **F16 / BF16 + other metrics**: up-converts to F32 via [`cast_to_f32`]
///   then delegates to [`crate::distance::distance`]. These metrics are not
///   used in embedding search hot paths; the allocation is acceptable.
/// - **F32**: up-converts via [`cast_to_f32`] (memcopy) then delegates.
///
/// # Errors
///
/// - [`DistanceError::Dtype`] — if either buffer's length does not match
///   `dtype.bytes_for_dim(dim)`.
/// - [`DistanceError::DimMismatch`] — reserved for asymmetric validation;
///   currently both sides are checked against the same `dim`, so this variant
///   is unreachable in normal use but is preserved for future asymmetric checks.
pub fn distance_typed(
    metric: DistanceMetric,
    dtype: VectorStorageDtype,
    a_bytes: &[u8],
    b_bytes: &[u8],
    dim: usize,
) -> Result<f32, DistanceError> {
    validate_byte_len(a_bytes, dtype, dim)?;
    validate_byte_len(b_bytes, dtype, dim)?;

    match dtype {
        VectorStorageDtype::F32 => {
            let a_f32 = cast_to_f32(a_bytes, dtype, dim)?;
            let b_f32 = cast_to_f32(b_bytes, dtype, dim)?;
            Ok(crate::distance::distance(&a_f32, &b_f32, metric))
        }
        VectorStorageDtype::F16 => Ok(match metric {
            DistanceMetric::L2 => {
                (crate::distance::simd::runtime().l2_squared_f16)(a_bytes, b_bytes, dim)
            }
            DistanceMetric::Cosine => {
                (crate::distance::simd::runtime().cosine_distance_f16)(a_bytes, b_bytes, dim)
            }
            DistanceMetric::InnerProduct => {
                (crate::distance::simd::runtime().neg_inner_product_f16)(a_bytes, b_bytes, dim)
            }
            // Non-hot metrics (Manhattan, Chebyshev, Hamming, Jaccard, Pearson)
            // are not used in embedding search; cast+delegate is correct and the
            // allocation overhead is acceptable on this path.
            _ => {
                let a_f32 = cast_to_f32(a_bytes, dtype, dim)?;
                let b_f32 = cast_to_f32(b_bytes, dtype, dim)?;
                crate::distance::distance(&a_f32, &b_f32, metric)
            }
        }),
        VectorStorageDtype::BF16 => Ok(match metric {
            DistanceMetric::L2 => {
                (crate::distance::simd::runtime().l2_squared_bf16)(a_bytes, b_bytes, dim)
            }
            DistanceMetric::Cosine => {
                (crate::distance::simd::runtime().cosine_distance_bf16)(a_bytes, b_bytes, dim)
            }
            DistanceMetric::InnerProduct => {
                (crate::distance::simd::runtime().neg_inner_product_bf16)(a_bytes, b_bytes, dim)
            }
            // Non-hot metrics fall through to cast+delegate (same rationale as F16 arm).
            _ => {
                let a_f32 = cast_to_f32(a_bytes, dtype, dim)?;
                let b_f32 = cast_to_f32(b_bytes, dtype, dim)?;
                crate::distance::distance(&a_f32, &b_f32, metric)
            }
        }),
        // `VectorStorageDtype` is #[non_exhaustive]; any future variant falls
        // back to the cast path, which will return DtypeError if it cannot handle
        // the variant. This arm is not reachable with any currently-defined dtype.
        _ => {
            let a_f32 = cast_to_f32(a_bytes, dtype, dim)?;
            let b_f32 = cast_to_f32(b_bytes, dtype, dim)?;
            Ok(crate::distance::distance(&a_f32, &b_f32, metric))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dtype::cast_from_f32;

    const EPS_F32: f32 = 1e-6;
    const EPS_F16: f32 = 1e-2;
    const EPS_BF16: f32 = 1e-1;

    // Fixed vector pair used across all tests.
    const A: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
    const B: [f32; 4] = [4.0, 3.0, 2.0, 1.0];

    fn f32_ref(metric: DistanceMetric) -> f32 {
        crate::distance::distance(&A, &B, metric)
    }

    // ── F32 path: byte-exact match with direct distance() ────────────────────

    #[test]
    fn f32_path_matches_direct_distance() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::F32);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::F32);
        for metric in [
            DistanceMetric::L2,
            DistanceMetric::Cosine,
            DistanceMetric::InnerProduct,
        ] {
            let via_typed = distance_typed(metric, VectorStorageDtype::F32, &a_bytes, &b_bytes, 4)
                .expect("F32 typed distance must not fail");
            let via_direct = f32_ref(metric);
            assert_eq!(
                via_typed, via_direct,
                "F32 typed vs direct mismatch for {metric:?}"
            );
        }
    }

    // ── F16 round-trip within tolerance ──────────────────────────────────────

    #[test]
    fn f16_round_trip_within_tolerance() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::F16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::F16);
        for metric in [
            DistanceMetric::L2,
            DistanceMetric::Cosine,
            DistanceMetric::InnerProduct,
        ] {
            let via_typed = distance_typed(metric, VectorStorageDtype::F16, &a_bytes, &b_bytes, 4)
                .expect("F16 typed distance must not fail");
            let reference = f32_ref(metric);
            assert!(
                (via_typed - reference).abs() < EPS_F16,
                "F16 typed distance for {metric:?}: got {via_typed}, ref {reference}, diff {}",
                (via_typed - reference).abs()
            );
        }
    }

    // ── BF16 round-trip within tolerance ─────────────────────────────────────

    #[test]
    fn bf16_round_trip_within_tolerance() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::BF16);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::BF16);
        for metric in [
            DistanceMetric::L2,
            DistanceMetric::Cosine,
            DistanceMetric::InnerProduct,
        ] {
            let via_typed = distance_typed(metric, VectorStorageDtype::BF16, &a_bytes, &b_bytes, 4)
                .expect("BF16 typed distance must not fail");
            let reference = f32_ref(metric);
            assert!(
                (via_typed - reference).abs() < EPS_BF16,
                "BF16 typed distance for {metric:?}: got {via_typed}, ref {reference}, diff {}",
                (via_typed - reference).abs()
            );
        }
    }

    // ── Dim mismatch is caught ────────────────────────────────────────────────

    #[test]
    fn dim_mismatch_returns_dtype_error() {
        // a_bytes encodes 2 F32 dims (8 bytes); b_bytes encodes 4 F32 dims (16 bytes).
        // validate_byte_len(b_bytes, F32, 2) should fire with BadByteLen.
        let a_bytes = [0u8; 8];
        let b_bytes = [0u8; 16];
        let err = distance_typed(
            DistanceMetric::L2,
            VectorStorageDtype::F32,
            &a_bytes,
            &b_bytes,
            2,
        )
        .expect_err("mismatched buffer must return an error");
        match err {
            DistanceError::Dtype(DtypeError::BadByteLen {
                dtype,
                dim,
                expected,
                actual,
            }) => {
                assert_eq!(dtype, VectorStorageDtype::F32);
                assert_eq!(dim, 2);
                assert_eq!(expected, 8);
                assert_eq!(actual, 16);
            }
            other => panic!("expected DistanceError::Dtype(BadByteLen), got {other:?}"),
        }
    }

    // ── All three metrics × all three dtypes: finite + non-NaN ───────────────

    #[test]
    fn all_metrics_all_dtypes_finite_non_nan() {
        let metrics = [
            DistanceMetric::L2,
            DistanceMetric::Cosine,
            DistanceMetric::InnerProduct,
        ];
        let dtypes = [
            VectorStorageDtype::F32,
            VectorStorageDtype::F16,
            VectorStorageDtype::BF16,
        ];
        for &metric in &metrics {
            for &dtype in &dtypes {
                let a_bytes = cast_from_f32(&A, dtype);
                let b_bytes = cast_from_f32(&B, dtype);
                let result =
                    distance_typed(metric, dtype, &a_bytes, &b_bytes, 4).unwrap_or_else(|e| {
                        panic!("distance_typed({metric:?}, {dtype:?}) failed: {e}")
                    });
                assert!(
                    result.is_finite() && !result.is_nan(),
                    "distance_typed({metric:?}, {dtype:?}) returned non-finite/NaN: {result}"
                );
            }
        }
    }

    // ── F32 path: result is finite ────────────────────────────────────────────

    #[test]
    fn f32_result_finite() {
        let a_bytes = cast_from_f32(&A, VectorStorageDtype::F32);
        let b_bytes = cast_from_f32(&B, VectorStorageDtype::F32);
        let result = distance_typed(
            DistanceMetric::L2,
            VectorStorageDtype::F32,
            &a_bytes,
            &b_bytes,
            4,
        )
        .expect("F32 distance must succeed");
        assert!(
            result.is_finite(),
            "F32 L2 result must be finite, got {result}"
        );
        assert!((result - f32_ref(DistanceMetric::L2)).abs() < EPS_F32);
    }
}
