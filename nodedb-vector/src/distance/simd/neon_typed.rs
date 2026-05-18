// SPDX-License-Identifier: Apache-2.0

#![cfg(target_arch = "aarch64")]

//! NEON distance kernels for F16 and BF16 byte buffers.
//!
//! **Stable-Rust intrinsic availability (Rust 1.95, as of this writing):**
//!
//! - BF16: `bfloat16x8_t`, `vld1q_bf16`, `vbfdotq_f32` — NOT available on stable
//!   (no stable feature gate exists; nightly-only via the `bf16` target feature).
//!   The BF16 widen path uses only base `neon`: `vmovl_u16` → `vshlq_n_u32` →
//!   `vreinterpretq_f32_u32`. This works on every aarch64 chip.
//!
//! - F16 arithmetic (`vfmaq_f16`, `vsubq_f16`, `vcvt_f32_f16`, `vreinterpret_f16_u16`):
//!   in `arm_shared` under `unstable(feature = "stdarch_arm_neon_intrinsics")` — NOT stable.
//!   `vld1q_f16` is also unstable (`stdarch_neon_f16`, issue 136306).
//!   `vcvt_high_f32_f16` is stable (since 1.94, `stdarch_neon_fp16`) but requires a
//!   `float16x8_t` input, which cannot be loaded without unstable `vld1q_f16`.
//!   Therefore the F16 NEON path falls back to a scalar widen-in-loop via
//!   `half::f16::from_le_bytes`, accumulating into base-NEON `float32x4_t` accumulators
//!   in chunks of 4. This is vectorized in the accumulation stage and correct on all cores.
//!
//! - Native FP16 arithmetic (keep math in F16 lanes): requires `vfmaq_f16` which is
//!   unstable. Not used here.
//!
//! Both paths emit `#[target_feature(enable = "neon")]` so the compiler can schedule
//! the F32 accumulator work with NEON registers even on the scalar-decode F16 path.
//!
//! Lite's primary target is mobile ARM64; the BF16 widen path is the production-hot
//! path for BF16 embeddings on all ARM chips.

// ── F16 public wrappers ────────────────────────────────────────────────────────

/// L2-squared distance between two F16-encoded byte slices (NEON, widen to F32).
pub fn l2_squared_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "neon_typed f16 l2: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "neon_typed f16 l2: b byte len mismatch");
    // SAFETY: all aarch64 targets have NEON.
    unsafe { f16_l2_impl(a, b, dim) }
}

/// Cosine distance between two F16-encoded byte slices (NEON, widen to F32).
pub fn cosine_distance_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(
        a.len(),
        dim * 2,
        "neon_typed f16 cosine: a byte len mismatch"
    );
    assert_eq!(
        b.len(),
        dim * 2,
        "neon_typed f16 cosine: b byte len mismatch"
    );
    unsafe { f16_cosine_impl(a, b, dim) }
}

/// Negative inner product between two F16-encoded byte slices (NEON, widen to F32).
pub fn neg_inner_product_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "neon_typed f16 ip: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "neon_typed f16 ip: b byte len mismatch");
    unsafe { f16_ip_impl(a, b, dim) }
}

// ── BF16 public wrappers ───────────────────────────────────────────────────────

/// L2-squared distance between two BF16-encoded byte slices (NEON widen to F32).
pub fn l2_squared_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "neon_typed bf16 l2: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "neon_typed bf16 l2: b byte len mismatch");
    unsafe { bf16_l2_impl(a, b, dim) }
}

/// Cosine distance between two BF16-encoded byte slices (NEON widen to F32).
pub fn cosine_distance_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(
        a.len(),
        dim * 2,
        "neon_typed bf16 cosine: a byte len mismatch"
    );
    assert_eq!(
        b.len(),
        dim * 2,
        "neon_typed bf16 cosine: b byte len mismatch"
    );
    unsafe { bf16_cosine_impl(a, b, dim) }
}

/// Negative inner product between two BF16-encoded byte slices (NEON widen to F32).
pub fn neg_inner_product_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "neon_typed bf16 ip: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "neon_typed bf16 ip: b byte len mismatch");
    unsafe { bf16_ip_impl(a, b, dim) }
}

// ── F16 impls — scalar decode, NEON accumulation ─────────────────────────────
//
// `vcvt_f32_f16` and `vreinterpret_f16_u16` are unstable (`stdarch_arm_neon_intrinsics`,
// issue 111800). `vld1q_f16` is unstable (`stdarch_neon_f16`, issue 136306). Therefore
// F16 elements are decoded via `half::f16::from_le_bytes` (scalar), then loaded into
// `float32x4_t` accumulators four at a time using `vld1q_f32` on a stack buffer.
// The accumulator FMA and horizontal sum are fully vectorized with NEON.

#[target_feature(enable = "neon")]
unsafe fn f16_l2_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    use std::arch::aarch64::*;
    let mut sum = vdupq_n_f32(0.0f32);
    let chunks = dim / 4;
    for i in 0..chunks {
        let base = i * 4;
        let af = decode_f16x4(a, base);
        let bf = decode_f16x4(b, base);
        let va = vld1q_f32(af.as_ptr());
        let vb = vld1q_f32(bf.as_ptr());
        let diff = vsubq_f32(va, vb);
        sum = vfmaq_f32(sum, diff, diff);
    }
    let mut result = vaddvq_f32(sum);
    for i in (chunks * 4)..dim {
        let off = i * 2;
        let av = half::f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = half::f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        let d = av - bv;
        result += d * d;
    }
    result
}

#[target_feature(enable = "neon")]
unsafe fn f16_cosine_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    use std::arch::aarch64::*;
    let mut vdot = vdupq_n_f32(0.0f32);
    let mut vna = vdupq_n_f32(0.0f32);
    let mut vnb = vdupq_n_f32(0.0f32);
    let chunks = dim / 4;
    for i in 0..chunks {
        let base = i * 4;
        let af = decode_f16x4(a, base);
        let bf = decode_f16x4(b, base);
        let va = vld1q_f32(af.as_ptr());
        let vb = vld1q_f32(bf.as_ptr());
        vdot = vfmaq_f32(vdot, va, vb);
        vna = vfmaq_f32(vna, va, va);
        vnb = vfmaq_f32(vnb, vb, vb);
    }
    let mut dot = vaddvq_f32(vdot);
    let mut na = vaddvq_f32(vna);
    let mut nb = vaddvq_f32(vnb);
    for i in (chunks * 4)..dim {
        let off = i * 2;
        let av = half::f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = half::f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
        na += av * av;
        nb += bv * bv;
    }
    let denom = (na * nb).sqrt();
    if denom < f32::EPSILON {
        1.0
    } else {
        (1.0 - dot / denom).max(0.0)
    }
}

#[target_feature(enable = "neon")]
unsafe fn f16_ip_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    use std::arch::aarch64::*;
    let mut vdot = vdupq_n_f32(0.0f32);
    let chunks = dim / 4;
    for i in 0..chunks {
        let base = i * 4;
        let af = decode_f16x4(a, base);
        let bf = decode_f16x4(b, base);
        let va = vld1q_f32(af.as_ptr());
        let vb = vld1q_f32(bf.as_ptr());
        vdot = vfmaq_f32(vdot, va, vb);
    }
    let mut dot = vaddvq_f32(vdot);
    for i in (chunks * 4)..dim {
        let off = i * 2;
        let av = half::f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = half::f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
    }
    -dot
}

/// Decode 4 consecutive F16 elements (LE bytes) starting at `base` into [f32; 4].
#[inline(always)]
fn decode_f16x4(buf: &[u8], base: usize) -> [f32; 4] {
    let off = base * 2;
    [
        half::f16::from_le_bytes([buf[off], buf[off + 1]]).to_f32(),
        half::f16::from_le_bytes([buf[off + 2], buf[off + 3]]).to_f32(),
        half::f16::from_le_bytes([buf[off + 4], buf[off + 5]]).to_f32(),
        half::f16::from_le_bytes([buf[off + 6], buf[off + 7]]).to_f32(),
    ]
}

// ── BF16 impls — NEON widen to F32 (base neon, no sub-extension) ─────────────
//
// BF16 occupies the upper 16 bits of an f32 mantissa (1 sign + 8 exp + 7 mantissa).
// Strategy: zero-extend each u16 lane to u32, shift left 16, reinterpret as f32.
// Uses only `vmovl_u16` + `vshlq_n_u32` + `vreinterpretq_f32_u32` — all stable on
// every aarch64 chip since NEON baseline (Rust 1.59).
// Processes 8 BF16 lanes per iteration (16 bytes): two float32x4_t from one load.

#[target_feature(enable = "neon")]
unsafe fn bf16_l2_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    use std::arch::aarch64::*;
    let mut sum = vdupq_n_f32(0.0f32);
    let chunks = dim / 8;
    for i in 0..chunks {
        let off = i * 16; // 8 elements × 2 bytes
        let au8 = a.as_ptr().add(off) as *const u16;
        let bu8 = b.as_ptr().add(off) as *const u16;
        let au16: uint16x8_t = vld1q_u16(au8);
        let bu16: uint16x8_t = vld1q_u16(bu8);
        let (alo, ahi) = bf16_widen_pair(au16);
        let (blo, bhi) = bf16_widen_pair(bu16);
        let diffl = vsubq_f32(alo, blo);
        let diffh = vsubq_f32(ahi, bhi);
        sum = vfmaq_f32(sum, diffl, diffl);
        sum = vfmaq_f32(sum, diffh, diffh);
    }
    let mut result = vaddvq_f32(sum);
    for i in (chunks * 8)..dim {
        let off = i * 2;
        let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        let d = av - bv;
        result += d * d;
    }
    result
}

#[target_feature(enable = "neon")]
unsafe fn bf16_cosine_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    use std::arch::aarch64::*;
    let mut vdot = vdupq_n_f32(0.0f32);
    let mut vna = vdupq_n_f32(0.0f32);
    let mut vnb = vdupq_n_f32(0.0f32);
    let chunks = dim / 8;
    for i in 0..chunks {
        let off = i * 16;
        let au16: uint16x8_t = vld1q_u16(a.as_ptr().add(off) as *const u16);
        let bu16: uint16x8_t = vld1q_u16(b.as_ptr().add(off) as *const u16);
        let (alo, ahi) = bf16_widen_pair(au16);
        let (blo, bhi) = bf16_widen_pair(bu16);
        vdot = vfmaq_f32(vdot, alo, blo);
        vdot = vfmaq_f32(vdot, ahi, bhi);
        vna = vfmaq_f32(vna, alo, alo);
        vna = vfmaq_f32(vna, ahi, ahi);
        vnb = vfmaq_f32(vnb, blo, blo);
        vnb = vfmaq_f32(vnb, bhi, bhi);
    }
    let mut dot = vaddvq_f32(vdot);
    let mut na = vaddvq_f32(vna);
    let mut nb = vaddvq_f32(vnb);
    for i in (chunks * 8)..dim {
        let off = i * 2;
        let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
        na += av * av;
        nb += bv * bv;
    }
    let denom = (na * nb).sqrt();
    if denom < f32::EPSILON {
        1.0
    } else {
        (1.0 - dot / denom).max(0.0)
    }
}

#[target_feature(enable = "neon")]
unsafe fn bf16_ip_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    use std::arch::aarch64::*;
    let mut vdot = vdupq_n_f32(0.0f32);
    let chunks = dim / 8;
    for i in 0..chunks {
        let off = i * 16;
        let au16: uint16x8_t = vld1q_u16(a.as_ptr().add(off) as *const u16);
        let bu16: uint16x8_t = vld1q_u16(b.as_ptr().add(off) as *const u16);
        let (alo, ahi) = bf16_widen_pair(au16);
        let (blo, bhi) = bf16_widen_pair(bu16);
        vdot = vfmaq_f32(vdot, alo, blo);
        vdot = vfmaq_f32(vdot, ahi, bhi);
    }
    let mut dot = vaddvq_f32(vdot);
    for i in (chunks * 8)..dim {
        let off = i * 2;
        let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
        let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
        dot += av * bv;
    }
    -dot
}

/// Widen 8 × BF16 (as `uint16x8_t`) into two `float32x4_t` (lo + hi halves).
///
/// BF16 = upper 16 bits of f32. Zero-extend u16 → u32, shift left 16, reinterpret.
/// Uses only base NEON — no sub-extension required.
#[inline(always)]
#[target_feature(enable = "neon")]
unsafe fn bf16_widen_pair(
    v: std::arch::aarch64::uint16x8_t,
) -> (
    std::arch::aarch64::float32x4_t,
    std::arch::aarch64::float32x4_t,
) {
    use std::arch::aarch64::*;
    let lo_u16: uint16x4_t = vget_low_u16(v);
    let hi_u16: uint16x4_t = vget_high_u16(v);
    let lo_u32: uint32x4_t = vmovl_u16(lo_u16);
    let hi_u32: uint32x4_t = vmovl_u16(hi_u16);
    let lo_shifted: uint32x4_t = vshlq_n_u32(lo_u32, 16);
    let hi_shifted: uint32x4_t = vshlq_n_u32(hi_u32, 16);
    (
        vreinterpretq_f32_u32(lo_shifted),
        vreinterpretq_f32_u32(hi_shifted),
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────────
//
// These tests run only on aarch64 hosts. The widen paths (both F16 and BF16)
// work on all aarch64 cores — no sub-extension detection needed. Tests compare
// NEON kernels against `typed_scalar` reference implementations within an
// absolute tolerance of 1e-4 (order-of-summation differences between NEON
// horizontal-sum and scalar accumulation may produce small floating-point deltas).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::typed_scalar;
    use crate::dtype::cast_from_f32;
    use nodedb_types::vector_dtype::VectorStorageDtype;

    const A16: [f32; 16] = [
        0.5, -1.0, 2.5, 0.1, 1.0, -0.5, 3.0, 0.2, -2.0, 1.5, 0.8, -0.3, 4.0, -1.2, 0.7, 0.9,
    ];
    const B16: [f32; 16] = [
        1.0, 0.5, -1.5, 2.0, -0.5, 1.0, -2.0, 0.3, 1.0, -1.0, 0.4, 0.6, -3.0, 0.8, -0.6, 1.1,
    ];

    const A13: [f32; 13] = [
        0.5, -1.0, 2.5, 0.1, 1.0, -0.5, 3.0, 0.2, -2.0, 1.5, 0.8, -0.3, 4.0,
    ];
    const B13: [f32; 13] = [
        1.0, 0.5, -1.5, 2.0, -0.5, 1.0, -2.0, 0.3, 1.0, -1.0, 0.4, 0.6, -3.0,
    ];

    // F16 round-trip through `half` introduces ~3 decimal digits of precision;
    // NEON horizontal-sum reordering adds at most one ULP on top.
    const EPS: f32 = 1e-4;

    #[test]
    fn f16_l2_dim16() {
        let a = cast_from_f32(&A16, VectorStorageDtype::F16);
        let b = cast_from_f32(&B16, VectorStorageDtype::F16);
        let simd = l2_squared_f16(&a, &b, 16);
        let scalar = typed_scalar::l2_squared_f16(&a, &b, 16);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 l2 dim16: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn f16_cosine_dim16() {
        let a = cast_from_f32(&A16, VectorStorageDtype::F16);
        let b = cast_from_f32(&B16, VectorStorageDtype::F16);
        let simd = cosine_distance_f16(&a, &b, 16);
        let scalar = typed_scalar::cosine_f16(&a, &b, 16);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 cosine dim16: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn f16_neg_ip_dim16() {
        let a = cast_from_f32(&A16, VectorStorageDtype::F16);
        let b = cast_from_f32(&B16, VectorStorageDtype::F16);
        let simd = neg_inner_product_f16(&a, &b, 16);
        let scalar = typed_scalar::neg_inner_product_f16(&a, &b, 16);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 ip dim16: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_l2_dim16() {
        let a = cast_from_f32(&A16, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B16, VectorStorageDtype::BF16);
        let simd = l2_squared_bf16(&a, &b, 16);
        let scalar = typed_scalar::l2_squared_bf16(&a, &b, 16);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 l2 dim16: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_cosine_dim16() {
        let a = cast_from_f32(&A16, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B16, VectorStorageDtype::BF16);
        let simd = cosine_distance_bf16(&a, &b, 16);
        let scalar = typed_scalar::cosine_bf16(&a, &b, 16);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 cosine dim16: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_neg_ip_dim16() {
        let a = cast_from_f32(&A16, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B16, VectorStorageDtype::BF16);
        let simd = neg_inner_product_bf16(&a, &b, 16);
        let scalar = typed_scalar::neg_inner_product_bf16(&a, &b, 16);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 ip dim16: simd={simd}, scalar={scalar}"
        );
    }

    // ── Tail-loop correctness: dim=13 (not a multiple of 4 or 8) ─────────────

    #[test]
    fn f16_l2_dim13_tail() {
        let a = cast_from_f32(&A13, VectorStorageDtype::F16);
        let b = cast_from_f32(&B13, VectorStorageDtype::F16);
        let simd = l2_squared_f16(&a, &b, 13);
        let scalar = typed_scalar::l2_squared_f16(&a, &b, 13);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 l2 dim13: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_l2_dim13_tail() {
        let a = cast_from_f32(&A13, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B13, VectorStorageDtype::BF16);
        let simd = l2_squared_bf16(&a, &b, 13);
        let scalar = typed_scalar::l2_squared_bf16(&a, &b, 13);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 l2 dim13: simd={simd}, scalar={scalar}"
        );
    }
}
