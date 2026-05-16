// SPDX-License-Identifier: Apache-2.0

#![cfg(target_arch = "x86_64")]

//! AVX2 distance kernels for F16 (via F16C conversion) and BF16 (via
//! bit-shift widening). All math runs in F32 inside the kernel; only the
//! load + widen differs from the F32 AVX2 kernels.

// ── F16 kernels (requires AVX2 + F16C + FMA) ─────────────────────────────────

/// L2-squared distance between two F16-encoded byte slices (AVX2+F16C+FMA).
pub fn l2_squared_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx2 f16 l2: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx2 f16 l2: b byte len mismatch");
    // SAFETY: caller verified avx2+f16c+fma via is_x86_feature_detected.
    unsafe { l2_squared_f16_impl(a, b, dim) }
}

#[target_feature(enable = "avx2,fma,f16c")]
unsafe fn l2_squared_f16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut sum = _mm256_setzero_ps();
        let chunks = dim / 8;
        for i in 0..chunks {
            let off = i * 16; // 8 elements × 2 bytes each
            let a_packed = _mm_loadu_si128(a.as_ptr().add(off) as *const __m128i);
            let b_packed = _mm_loadu_si128(b.as_ptr().add(off) as *const __m128i);
            let a_f32 = _mm256_cvtph_ps(a_packed);
            let b_f32 = _mm256_cvtph_ps(b_packed);
            let diff = _mm256_sub_ps(a_f32, b_f32);
            sum = _mm256_fmadd_ps(diff, diff, sum);
        }
        let mut result = hsum256(sum);
        for i in (chunks * 8)..dim {
            let off = i * 2;
            let av = half::f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            let d = av - bv;
            result += d * d;
        }
        result
    }
}

/// Cosine distance between two F16-encoded byte slices (AVX2+F16C+FMA).
pub fn cosine_distance_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx2 f16 cosine: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx2 f16 cosine: b byte len mismatch");
    unsafe { cosine_f16_impl(a, b, dim) }
}

#[target_feature(enable = "avx2,fma,f16c")]
unsafe fn cosine_f16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm256_setzero_ps();
        let mut vna = _mm256_setzero_ps();
        let mut vnb = _mm256_setzero_ps();
        let chunks = dim / 8;
        for i in 0..chunks {
            let off = i * 16;
            let a_packed = _mm_loadu_si128(a.as_ptr().add(off) as *const __m128i);
            let b_packed = _mm_loadu_si128(b.as_ptr().add(off) as *const __m128i);
            let va = _mm256_cvtph_ps(a_packed);
            let vb = _mm256_cvtph_ps(b_packed);
            vdot = _mm256_fmadd_ps(va, vb, vdot);
            vna = _mm256_fmadd_ps(va, va, vna);
            vnb = _mm256_fmadd_ps(vb, vb, vnb);
        }
        let mut dot = hsum256(vdot);
        let mut na = hsum256(vna);
        let mut nb = hsum256(vnb);
        for i in (chunks * 8)..dim {
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

/// Negative inner product between two F16-encoded byte slices (AVX2+F16C+FMA).
pub fn neg_inner_product_f16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx2 f16 ip: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx2 f16 ip: b byte len mismatch");
    unsafe { ip_f16_impl(a, b, dim) }
}

#[target_feature(enable = "avx2,fma,f16c")]
unsafe fn ip_f16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm256_setzero_ps();
        let chunks = dim / 8;
        for i in 0..chunks {
            let off = i * 16;
            let a_packed = _mm_loadu_si128(a.as_ptr().add(off) as *const __m128i);
            let b_packed = _mm_loadu_si128(b.as_ptr().add(off) as *const __m128i);
            let va = _mm256_cvtph_ps(a_packed);
            let vb = _mm256_cvtph_ps(b_packed);
            vdot = _mm256_fmadd_ps(va, vb, vdot);
        }
        let mut dot = hsum256(vdot);
        for i in (chunks * 8)..dim {
            let off = i * 2;
            let av = half::f16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::f16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            dot += av * bv;
        }
        -dot
    }
}

// ── BF16 kernels (requires AVX2 + FMA) ────────────────────────────────────────

/// L2-squared distance between two BF16-encoded byte slices (AVX2+FMA).
pub fn l2_squared_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx2 bf16 l2: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx2 bf16 l2: b byte len mismatch");
    unsafe { l2_squared_bf16_impl(a, b, dim) }
}

#[target_feature(enable = "avx2,fma")]
unsafe fn l2_squared_bf16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut sum = _mm256_setzero_ps();
        let chunks = dim / 8;
        for i in 0..chunks {
            let off = i * 16;
            let a_packed = _mm_loadu_si128(a.as_ptr().add(off) as *const __m128i);
            let b_packed = _mm_loadu_si128(b.as_ptr().add(off) as *const __m128i);
            let a_f32 = bf16_to_f32x8(a_packed);
            let b_f32 = bf16_to_f32x8(b_packed);
            let diff = _mm256_sub_ps(a_f32, b_f32);
            sum = _mm256_fmadd_ps(diff, diff, sum);
        }
        let mut result = hsum256(sum);
        for i in (chunks * 8)..dim {
            let off = i * 2;
            let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            let d = av - bv;
            result += d * d;
        }
        result
    }
}

/// Cosine distance between two BF16-encoded byte slices (AVX2+FMA).
pub fn cosine_distance_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx2 bf16 cosine: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx2 bf16 cosine: b byte len mismatch");
    unsafe { cosine_bf16_impl(a, b, dim) }
}

#[target_feature(enable = "avx2,fma")]
unsafe fn cosine_bf16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm256_setzero_ps();
        let mut vna = _mm256_setzero_ps();
        let mut vnb = _mm256_setzero_ps();
        let chunks = dim / 8;
        for i in 0..chunks {
            let off = i * 16;
            let a_packed = _mm_loadu_si128(a.as_ptr().add(off) as *const __m128i);
            let b_packed = _mm_loadu_si128(b.as_ptr().add(off) as *const __m128i);
            let va = bf16_to_f32x8(a_packed);
            let vb = bf16_to_f32x8(b_packed);
            vdot = _mm256_fmadd_ps(va, vb, vdot);
            vna = _mm256_fmadd_ps(va, va, vna);
            vnb = _mm256_fmadd_ps(vb, vb, vnb);
        }
        let mut dot = hsum256(vdot);
        let mut na = hsum256(vna);
        let mut nb = hsum256(vnb);
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
}

/// Negative inner product between two BF16-encoded byte slices (AVX2+FMA).
pub fn neg_inner_product_bf16(a: &[u8], b: &[u8], dim: usize) -> f32 {
    assert_eq!(a.len(), dim * 2, "avx2 bf16 ip: a byte len mismatch");
    assert_eq!(b.len(), dim * 2, "avx2 bf16 ip: b byte len mismatch");
    unsafe { ip_bf16_impl(a, b, dim) }
}

#[target_feature(enable = "avx2,fma")]
unsafe fn ip_bf16_impl(a: &[u8], b: &[u8], dim: usize) -> f32 {
    unsafe {
        use std::arch::x86_64::*;
        let mut vdot = _mm256_setzero_ps();
        let chunks = dim / 8;
        for i in 0..chunks {
            let off = i * 16;
            let a_packed = _mm_loadu_si128(a.as_ptr().add(off) as *const __m128i);
            let b_packed = _mm_loadu_si128(b.as_ptr().add(off) as *const __m128i);
            let va = bf16_to_f32x8(a_packed);
            let vb = bf16_to_f32x8(b_packed);
            vdot = _mm256_fmadd_ps(va, vb, vdot);
        }
        let mut dot = hsum256(vdot);
        for i in (chunks * 8)..dim {
            let off = i * 2;
            let av = half::bf16::from_le_bytes([a[off], a[off + 1]]).to_f32();
            let bv = half::bf16::from_le_bytes([b[off], b[off + 1]]).to_f32();
            dot += av * bv;
        }
        -dot
    }
}

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Widen 8 × BF16 (LE u16 in __m128i) to 8 × f32 (__m256) via left-shift.
///
/// BF16 occupies the upper 16 bits of an f32. Zero-extend u16→u32, shift
/// left 16, reinterpret as f32.
#[target_feature(enable = "avx2")]
unsafe fn bf16_to_f32x8(v: std::arch::x86_64::__m128i) -> std::arch::x86_64::__m256 {
    use std::arch::x86_64::*;
    let u32s = _mm256_cvtepu16_epi32(v);
    let shifted = _mm256_slli_epi32(u32s, 16);
    _mm256_castsi256_ps(shifted)
}

/// Horizontal sum of 8 × f32 in a __m256.
#[target_feature(enable = "avx2")]
unsafe fn hsum256(v: std::arch::x86_64::__m256) -> f32 {
    use std::arch::x86_64::*;
    let hi = _mm256_extractf128_ps(v, 1);
    let lo = _mm256_castps256_ps128(v);
    let sum128 = _mm_add_ps(lo, hi);
    let shuf = _mm_movehdup_ps(sum128);
    let sums = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_movehl_ps(sums, sums);
    let sums2 = _mm_add_ss(sums, shuf2);
    _mm_cvtss_f32(sums2)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

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

    // Small ULP-safe margin: both paths produce identical f32 after widening;
    // horizontal-sum reordering may introduce a single-ULP difference.
    const EPS: f32 = 1e-5;

    #[test]
    fn f16_l2_dim16() {
        if !std::is_x86_feature_detected!("avx2") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
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
        if !std::is_x86_feature_detected!("avx2") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
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
        if !std::is_x86_feature_detected!("avx2") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
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
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
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
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
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
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let a = cast_from_f32(&A16, VectorStorageDtype::BF16);
        let b = cast_from_f32(&B16, VectorStorageDtype::BF16);
        let simd = neg_inner_product_bf16(&a, &b, 16);
        let scalar = typed_scalar::neg_inner_product_bf16(&a, &b, 16);
        assert!(
            (simd - scalar).abs() < EPS,
            "bf16 ip dim16: simd={simd}, scalar={scalar}"
        );
    }

    // ── Tail-loop correctness: dim=13 (not a multiple of 8) ──────────────────

    #[test]
    fn f16_l2_dim13_tail() {
        if !std::is_x86_feature_detected!("avx2") || !std::is_x86_feature_detected!("f16c") {
            return;
        }
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
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
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
