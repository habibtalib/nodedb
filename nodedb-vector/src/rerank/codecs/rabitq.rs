// SPDX-License-Identifier: Apache-2.0

//! `RerankCodec` wrapper for RaBitQ 1-bit quantization.
//!
//! RaBitQ is a training-based codec: `train()` runs centroid calibration and
//! stores a randomised WHT rotation. Until training is complete, `encode` and
//! `prepare_query` return `RerankError::NotTrained`.
//!
//! Distance is computed by inlining the asymmetric Hamming-based formula from
//! `RaBitQCodec::exact_asymmetric_distance`:
//!
//!   approx_l2 = q_norm² + v_norm² − 2 · q_norm · v_norm · (1 − 2·hamming/dim)
//!
//! The prepared form is `PreparedQuery::Bytes` with the layout:
//!   [0..4]   query_norm as f32 little-endian
//!   [4..]    rotated_signs bytes (length = dim.div_ceil(8))

use nodedb_codec::vector_quant::codec::VectorCodec as _;
use nodedb_codec::vector_quant::hamming::hamming_distance;
use nodedb_codec::vector_quant::layout::UnifiedQuantizedVectorRef;
use nodedb_codec::vector_quant::rabitq::{RaBitQCodec, RaBitQQuery};

use crate::{
    rerank::codec::{CodecName, PreparedQuery, RerankCodec},
    rerank::types::RerankError,
};

// ── Payload helpers ───────────────────────────────────────────────────────────

fn encode_payload(query: &RaBitQQuery) -> Vec<u8> {
    // Layout: 4 bytes query_norm (f32 LE) || rotated_signs bytes
    let mut buf = Vec::with_capacity(4 + query.rotated_signs.len());
    buf.extend_from_slice(&query.query_norm.to_le_bytes());
    buf.extend_from_slice(&query.rotated_signs);
    buf
}

fn decode_payload(payload: &[u8], dim: usize) -> Result<(f32, Vec<u8>), RerankError> {
    let sign_len = dim.div_ceil(8);
    let expected = 4 + sign_len;
    if payload.len() != expected {
        return Err(RerankError::BadInput(format!(
            "rabitq distance: payload len {} != expected {} for dim {}",
            payload.len(),
            expected,
            dim
        )));
    }
    let query_norm = f32::from_le_bytes(
        payload[..4]
            .try_into()
            .expect("slice of 4 bytes always converts to [u8;4]"),
    );
    Ok((query_norm, payload[4..].to_vec()))
}

// ── RaBitQRerank ──────────────────────────────────────────────────────────────

/// Default rotation seed used when the caller does not specify one.
pub const DEFAULT_ROTATION_SEED: u64 = 0x00C0_FFEE_00C0_FFEE;

/// Object-safe `RerankCodec` wrapper around `RaBitQCodec`.
///
/// The codec starts untrained. `encode` and `prepare_query` return
/// `RerankError::NotTrained` until `train()` has been called with a
/// representative sample of vectors.
///
/// `from_codec` accepts a pre-calibrated `RaBitQCodec` (used when restoring
/// from a snapshot).
pub struct RaBitQRerank {
    codec: Option<RaBitQCodec>,
    dim: usize,
    rotation_seed: u64,
}

impl RaBitQRerank {
    /// Construct an untrained wrapper.
    ///
    /// `encode` / `distance_prepared` return `RerankError::NotTrained` until
    /// `train()` is called.
    pub fn new(dim: usize, rotation_seed: u64) -> Self {
        Self {
            codec: None,
            dim,
            rotation_seed,
        }
    }

    /// Construct from a pre-calibrated codec (used when restoring from snapshot).
    pub fn from_codec(codec: RaBitQCodec) -> Self {
        let dim = codec.dim;
        Self {
            codec: Some(codec),
            dim,
            rotation_seed: DEFAULT_ROTATION_SEED,
        }
    }
}

impl RerankCodec for RaBitQRerank {
    /// Encode a full-precision vector to RaBitQ 1-bit bytes.
    ///
    /// The serialised form is the raw `UnifiedQuantizedVector` buffer
    /// (`as_bytes()`): 32-byte `QuantHeader` followed by `dim.div_ceil(8)`
    /// sign-packed bits.
    fn encode(&self, v: &[f32]) -> Result<Vec<u8>, RerankError> {
        if v.len() != self.dim {
            return Err(RerankError::BadInput(format!(
                "rabitq encode: vector len {} != codec dim {}",
                v.len(),
                self.dim
            )));
        }
        let codec = self.codec.as_ref().ok_or_else(|| {
            RerankError::NotTrained(
                "rabitq: codec must be trained before encoding (call train() with a sample of vectors)"
                    .to_string(),
            )
        })?;
        let quantized = codec.encode(v);
        Ok(quantized.as_ref().as_bytes().to_vec())
    }

    /// Prepare the query by computing its centroid-subtracted, rotated sign pack
    /// and the exact query norm.
    ///
    /// The prepared form is `PreparedQuery::Bytes` with the layout:
    ///   4 bytes query_norm (f32 LE) || sign bytes (dim.div_ceil(8)).
    fn prepare_query(&self, q: &[f32]) -> Result<PreparedQuery, RerankError> {
        if q.len() != self.dim {
            return Err(RerankError::BadInput(format!(
                "rabitq prepare_query: query len {} != codec dim {}",
                q.len(),
                self.dim
            )));
        }
        let codec = self.codec.as_ref().ok_or_else(|| {
            RerankError::NotTrained(
                "rabitq: codec must be trained before prepare_query (call train() with a sample of vectors)"
                    .to_string(),
            )
        })?;
        let query = codec.prepare_query(q);
        Ok(PreparedQuery::Bytes(encode_payload(&query)))
    }

    /// Compute asymmetric Hamming-based L2 distance from a prepared query to a
    /// RaBitQ-encoded candidate.
    ///
    /// Inlines `RaBitQCodec::exact_asymmetric_distance` (without bias_correct)
    /// using `UnifiedQuantizedVectorRef` to avoid a redundant allocation:
    ///
    ///   approx = q_norm² + v_norm² − 2·q_norm·v_norm·(1 − 2·hamming/dim)
    ///
    /// Expects `PreparedQuery::Bytes` produced by `prepare_query`.
    fn distance_prepared(
        &self,
        prepared: &PreparedQuery,
        encoded: &[u8],
    ) -> Result<f32, RerankError> {
        let payload = match prepared {
            PreparedQuery::Bytes(b) => b.as_slice(),
            _ => {
                return Err(RerankError::BadInput(
                    "rabitq distance: prepared query is not Bytes".to_string(),
                ));
            }
        };

        let (query_norm, rotated_signs) = decode_payload(payload, self.dim)?;

        let packed_len = self.dim.div_ceil(8);
        let uqv_ref = UnifiedQuantizedVectorRef::from_bytes(encoded, packed_len).map_err(|e| {
            RerankError::BadInput(format!(
                "rabitq distance: failed to parse encoded bytes: {e}"
            ))
        })?;

        let vh = uqv_ref.header();
        let vb = uqv_ref.packed_bits();
        let h = hamming_distance(&rotated_signs, vb);
        let dim = self.dim as f32;
        let dot_estimate = 1.0 - 2.0 * h as f32 / dim;
        let approx = query_norm * query_norm + vh.residual_norm * vh.residual_norm
            - 2.0 * query_norm * vh.residual_norm * dot_estimate;
        Ok(approx.max(0.0))
    }

    fn name(&self) -> CodecName {
        CodecName::RaBitQ
    }

    fn to_bytes(&self) -> Result<Vec<u8>, RerankError> {
        let codec = self.codec.as_ref().ok_or_else(|| {
            RerankError::NotTrained("rabitq sidecar serialize: codec not trained".to_string())
        })?;
        codec
            .to_bytes()
            .map_err(|e| RerankError::BadInput(format!("rabitq to_bytes: {e}")))
    }

    /// Calibrate from a sample of vectors.
    ///
    /// Validates that:
    /// - `samples` is non-empty.
    /// - Every sample has length `self.dim`.
    ///
    /// On success, stores the calibrated codec; subsequent `encode` /
    /// `distance_prepared` calls will succeed.
    fn train(&mut self, samples: &[&[f32]]) -> Result<(), RerankError> {
        if samples.is_empty() {
            return Err(RerankError::BadInput(
                "rabitq train: empty sample set".to_string(),
            ));
        }
        for s in samples {
            if s.len() != self.dim {
                return Err(RerankError::BadInput(format!(
                    "rabitq train: sample has len {} but codec dim is {}",
                    s.len(),
                    self.dim
                )));
            }
        }
        let codec = RaBitQCodec::calibrate(samples, self.dim, self.rotation_seed);
        self.codec = Some(codec);
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 16;
    const N: usize = 64;

    fn det_vec(i: usize, dim: usize) -> Vec<f32> {
        (0..dim)
            .map(|j| ((i * 31 + j) % 100) as f32 / 100.0)
            .collect()
    }

    fn trained() -> RaBitQRerank {
        let vecs: Vec<Vec<f32>> = (0..N).map(|i| det_vec(i, DIM)).collect();
        let refs: Vec<&[f32]> = vecs.iter().map(|v| v.as_slice()).collect();
        let mut codec = RaBitQRerank::new(DIM, DEFAULT_ROTATION_SEED);
        codec.train(&refs).expect("train must succeed");
        codec
    }

    #[test]
    fn train_then_encode_roundtrip() {
        let codec = trained();
        let v = det_vec(0, DIM);
        let enc = codec.encode(&v).expect("encode");
        let prep = codec.prepare_query(&v).expect("prepare_query");
        let dist = codec.distance_prepared(&prep, &enc).expect("distance");
        assert!(dist.is_finite(), "distance must be finite, got {dist}");
        assert!(dist >= 0.0, "distance must be non-negative, got {dist}");
    }

    #[test]
    fn encode_before_train_returns_not_trained() {
        let codec = RaBitQRerank::new(DIM, DEFAULT_ROTATION_SEED);
        let v = det_vec(0, DIM);
        let err = codec.encode(&v).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not trained") || msg.contains("trained"),
            "expected 'trained' in error, got: {msg}"
        );
    }

    #[test]
    fn train_with_empty_samples_fails() {
        let mut codec = RaBitQRerank::new(DIM, DEFAULT_ROTATION_SEED);
        let err = codec.train(&[]).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("bad input") || msg.contains("empty"),
            "expected bad input error, got: {msg}"
        );
    }

    #[test]
    fn train_with_dim_mismatch_fails() {
        let vecs: Vec<Vec<f32>> = (0..N).map(|i| det_vec(i, DIM)).collect();
        let mut refs: Vec<&[f32]> = vecs.iter().map(|v| v.as_slice()).collect();
        let bad = det_vec(0, DIM + 4);
        refs.push(bad.as_slice());
        let mut codec = RaBitQRerank::new(DIM, DEFAULT_ROTATION_SEED);
        let err = codec.train(&refs).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("bad input") || msg.contains("dim"),
            "expected bad input error, got: {msg}"
        );
    }

    #[test]
    fn prepare_query_wrong_dim_fails() {
        let codec = trained();
        let bad = det_vec(0, DIM + 2);
        match codec.prepare_query(&bad) {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("bad input") || msg.contains("dim"),
                    "expected bad input error, got: {msg}"
                );
            }
            Ok(_) => panic!("expected an error for wrong dim"),
        }
    }

    #[test]
    fn distance_prepared_wrong_variant_fails() {
        let codec = trained();
        let v = det_vec(0, DIM);
        let enc = codec.encode(&v).expect("encode");
        let bad_prepared = PreparedQuery::Raw(vec![0.0f32; DIM]);
        let err = codec.distance_prepared(&bad_prepared, &enc).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Bytes") || msg.contains("not Bytes"),
            "error message should mention Bytes variant, got: {msg}"
        );
    }

    #[test]
    fn name_is_expected() {
        let codec = RaBitQRerank::new(DIM, DEFAULT_ROTATION_SEED);
        assert_eq!(codec.name(), CodecName::RaBitQ);
    }
}
