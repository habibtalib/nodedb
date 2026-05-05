//! Aggregation operator evaluation.

use std::collections::BTreeMap;

use super::super::ast::{AggOp, Grouping};
use super::super::error::PromqlError;
use super::super::types::*;
use super::helpers::{group_key, group_labels};

pub fn eval_aggregation(
    op: AggOp,
    val: Value,
    param: Option<f64>,
    grouping: &Grouping,
    ts: i64,
) -> Result<Value, PromqlError> {
    let Value::Vector(samples) = val else {
        return Err(PromqlError::TypeError {
            context: "aggregation".to_string(),
            detail: "requires instant vector".to_string(),
        });
    };

    let mut groups: BTreeMap<String, Vec<&InstantSample>> = BTreeMap::new();
    for s in &samples {
        let key = group_key(&s.labels, grouping);
        groups.entry(key).or_default().push(s);
    }

    let mut result = Vec::new();
    for group in groups.values() {
        let vals: Vec<f64> = group.iter().map(|s| s.value).collect();

        // Topk/bottomk return individual series, not a single aggregate.
        if matches!(op, AggOp::Topk | AggOp::Bottomk) {
            let k = param.unwrap_or(1.0) as usize;
            let mut sorted_group: Vec<&InstantSample> = group.clone();
            sorted_group.sort_by(|a, b| {
                b.value
                    .partial_cmp(&a.value)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            if matches!(op, AggOp::Bottomk) {
                sorted_group.reverse();
            }
            for s in sorted_group.into_iter().take(k) {
                result.push(InstantSample {
                    labels: group_labels(&s.labels, grouping),
                    value: s.value,
                    timestamp_ms: ts,
                });
            }
            continue;
        }

        let agg_val = compute_agg(op, &vals, param);

        let labels = group_labels(&group[0].labels, grouping);
        result.push(InstantSample {
            labels,
            value: agg_val,
            timestamp_ms: ts,
        });
    }

    Ok(Value::Vector(result))
}

fn compute_agg(op: AggOp, vals: &[f64], param: Option<f64>) -> f64 {
    match op {
        AggOp::Sum => vals.iter().sum(),
        AggOp::Avg => vals.iter().sum::<f64>() / vals.len() as f64,
        AggOp::Min => vals.iter().copied().reduce(f64::min).unwrap_or(f64::NAN),
        AggOp::Max => vals.iter().copied().reduce(f64::max).unwrap_or(f64::NAN),
        AggOp::Count => vals.len() as f64,
        AggOp::Group => 1.0,
        AggOp::Stddev => {
            let avg = vals.iter().sum::<f64>() / vals.len() as f64;
            let var = vals.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / vals.len() as f64;
            var.sqrt()
        }
        AggOp::Stdvar => {
            let avg = vals.iter().sum::<f64>() / vals.len() as f64;
            vals.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / vals.len() as f64
        }
        AggOp::Quantile => {
            let q = param.unwrap_or(0.5);
            let mut sorted = vals.to_vec();
            sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let rank = q * (sorted.len() - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = (lo + 1).min(sorted.len() - 1);
            sorted[lo] * (1.0 - (rank - lo as f64)) + sorted[hi] * (rank - lo as f64)
        }
        AggOp::CountValues => vals.len() as f64,
        AggOp::Topk | AggOp::Bottomk => f64::NAN, // handled above
    }
}
