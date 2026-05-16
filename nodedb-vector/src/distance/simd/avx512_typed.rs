// SPDX-License-Identifier: Apache-2.0

#![cfg(target_arch = "x86_64")]

//! AVX-512 distance kernels for F16 and BF16 byte buffers.
//!
//! - F16 path: uses `avx512f` + `f16c` widening (16 elements per chunk).
//!   Native half-precision via `avx512fp16` (`__m512h` + `_mm512_fmadd_ph`)
//!   requires the `stdarch_x86_avx512_f16` unstable feature and is therefore
//!   not compiled on stable toolchains. The widen-to-F32 path is used instead.
//! - BF16 path: uses `avx512bf16` (`_mm512_dpbf16_ps` VDPBF16PS) when
//!   available for InnerProduct and Cosine — it accumulates dot-products
//!   directly in F32. L2 stays in the widen path even on `avx512bf16` because
//!   VDPBF16PS computes dot-product, not element-wise diff-square.
//!   Falls through to `avx512f` + bit-shift widening otherwise.
//!
//! Each kernel processes 16 elements per iteration (vs AVX2's 8). Tail loop
//! handles `dim % 16` remainder element-wise via scalar decode.

// ── F16 kernels ───────────────────────────────────────────────────────────────

/// L2-squared distance between two F16-encoded byte slices (AVX-512+F16C+FMA).
pub fn l2_squared_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx512 f16 l2: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx512 f16 l2: b byte len mismatch");
    // SAFETY: caller verified avx512f+f16c+fma via is_x86_feature_detected.
    unsafe { l2_f16_impl(a, b, dim) }
}

#[target_feature(enable = "avx512f,f16c,fma")]
unsafe fn l2_f16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut sum = _mm512_setzero_ps();
        let chunks = dim / 16;
        for i in 0..chunks {
            let off = i * 32; // 16 elements × 2 bytes each
            let a_half = _mm256_loadu_si256(a.as_ptr().add(off) as *const __m256i);
            let b_half = _mm256_loadu_si256(b.as_ptr().add(off) as *const __m256i);
            let va = _mm512_cvtph_ps(a_half);
            let vb = _mm512_cvtph_ps(b_half);
            let diff = _mm512_sub_ps(va, vb);
            sum = _mm512_fmadd_ps(diff, diff, sum);
        }
        let mut result = _mm512_reduce_add_ps(sum);
        for i in (chunks * 16)..dim {
            let off = i * 2;
            let av = half::f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            let d = av - bv;
            result += d * d;
        }
        result
    }
}

/// Cosine distance between two F16-encoded byte slices (AVX-512+F16C+FMA).
pub fn cosine_distance_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx512 f16 cosine: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx512 f16 cosine: b byte len mismatch");
    unsafe { cosine_f16_impl(a, b, dim) }
}

#[target_feature(enable = "avx512f,f16c,fma")]
unsafe fn cosine_f16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm512_setzero_ps();
        let mut vna = _mm512_setzero_ps();
        let mut vnb = _mm512_setzero_ps();
        let chunks = dim / 16;
        for i in 0..chunks {
            let off = i * 32;
            let va = _mm512_cvtph_ps(_mm256_loadu_si256(a.as_ptr().add(off) as *const __m256i));
            let vb = _mm512_cvtph_ps(_mm256_loadu_si256(b.as_ptr().add(off) as *const __m256i));
            vdot = _mm512_fmadd_ps(va, vb, vdot);
            vna = _mm512_fmadd_ps(va, va, vna);
            vnb = _mm512_fmadd_ps(vb, vb, vnb);
        }
        let mut dot = _mm512_reduce_add_ps(vdot);
        let mut na = _mm512_reduce_add_ps(vna);
        let mut nb = _mm512_reduce_add_ps(vnb);
        for i in (chunks * 16)..dim {
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
}

/// Negative inner product between two F16-encoded byte slices (AVX-512+F16C+FMA).
pub fn neg_inner_product_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx512 f16 ip: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx512 f16 ip: b byte len mismatch");
    unsafe { ip_f16_impl(a, b, dim) }
}

#[target_feature(enable = "avx512f,f16c,fma")]
unsafe fn ip_f16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm512_setzero_ps();
        let chunks = dim / 16;
        for i in 0..chunks {
            let off = i * 32;
            let va = _mm512_cvtph_ps(_mm256_loadu_si256(a.as_ptr().add(off) as *const __m256i));
            let vb = _mm512_cvtph_ps(_mm256_loadu_si256(b.as_ptr().add(off) as *const __m256i));
            vdot = _mm512_fmadd_ps(va, vb, vdot);
        }
        let mut dot = _mm512_reduce_add_ps(vdot);
        for i in (chunks * 16)..dim {
            let off = i * 2;
            let av = half::f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            dot += av * bv;
        }
        -dot
    }
}

// ── BF16 kernels ──────────────────────────────────────────────────────────────

/// L2-squared distance between two BF16-encoded byte slices (AVX-512+FMA).
///
/// Uses widen-to-F32 even when `avx512bf16` is available: `_mm512_dpbf16_ps`
/// computes dot-product, not element-wise diff-square, so it cannot be used
/// for L2 without a separate subtract step that eliminates its advantage.
pub fn l2_squared_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx512 bf16 l2: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx512 bf16 l2: b byte len mismatch");
    unsafe { l2_bf16_impl(a, b, dim) }
}

#[target_feature(enable = "avx512f,fma")]
unsafe fn l2_bf16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut sum = _mm512_setzero_ps();
        let chunks = dim / 16;
        for i in 0..chunks {
            let off = i * 32;
            let va = bf16_to_f32x16(_mm256_loadu_si256(a.as_ptr().add(off) as *const __m256i));
            let vb = bf16_to_f32x16(_mm256_loadu_si256(b.as_ptr().add(off) as *const __m256i));
            let diff = _mm512_sub_ps(va, vb);
            sum = _mm512_fmadd_ps(diff, diff, sum);
        }
        let mut result = _mm512_reduce_add_ps(sum);
        for i in (chunks * 16)..dim {
            let off = i * 2;
            let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            let d = av - bv;
            result += d * d;
        }
        result
    }
}

/// Cosine distance between two BF16-encoded byte slices.
///
/// Uses `avx512bf16` (`_mm512_dpbf16_ps`) when available: dot, a-norm, and
/// b-norm are each accumulated in a single pass with three F32 accumulators.
/// Falls through to AVX-512F + bit-shift widening otherwise.
pub fn cosine_distance_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx512 bf16 cosine: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx512 bf16 cosine: b byte len mismatch");
    if std::is_x86_feature_detected!("avx512bf16") {
        unsafe { cosine_bf16_dp_impl(a, b, dim) }
    } else {
        unsafe { cosine_bf16_widen_impl(a, b, dim) }
    }
}

/// Uses VDPBF16PS (avx512bf16): 32 BF16 values per iteration, three accumulators.
#[target_feature(enable = "avx512f,avx512bf16")]
unsafe fn cosine_bf16_dp_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm512_setzero_ps();
        let mut vna = _mm512_setzero_ps();
        let mut vnb = _mm512_setzero_ps();
        // dpbf16_ps processes 32 BF16 values per call (two packed __m512bh).
        let chunks = dim / 32;
        for i in 0..chunks {
            let off = i * 64; // 32 elements × 2 bytes
            let ra: __m512i = _mm512_loadu_si512(a.as_ptr().add(off) as *const _);
            let rb: __m512i = _mm512_loadu_si512(b.as_ptr().add(off) as *const _);
            let ba: __m512bh = std::mem::transmute(ra);
            let bb: __m512bh = std::mem::transmute(rb);
            vdot = _mm512_dpbf16_ps(vdot, ba, bb);
            vna = _mm512_dpbf16_ps(vna, ba, ba);
            vnb = _mm512_dpbf16_ps(vnb, bb, bb);
        }
        let mut dot = _mm512_reduce_add_ps(vdot);
        let mut na = _mm512_reduce_add_ps(vna);
        let mut nb = _mm512_reduce_add_ps(vnb);
        // Scalar tail for remainder elements (dim % 32).
        for i in (chunks * 32)..dim {
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
}

#[target_feature(enable = "avx512f,fma")]
unsafe fn cosine_bf16_widen_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm512_setzero_ps();
        let mut vna = _mm512_setzero_ps();
        let mut vnb = _mm512_setzero_ps();
        let chunks = dim / 16;
        for i in 0..chunks {
            let off = i * 32;
            let va = bf16_to_f32x16(_mm256_loadu_si256(a.as_ptr().add(off) as *const __m256i));
            let vb = bf16_to_f32x16(_mm256_loadu_si256(b.as_ptr().add(off) as *const __m256i));
            vdot = _mm512_fmadd_ps(va, vb, vdot);
            vna = _mm512_fmadd_ps(va, va, vna);
            vnb = _mm512_fmadd_ps(vb, vb, vnb);
        }
        let mut dot = _mm512_reduce_add_ps(vdot);
        let mut na = _mm512_reduce_add_ps(vna);
        let mut nb = _mm512_reduce_add_ps(vnb);
        for i in (chunks * 16)..dim {
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
}

/// Negative inner product between two BF16-encoded byte slices.
///
/// Uses `avx512bf16` (`_mm512_dpbf16_ps`) when available for the ideal use
/// case: accumulate `dot += a · b` directly in F32, 32 elements per iteration.
/// Falls through to AVX-512F + bit-shift widening otherwise.
pub fn neg_inner_product_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx512 bf16 ip: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx512 bf16 ip: b byte len mismatch");
    if std::is_x86_feature_detected!("avx512bf16") {
        unsafe { ip_bf16_dp_impl(a, b, dim) }
    } else {
        unsafe { ip_bf16_widen_impl(a, b, dim) }
    }
}

#[target_feature(enable = "avx512f,avx512bf16")]
unsafe fn ip_bf16_dp_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm512_setzero_ps();
        let chunks = dim / 32;
        for i in 0..chunks {
            let off = i * 64;
            let ba: __m512bh =
                std::mem::transmute(_mm512_loadu_si512(a.as_ptr().add(off) as *const _));
            let bb: __m512bh =
                std::mem::transmute(_mm512_loadu_si512(b.as_ptr().add(off) as *const _));
            vdot = _mm512_dpbf16_ps(vdot, ba, bb);
        }
        let mut dot = _mm512_reduce_add_ps(vdot);
        for i in (chunks * 32)..dim {
            let off = i * 2;
            let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            dot += av * bv;
        }
        -dot
    }
}

#[target_feature(enable = "avx512f,fma")]
unsafe fn ip_bf16_widen_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm512_setzero_ps();
        let chunks = dim / 16;
        for i in 0..chunks {
            let off = i * 32;
            let va = bf16_to_f32x16(_mm256_loadu_si256(a.as_ptr().add(off) as *const __m256i));
            let vb = bf16_to_f32x16(_mm256_loadu_si256(b.as_ptr().add(off) as *const __m256i));
            vdot = _mm512_fmadd_ps(va, vb, vdot);
        }
        let mut dot = _mm512_reduce_add_ps(vdot);
        for i in (chunks * 16)..dim {
            let off = i * 2;
            let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            dot += av * bv;
        }
        -dot
    }
}

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Widen 16 × BF16 (LE u16 in __m256i) to 16 × f32 (__m512) via left-shift.
///
/// BF16 occupies the upper 16 bits of an f32. Zero-extend u16→u32 via
/// `_mm512_cvtepu16_epi32`, shift left 16, reinterpret as f32.
#[target_feature(enable = "avx512f")]
#[inline]
unsafe fn bf16_to_f32x16(v: std::arch::x86_64::__m256i) -> std::arch::x86_64::__m512 {
    use std::arch::x86_64::*;
    let u32s = _mm512_cvtepu16_epi32(v);
    let shifted = _mm512_slli_epi32(u32s, 16);
    _mm512_castsi512_ps(shifted)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::typed_scalar;
    use crate::dtype::cast_from_f32;
    use nodedb_types::vector_dtype::VectorStorageDtype;

    const A32: [f32; 32] = [
        0.5, -1.0, 2.5, 0.1, 1.0, -0.5, 3.0, 0.2, -2.0, 1.5, 0.8, -0.3, 4.0, -1.2, 0.7, 0.9, 0.3,
        -0.8, 1.2, -1.5, 2.0, 0.6, -3.0, 0.4, 1.1, -0.9, 0.2, 2.2, -1.8, 0.5, -0.4, 1.3,
    ];
    const B32: [f32; 32] = [
        1.0, 0.5, -1.5, 2.0, -0.5, 1.0, -2.0, 0.3, 1.0, -1.0, 0.4, 0.6, -3.0, 0.8, -0.6, 1.1, -0.2,
        1.4, -0.7, 0.9, -1.3, 0.2, 2.5, -0.5, 0.8, -1.1, 1.6, -0.3, 0.7, -2.0, 0.9, 0.4,
    ];

    // dim=23: exercises both the 16-element SIMD chunk and a 7-element scalar tail.
    const A23: [f32; 23] = [
        0.5, -1.0, 2.5, 0.1, 1.0, -0.5, 3.0, 0.2, -2.0, 1.5, 0.8, -0.3, 4.0, -1.2, 0.7, 0.9, 0.3,
        -0.8, 1.2, -1.5, 2.0, 0.6, -3.0,
    ];
    const B23: [f32; 23] = [
        1.0, 0.5, -1.5, 2.0, -0.5, 1.0, -2.0, 0.3, 1.0, -1.0, 0.4, 0.6, -3.0, 0.8, -0.6, 1.1, -0.2,
        1.4, -0.7, 0.9, -1.3, 0.2, 2.5,
    ];

    // SIMD/scalar horizontal-sum reordering may introduce small floating-point differences.
    const EPS: f32 = 2e-4;

    #[test]
    fn f16_l2_dim32() {
        if !std::is_x86_feature_detected!("avx512f") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
        let a = cast_from_f32(&A32, VectorStorageDtype::F16);
        let b = cast_from_f32(&B32, VectorStorageDtype::F16);
        let simd = l2_squared_f16(&a, &b, 32);
        let scalar = typed_scalar::l2_squared_f16(&a, &b, 32);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 l2 dim32: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn f16_cosine_dim32() {
        if !std::is_x86_feature_detected!("avx512f") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
        let a = cast_from_f32(&A32, VectorStorageDtype::F16);
        let b = cast_from_f32(&B32, VectorStorageDtype::F16);
        let simd = cosine_distance_f16(&a, &b, 32);
        let scalar = typed_scalar::cosine_f16(&a, &b, 32);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 cosine dim32: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn f16_neg_ip_dim32() {
        if !std::is_x86_feature_detected!("avx512f") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
        let a = cast_from_f32(&A32, VectorStorageDtype::F16);
        let b = cast_from_f32(&B32, VectorStorageDtype::F16);
        let simd = neg_inner_product_f16(&a, &b, 32);
        let scalar = typed_scalar::neg_inner_product_f16(&a, &b, 32);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 ip dim32: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_l2_dim32() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let a = cast_from_f32(&A32, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B32, VectorStorageDtype::BF16);
        let simd = l2_squared_bf16(&a, &b, 32);
        let scalar = typed_scalar::l2_squared_bf16(&a, &b, 32);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 l2 dim32: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_cosine_dim32() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let a = cast_from_f32(&A32, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B32, VectorStorageDtype::BF16);
        let simd = cosine_distance_bf16(&a, &b, 32);
        let scalar = typed_scalar::cosine_bf16(&a, &b, 32);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 cosine dim32: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_neg_ip_dim32() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let a = cast_from_f32(&A32, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B32, VectorStorageDtype::BF16);
        let simd = neg_inner_product_bf16(&a, &b, 32);
        let scalar = typed_scalar::neg_inner_product_bf16(&a, &b, 32);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 ip dim32: simd={simd}, scalar={scalar}"
        );
    }

    // ── Tail-loop correctness: dim=23 (16-chunk + 7-element scalar tail) ─────

    #[test]
    fn f16_l2_dim23_tail() {
        if !std::is_x86_feature_detected!("avx512f") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
        let a = cast_from_f32(&A23, VectorStorageDtype::F16);
        let b = cast_from_f32(&B23, VectorStorageDtype::F16);
        let simd = l2_squared_f16(&a, &b, 23);
        let scalar = typed_scalar::l2_squared_f16(&a, &b, 23);
        assert!(
            (simd - scalar).abs() < EPS,
            "f16 l2 dim23: simd={simd}, scalar={scalar}"
        );
    }

    #[test]
    fn bf16_l2_dim23_tail() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let a = cast_from_f32(&A23, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B23, VectorStorageDtype::BF16);
        let simd = l2_squared_bf16(&a, &b, 23);
        let scalar = typed_scalar::l2_squared_bf16(&a, &b, 23);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 l2 dim23: simd={simd}, scalar={scalar}"
        );
    }
}
