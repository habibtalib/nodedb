// SPDX-License-Identifier: Apache-2.0

use super::types::RerankError;

/// Scale `ef_search` and `oversample` up to meet a recall target.
///
/// `target_recall` must be in (0.0, 1.0]. Returns `(ef, oversample)` adjusted
/// for the given target. When `target_recall` is `None` or already-default,
/// returns the base values unchanged.
///
/// Formula (heuristic, not a guarantee):
/// - For `r <= 0.80`: identity — `ef = base_ef`, `oversample = base_oversample`.
/// - For `r > 0.80`: ramp scale from 1.0× at r=0.80 to 5.0× at r=1.00, linearly.
///   `scale = 1.0 + (r - 0.80) / 0.20 * 4.0`, clamped to `[1.0, 5.0]`.
/// - `ef` becomes `max(base_ef, (base_ef as f32 * scale).ceil() as usize)`.
/// - `oversample` becomes `max(base_oversample, (base_oversample as f32 * scale.sqrt()).ceil() as u8)`
///   — oversample grows sub-linearly because rerank cost is linear in oversample,
///   while ef has a more favourable cost curve.
pub fn recall_scale(
    target_recall: Option<f32>,
    base_ef: usize,
    base_oversample: u8,
) -> Result<(usize, u8), RerankError> {
    let r = match target_recall {
        None => return Ok((base_ef, base_oversample)),
        Some(v) => v,
    };

    if r.is_nan() || r <= 0.0 || r > 1.0 {
        return Err(RerankError::BadInput(format!(
            "target_recall must be in (0.0, 1.0], got {r}"
        )));
    }

    if r <= 0.80 {
        return Ok((base_ef, base_oversample));
    }

    let scale = (1.0_f32 + (r - 0.80) / 0.20 * 4.0).clamp(1.0, 5.0);

    let ef = base_ef.max((base_ef as f32 * scale).ceil() as usize);

    let oversample_scaled = (base_oversample as f32 * scale.sqrt()).ceil() as u32;
    let oversample = base_oversample.max(oversample_scaled.min(u8::MAX as u32) as u8);

    Ok((ef, oversample))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_returns_base() {
        assert_eq!(recall_scale(None, 100, 1).unwrap(), (100, 1));
    }

    #[test]
    fn boundary_identity_at_0_80() {
        assert_eq!(recall_scale(Some(0.80), 100, 1).unwrap(), (100, 1));
    }

    #[test]
    fn recall_0_90_scale_3() {
        // scale = 1 + (0.10/0.20)*4 = 3.0; ef = 300; oversample = ceil(sqrt(3)) = 2
        assert_eq!(recall_scale(Some(0.90), 100, 1).unwrap(), (300, 2));
    }

    #[test]
    fn recall_1_00_scale_5() {
        // scale = 5.0; ef = 500; oversample = ceil(sqrt(5)) = ceil(2.236) = 3
        assert_eq!(recall_scale(Some(1.00), 100, 1).unwrap(), (500, 3));
    }

    #[test]
    fn recall_0_95_spot_check() {
        // scale = 1 + (0.15/0.20)*4 = 4.0; ef = 800; oversample = ceil(4*sqrt(4)) = ceil(8) = 8
        assert_eq!(recall_scale(Some(0.95), 200, 4).unwrap(), (800, 8));
    }

    #[test]
    fn zero_is_bad_input() {
        assert!(matches!(
            recall_scale(Some(0.0), 100, 1),
            Err(RerankError::BadInput(_))
        ));
    }

    #[test]
    fn above_one_is_bad_input() {
        assert!(matches!(
            recall_scale(Some(1.01), 100, 1),
            Err(RerankError::BadInput(_))
        ));
    }

    #[test]
    fn nan_is_bad_input() {
        assert!(matches!(
            recall_scale(Some(f32::NAN), 100, 1),
            Err(RerankError::BadInput(_))
        ));
    }

    #[test]
    fn negative_is_bad_input() {
        assert!(matches!(
            recall_scale(Some(-0.5), 100, 1),
            Err(RerankError::BadInput(_))
        ));
    }
}
