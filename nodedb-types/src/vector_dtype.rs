// SPDX-License-Identifier: Apache-2.0

//! Vector storage precision tag.
//!
//! Selects the on-disk + in-memory dtype for vector storage on a per-collection
//! basis. Independent of quantization (see [`crate::vector_ann::VectorQuantization`]):
//! a collection can be `(F32, None)`, `(BF16, None)`, `(F32, RaBitQ)`,
//! `(BF16, RaBitQ)`, etc. Storage dtype controls the durable form; quantization
//! is an optional search-time overlay on top.

/// Vector storage dtype for HNSW + flat indexes.
///
/// `F32` is the default and the historical NodeDB storage form. `F16` and `BF16`
/// give 2x memory + disk savings with negligible recall loss for typical
/// embedding workloads, at the cost of a slightly more expensive distance kernel
/// (F16/BF16 must up-convert to F32 for arithmetic on hardware without native
/// half-precision FMA, e.g., pre-AVX-512-FP16 x86).
///
/// `FP8` (E4M3 / E5M2) is deliberately omitted from this release; it is rare in
/// vector-search workloads relative to the conversion-surface cost of supporting
/// it, and the recall hit on typical embeddings (1.5-bit-ish effective mantissa
/// precision) is severe. Reconsider when there is concrete user demand.
#[repr(u8)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
#[msgpack(c_enum)]
#[non_exhaustive]
pub enum VectorStorageDtype {
    /// 32-bit IEEE 754 single precision. Default; 4 bytes per dim.
    #[default]
    F32 = 0,
    /// 16-bit IEEE 754 half precision. 2 bytes per dim. ~3 decimal digits of
    /// precision; ~6e-5 to 65504 range.
    F16 = 1,
    /// 16-bit Brain Float (Google bfloat16). 2 bytes per dim. Same exponent
    /// range as F32 (~1e-38 to 3.4e38) but only ~7-bit mantissa. Better
    /// dynamic range than F16; preferred for embedding workloads.
    BF16 = 2,
}

impl VectorStorageDtype {
    /// Bytes occupied per vector dimension at this dtype.
    pub const fn bytes_per_dim(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 => 2,
            Self::BF16 => 2,
        }
    }

    /// Total bytes needed to store `dim`-dimensional vector in this dtype.
    pub const fn bytes_for_dim(self, dim: usize) -> usize {
        dim * self.bytes_per_dim()
    }

    /// Stable lowercase string identifier — used in DDL parsing
    /// (`WITH (storage_dtype='bf16')`) and in error messages.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F16 => "f16",
            Self::BF16 => "bf16",
        }
    }

    /// Parse from the lowercase identifier. Returns `None` for unknown values;
    /// the caller wraps that in a typed error (e.g., `NodeDbError::bad_request`)
    /// with a precise message naming the offending value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "f32" => Some(Self::F32),
            "f16" => Some(Self::F16),
            "bf16" => Some(Self::BF16),
            _ => None,
        }
    }
}

impl core::str::FromStr for VectorStorageDtype {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(())
    }
}

impl core::fmt::Display for VectorStorageDtype {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_f32() {
        assert_eq!(VectorStorageDtype::default(), VectorStorageDtype::F32);
    }

    #[test]
    fn bytes_per_dim_matches_iec_widths() {
        assert_eq!(VectorStorageDtype::F32.bytes_per_dim(), 4);
        assert_eq!(VectorStorageDtype::F16.bytes_per_dim(), 2);
        assert_eq!(VectorStorageDtype::BF16.bytes_per_dim(), 2);
    }

    #[test]
    fn bytes_for_dim_is_dim_times_width() {
        assert_eq!(VectorStorageDtype::F32.bytes_for_dim(128), 512);
        assert_eq!(VectorStorageDtype::BF16.bytes_for_dim(1536), 3072);
        assert_eq!(VectorStorageDtype::F16.bytes_for_dim(256), 512);
    }

    #[test]
    fn as_str_roundtrips_from_str() {
        for v in [
            VectorStorageDtype::F32,
            VectorStorageDtype::F16,
            VectorStorageDtype::BF16,
        ] {
            assert_eq!(VectorStorageDtype::parse(v.as_str()), Some(v));
        }
    }

    #[test]
    fn from_str_unknown_returns_none() {
        assert_eq!(VectorStorageDtype::parse("fp8"), None);
        assert_eq!(VectorStorageDtype::parse("F32"), None);
        assert_eq!(VectorStorageDtype::parse(""), None);
    }

    #[test]
    fn display_matches_as_str() {
        for v in [
            VectorStorageDtype::F32,
            VectorStorageDtype::F16,
            VectorStorageDtype::BF16,
        ] {
            assert_eq!(format!("{}", v), v.as_str());
        }
    }

    #[test]
    fn msgpack_roundtrip() {
        for v in [
            VectorStorageDtype::F32,
            VectorStorageDtype::F16,
            VectorStorageDtype::BF16,
        ] {
            let bytes = zerompk::to_msgpack_vec(&v).unwrap();
            let restored: VectorStorageDtype = zerompk::from_msgpack(&bytes).unwrap();
            assert_eq!(restored, v);
        }
    }
}
