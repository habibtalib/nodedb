// SPDX-License-Identifier: Apache-2.0

//! `RerankCodec` wrapper for binary (sign-bit) quantization.
//!
//! Bridges `BinaryCodec` (which implements `VectorCodec` with associated types)
//! into the object-safe `RerankCodec` trait used by the rerank sidecar.
//!
//! Binary has no learned state: `train()` is satisfied by the default no-op.

use nodedb_codec::vector_quant::layout::UnifiedQuantizedVectorRef;

use crate::{
    quantize::binary_codec::BinaryCodec,
    rerank::codec::{CodecName, PreparedQuery, RerankCodec},
    rerank::types::RerankError,
};

// ── packed_bits_len helper ────────────────────────────────────────────────────

/// Binary is 1 bpw: `ceil(dim / 8)` bytes.
#[inline]
fn binary_packed_bits_len(dim: usize) -> usize {
    dim.div_ceil(8)
}

// ── BinaryRerank ──────────────────────────────────────────────────────────────

/// Object-safe `RerankCodec` wrapper around `BinaryCodec`.
///
/// Binary has no learned parameters. All instances with the same `dim` are
/// equivalent. `train()` is the default no-op.
pub struct BinaryRerank {
    codec: BinaryCodec,
    dim: usize,
}

impl BinaryRerank {
    /// Create a binary rerank codec for vectors of length `dim`.
    pub fn new(dim: usize) -> Self {
        Self {
            codec: BinaryCodec { dim },
            dim,
        }
    }
}

impl RerankCodec for BinaryRerank {
    /// Encode a full-precision vector to binary sign bits.
    ///
    /// The serialized form is the raw `UnifiedQuantizedVector` buffer
    /// (`as_bytes()`): 32-byte `QuantHeader` followed by `ceil(dim/8)` bytes
    /// of packed sign bits.
    fn encode(&self, v: &[f32]) -> Result<Vec<u8>, RerankError> {
        if v.len() != self.dim {
            return Err(RerankError::BadInput(format!(
                "binary encode: vector len {} != codec dim {}",
                v.len(),
                self.dim
            )));
        }
        use nodedb_codec::vector_quant::codec::VectorCodec as _;
        let quantized = self.codec.encode(v);
        Ok(quantized.as_ref().as_bytes().to_vec())
    }

    /// Prepare the query for repeated distance calls.
    ///
    /// Binary encodes both the query and candidates to sign bits and computes
    /// Hamming distance. The prepared form is `PreparedQuery::Bytes` holding
    /// the packed query bits.
    fn prepare_query(&self, q: &[f32]) -> Result<PreparedQuery, RerankError> {
        if q.len() != self.dim {
            return Err(RerankError::BadInput(format!(
                "binary prepare_query: query len {} != codec dim {}",
                q.len(),
                self.dim
            )));
        }
        use nodedb_codec::vector_quant::codec::VectorCodec as _;
        let query_bits = self.codec.prepare_query(q);
        Ok(PreparedQuery::Bytes(query_bits))
    }

    /// Compute Hamming distance from a prepared query to a binary-encoded
    /// candidate.
    fn distance_prepared(
        &self,
        prepared: &PreparedQuery,
        encoded: &[u8],
    ) -> Result<f32, RerankError> {
        let q_bits = match prepared {
            PreparedQuery::Bytes(b) => b,
            _ => {
                return Err(RerankError::BadInput(
                    "binary distance: expected PreparedQuery::Bytes".to_string(),
                ));
            }
        };

        let packed_len = binary_packed_bits_len(self.dim);
        let uqv_ref = UnifiedQuantizedVectorRef::from_bytes(encoded, packed_len).map_err(|e| {
            RerankError::BadInput(format!(
                "binary distance: failed to parse encoded bytes: {e}"
            ))
        })?;

        let packed = uqv_ref.packed_bits();
        // Compute Hamming distance directly via the public helper.
        let dist = crate::quantize::binary::hamming_distance(q_bits, packed) as f32;
        Ok(dist)
    }

    fn name(&self) -> CodecName {
        CodecName::Binary
    }

    /// Serialize binary codec state.
    ///
    /// Format: `[NDBIN\0 (6 bytes)][version: u8 = 1][dim: u32 LE (4 bytes)]` — 11 bytes total.
    /// `dim` is stored so `rerank_codec_from_bytes` can reconstruct a stateless `BinaryRerank`.
    fn to_bytes(&self) -> Result<Vec<u8>, RerankError> {
        let mut buf = Vec::with_capacity(11);
        buf.extend_from_slice(b"NDBIN\0");
        buf.push(1u8); // version
        buf.extend_from_slice(&(self.dim as u32).to_le_bytes());
        Ok(buf)
    }

    // train() is the default no-op — Binary has no learned state.
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 16;

    fn all_pos() -> Vec<f32> {
        vec![1.0f32; DIM]
    }

    fn all_neg() -> Vec<f32> {
        vec![-1.0f32; DIM]
    }

    #[test]
    fn round_trip_returns_finite_distance() {
        let codec = BinaryRerank::new(DIM);
        let v1 = all_pos();
        let v2 = all_neg();

        let enc = codec.encode(&v1).expect("encode v1");
        let prepared = codec.prepare_query(&v2).expect("prepare_query v2");
        let dist = codec
            .distance_prepared(&prepared, &enc)
            .expect("distance_prepared");
        assert!(dist.is_finite(), "expected finite distance, got {dist}");
        assert!(dist >= 0.0, "expected non-negative distance, got {dist}");
    }

    #[test]
    fn opposite_vectors_have_max_distance() {
        let codec = BinaryRerank::new(DIM);
        let pos = all_pos();
        let neg = all_neg();

        let enc = codec.encode(&pos).expect("encode pos");
        let prepared = codec.prepare_query(&neg).expect("prepare_query neg");
        let dist = codec
            .distance_prepared(&prepared, &enc)
            .expect("distance_prepared");
        assert!(
            (dist - DIM as f32).abs() < f32::EPSILON,
            "opposite vectors should have Hamming distance == dim ({DIM}), got {dist}"
        );
    }

    #[test]
    fn identical_vectors_zero_distance() {
        let codec = BinaryRerank::new(DIM);
        let v = all_pos();

        let enc = codec.encode(&v).expect("encode");
        let prepared = codec.prepare_query(&v).expect("prepare_query");
        let dist = codec
            .distance_prepared(&prepared, &enc)
            .expect("distance_prepared");
        assert!(
            dist < f32::EPSILON,
            "identical vectors must have zero Hamming distance, got {dist}"
        );
    }

    #[test]
    fn wrong_prepared_query_variant_returns_bad_input() {
        let codec = BinaryRerank::new(DIM);
        let v = all_pos();
        let enc = codec.encode(&v).expect("encode");
        let bad_prepared = PreparedQuery::Raw(vec![0.0f32; DIM]);

        let result = codec.distance_prepared(&bad_prepared, &enc);
        assert!(result.is_err(), "expected BadInput error");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("Bytes"),
            "error message should mention Bytes, got: {msg}"
        );
    }

    #[test]
    fn name_returns_binary() {
        let codec = BinaryRerank::new(DIM);
        assert_eq!(codec.name(), CodecName::Binary);
    }

    #[test]
    fn wrong_dim_encode_returns_error() {
        let codec = BinaryRerank::new(DIM);
        let bad = vec![0.0f32; DIM + 1];
        assert!(codec.encode(&bad).is_err());
    }
}
