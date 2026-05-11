// SPDX-License-Identifier: BUSL-1.1

//! `merge_from` implementations for `AggAccum` and `GroupState`.

use super::state::{ARRAY_AGG_CAP, AggAccum, GroupState};

impl AggAccum {
    /// Merge a partial accumulator `other` into `self` (used by tests).
    #[cfg(test)]
    pub(crate) fn merge_from(&mut self, other: AggAccum) {
        merge_accum(self, other);
    }
}

/// Merge the partial accumulator `other` (from a spilled run) into `dst`.
///
/// Both `dst` and `other` must be the same variant — they always come from
/// the same aggregate-spec position in the same query.  A variant mismatch
/// is an internal programming error and will panic via `unreachable!`.
pub(super) fn merge_accum(dst: &mut AggAccum, other: AggAccum) {
    match (dst, other) {
        (AggAccum::Count { n: a }, AggAccum::Count { n: b }) => {
            *a += b;
        }
        (
            AggAccum::SumAvg {
                sum: sa,
                comp: ca,
                n: na,
            },
            AggAccum::SumAvg {
                sum: sb,
                comp: _cb,
                n: nb,
            },
        ) => {
            // Kahan-compensated addition of the other partial sum into self.
            let y = sb - *ca;
            let t = *sa + y;
            *ca = (t - *sa) - y;
            *sa = t;
            *na += nb;
        }
        (AggAccum::SumAvgDistinct { seen: a }, AggAccum::SumAvgDistinct { seen: b }) => {
            // Union the deduped value maps; the first-seen parsed f64 wins
            // (all instances of the same key carry the same value, so the
            // choice is immaterial). The sum is re-derived at finalize.
            for (key, value) in b {
                a.entry(key).or_insert(value);
            }
        }
        (AggAccum::Min { best: a }, AggAccum::Min { best: b }) => {
            if let Some(bv) = b {
                let replace = match a {
                    None => true,
                    Some(av) => {
                        nodedb_query::value_ops::compare_values(&bv, av) == std::cmp::Ordering::Less
                    }
                };
                if replace {
                    *a = Some(bv);
                }
            }
        }
        (AggAccum::Max { best: a }, AggAccum::Max { best: b }) => {
            if let Some(bv) = b {
                let replace = match a {
                    None => true,
                    Some(av) => {
                        nodedb_query::value_ops::compare_values(&bv, av)
                            == std::cmp::Ordering::Greater
                    }
                };
                if replace {
                    *a = Some(bv);
                }
            }
        }
        (AggAccum::CountDistinct { seen: a }, AggAccum::CountDistinct { seen: b }) => {
            a.extend(b);
        }
        (
            AggAccum::Welford {
                n: na,
                mean: ma,
                m2: m2a,
            },
            AggAccum::Welford {
                n: nb,
                mean: mb,
                m2: m2b,
            },
        ) => {
            // Parallel Welford merge formula.
            let n_new = *na + nb;
            if n_new == 0 {
                return;
            }
            let delta = mb - *ma;
            let mean_new = *ma + delta * (nb as f64 / n_new as f64);
            let m2_new = *m2a + m2b + delta * delta * (*na as f64) * (nb as f64) / n_new as f64;
            *na = n_new;
            *ma = mean_new;
            *m2a = m2_new;
        }
        (AggAccum::Hll { hll: a }, AggAccum::Hll { hll: b }) => {
            a.merge(&b);
        }
        (AggAccum::TDigest { digest: a }, AggAccum::TDigest { digest: b }) => {
            a.merge(&b);
        }
        (AggAccum::TopK { ss: a, .. }, AggAccum::TopK { ss: b, .. }) => {
            a.merge(&b);
        }
        (AggAccum::ArrayAgg { values: a }, AggAccum::ArrayAgg { values: b }) => {
            let remaining = ARRAY_AGG_CAP.saturating_sub(a.len());
            a.extend(b.into_iter().take(remaining));
        }
        (
            AggAccum::ArrayAggDistinct {
                seen: sa,
                values: va,
            },
            AggAccum::ArrayAggDistinct {
                seen: sb,
                values: vb,
            },
        ) => {
            for (bytes_key, value) in sb.into_iter().zip(vb) {
                if va.len() >= ARRAY_AGG_CAP {
                    break;
                }
                if sa.insert(bytes_key) {
                    va.push(value);
                }
            }
        }
        (
            AggAccum::PercentileCont { values: a, .. },
            AggAccum::PercentileCont { values: b, .. },
        ) => {
            let remaining = ARRAY_AGG_CAP.saturating_sub(a.len());
            a.extend(b.into_iter().take(remaining));
        }
        (AggAccum::StringAgg { parts: a }, AggAccum::StringAgg { parts: b }) => {
            let remaining = ARRAY_AGG_CAP.saturating_sub(a.len());
            a.extend(b.into_iter().take(remaining));
        }
        _ => {
            // Invariant: same query → same aggregate-spec position → same variant.
            unreachable!(
                "AggAccum::merge_from: variant mismatch — \
                 both operands must be the same variant (same aggregate spec, same query)"
            );
        }
    }
}

/// Merge all accumulators from `other` into `dst` element-wise.
pub(super) fn merge_group_state(dst: &mut GroupState, other: GroupState) {
    assert_eq!(
        dst.accums.len(),
        other.accums.len(),
        "GroupState::merge_from: accum count mismatch — \
         both GroupState values must come from the same aggregate spec list"
    );
    for (a, b) in dst.accums.iter_mut().zip(other.accums) {
        merge_accum(a, b);
    }
}
