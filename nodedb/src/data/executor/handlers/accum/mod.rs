//! Streaming aggregate accumulators for the generic GROUP BY path.
//!
//! Each `AggAccum` variant holds only the derived state needed to compute the
//! final aggregate result — no raw document bytes are retained.  Memory per
//! group is O(num_aggregates × accumulator_size) regardless of how many
//! documents match that group.

mod merge;

use std::collections::HashSet;

use crate::bridge::physical_plan::AggregateSpec;
use nodedb_types::Value;

/// Maximum items collected by materializing aggregates (`array_agg`,
/// `array_agg_distinct`, `percentile_cont`, `string_agg`).
pub(super) const ARRAY_AGG_CAP: usize = 10_000;

/// Per-(group, aggregate-spec) running accumulator.
///
/// Derives `Serialize` / `Deserialize` so that partial states can be spilled
/// to disk by `GroupBySpiller` and merged back during finalize.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) enum AggAccum {
    /// count(*) or count(field).
    Count { n: u64 },
    /// sum / avg: Kahan-compensated running sum + count.
    SumAvg { sum: f64, comp: f64, n: u64 },
    /// min.
    Min { best: Option<Value> },
    /// max.
    Max { best: Option<Value> },
    /// count_distinct: set of raw msgpack bytes.
    CountDistinct { seen: HashSet<Vec<u8>> },
    /// stddev / variance variants: Welford M2 accumulator.
    Welford { n: u64, mean: f64, m2: f64 },
    /// approx_count_distinct: HyperLogLog.
    Hll {
        hll: nodedb_types::approx::HyperLogLog,
    },
    /// approx_percentile: t-digest.
    TDigest {
        digest: nodedb_types::approx::TDigest,
    },
    /// approx_topk: space-saving.
    TopK {
        ss: nodedb_types::approx::SpaceSaving,
        k: usize,
    },
    /// array_agg (capped).
    ArrayAgg { values: Vec<Value> },
    /// array_agg_distinct (capped).
    ArrayAggDistinct {
        seen: HashSet<Vec<u8>>,
        values: Vec<Value>,
    },
    /// percentile_cont (capped).
    PercentileCont { values: Vec<f64>, pct: f64 },
    /// string_agg / group_concat (capped).
    StringAgg { parts: Vec<String> },
}

impl AggAccum {
    pub(super) fn new(agg: &AggregateSpec) -> Self {
        match agg.function.as_str() {
            "count" => AggAccum::Count { n: 0 },
            "sum" | "avg" => AggAccum::SumAvg {
                sum: 0.0,
                comp: 0.0,
                n: 0,
            },
            "min" => AggAccum::Min { best: None },
            "max" => AggAccum::Max { best: None },
            "count_distinct" => AggAccum::CountDistinct {
                seen: HashSet::new(),
            },
            "stddev" | "stddev_pop" | "stddev_samp" | "variance" | "var_pop" | "var_samp" => {
                AggAccum::Welford {
                    n: 0,
                    mean: 0.0,
                    m2: 0.0,
                }
            }
            "approx_count_distinct" => AggAccum::Hll {
                hll: nodedb_types::approx::HyperLogLog::new(),
            },
            "approx_percentile" => AggAccum::TDigest {
                digest: nodedb_types::approx::TDigest::new(),
            },
            "approx_topk" => {
                let k: usize = agg
                    .field
                    .find(':')
                    .and_then(|i| agg.field[..i].parse().ok())
                    .unwrap_or(10);
                AggAccum::TopK {
                    ss: nodedb_types::approx::SpaceSaving::new(k),
                    k,
                }
            }
            "array_agg" => AggAccum::ArrayAgg { values: Vec::new() },
            "array_agg_distinct" => AggAccum::ArrayAggDistinct {
                seen: HashSet::new(),
                values: Vec::new(),
            },
            "percentile_cont" => {
                let pct = agg
                    .field
                    .find(':')
                    .and_then(|i| agg.field[..i].parse().ok())
                    .unwrap_or(0.5);
                AggAccum::PercentileCont {
                    values: Vec::new(),
                    pct,
                }
            }
            "string_agg" | "group_concat" => AggAccum::StringAgg { parts: Vec::new() },
            _ => AggAccum::Count { n: 0 },
        }
    }

    /// Feed one document into this accumulator.
    pub(super) fn feed(&mut self, agg: &AggregateSpec, doc: &[u8]) {
        use nodedb_query::msgpack_scan::aggregate_helpers as ah;
        match self {
            AggAccum::Count { n } => {
                if (agg.field == "*" && agg.expr.is_none())
                    || ah::extract_non_null(doc, &agg.field, agg.expr.as_ref()).is_some()
                {
                    *n += 1;
                }
            }
            AggAccum::SumAvg { sum, comp, n } => {
                if let Some(v) = ah::extract_f64(doc, &agg.field, agg.expr.as_ref()) {
                    let y = v - *comp;
                    let t = *sum + y;
                    *comp = (t - *sum) - y;
                    *sum = t;
                    *n += 1;
                }
            }
            AggAccum::Min { best } => {
                if let Some(v) = ah::extract_value(doc, &agg.field, agg.expr.as_ref()) {
                    if v.is_null() {
                        return;
                    }
                    let replace = match best {
                        None => true,
                        Some(cur) => {
                            nodedb_query::value_ops::compare_values(&v, cur)
                                == std::cmp::Ordering::Less
                        }
                    };
                    if replace {
                        *best = Some(v);
                    }
                }
            }
            AggAccum::Max { best } => {
                if let Some(v) = ah::extract_value(doc, &agg.field, agg.expr.as_ref()) {
                    if v.is_null() {
                        return;
                    }
                    let replace = match best {
                        None => true,
                        Some(cur) => {
                            nodedb_query::value_ops::compare_values(&v, cur)
                                == std::cmp::Ordering::Greater
                        }
                    };
                    if replace {
                        *best = Some(v);
                    }
                }
            }
            AggAccum::CountDistinct { seen } => {
                if let Some(bytes) = ah::extract_bytes(doc, &agg.field, agg.expr.as_ref())
                    && bytes != [0xc0u8]
                {
                    seen.insert(bytes);
                }
            }
            AggAccum::Welford { n, mean, m2 } => {
                if let Some(v) = ah::extract_f64(doc, &agg.field, agg.expr.as_ref()) {
                    *n += 1;
                    let delta = v - *mean;
                    *mean += delta / *n as f64;
                    let delta2 = v - *mean;
                    *m2 += delta * delta2;
                }
            }
            AggAccum::Hll { hll } => {
                if let Some(bytes) = ah::extract_bytes(doc, &agg.field, agg.expr.as_ref())
                    && bytes != [0xc0u8]
                {
                    hll.add(fnv1a(&bytes));
                }
            }
            AggAccum::TDigest { digest } => {
                let actual = field_after_colon(&agg.field);
                if let Some(v) = ah::extract_f64(doc, actual, agg.expr.as_ref()) {
                    digest.add(v);
                }
            }
            AggAccum::TopK { ss, .. } => {
                let actual = field_after_colon(&agg.field);
                if let Some(bytes) = ah::extract_bytes(doc, actual, agg.expr.as_ref())
                    && bytes != [0xc0u8]
                {
                    ss.add(fnv1a(&bytes));
                }
            }
            AggAccum::ArrayAgg { values } => {
                if values.len() < ARRAY_AGG_CAP
                    && let Some(v) = ah::extract_value(doc, &agg.field, agg.expr.as_ref())
                    && !v.is_null()
                {
                    values.push(v);
                }
            }
            AggAccum::ArrayAggDistinct { seen, values } => {
                if values.len() < ARRAY_AGG_CAP
                    && let Some(bytes) = ah::extract_bytes(doc, &agg.field, agg.expr.as_ref())
                    && bytes != [0xc0u8]
                    && seen.insert(bytes)
                    && let Some(v) = ah::extract_value(doc, &agg.field, agg.expr.as_ref())
                {
                    values.push(v);
                }
            }
            AggAccum::PercentileCont { values, .. } => {
                let actual = field_after_colon(&agg.field);
                if values.len() < ARRAY_AGG_CAP
                    && let Some(v) = ah::extract_f64(doc, actual, agg.expr.as_ref())
                {
                    values.push(v);
                }
            }
            AggAccum::StringAgg { parts } => {
                if parts.len() < ARRAY_AGG_CAP
                    && let Some(s) = ah::extract_str(doc, &agg.field, agg.expr.as_ref())
                {
                    parts.push(s);
                }
            }
        }
    }

    /// Merge a partial accumulator `other` into `self` (used by tests).
    #[cfg(test)]
    pub(super) fn merge_from(&mut self, other: AggAccum) {
        merge::merge_accum(self, other);
    }

    /// Consume the accumulator and produce the final `Value`.
    pub(super) fn finalize(self, agg: &AggregateSpec) -> Value {
        match self {
            AggAccum::Count { n } => Value::Integer(n as i64),
            AggAccum::SumAvg { sum, n, .. } => {
                if agg.function == "avg" {
                    if n == 0 {
                        Value::Null
                    } else {
                        Value::Float(sum / n as f64)
                    }
                } else {
                    Value::Float(sum)
                }
            }
            AggAccum::Min { best } => best.unwrap_or(Value::Null),
            AggAccum::Max { best } => best.unwrap_or(Value::Null),
            AggAccum::CountDistinct { seen } => Value::Integer(seen.len() as i64),
            AggAccum::Welford { n, mean: _, m2 } => {
                if n < 2 {
                    return Value::Null;
                }
                let population = matches!(
                    agg.function.as_str(),
                    "stddev" | "stddev_pop" | "variance" | "var_pop"
                );
                let divisor = if population { n as f64 } else { (n - 1) as f64 };
                let variance = m2 / divisor;
                let result = if agg.function.contains("stddev") {
                    variance.sqrt()
                } else {
                    variance
                };
                Value::Float(result)
            }
            AggAccum::Hll { hll } => Value::Integer(hll.estimate().round() as i64),
            AggAccum::TDigest { digest } => {
                let pct = agg
                    .field
                    .find(':')
                    .and_then(|i| agg.field[..i].parse().ok())
                    .unwrap_or(0.5);
                let r = digest.quantile(pct);
                if r.is_nan() {
                    Value::Null
                } else {
                    Value::Float(r)
                }
            }
            AggAccum::TopK { ss, k } => {
                let arr: Vec<Value> = ss
                    .top_k()
                    .into_iter()
                    .take(k)
                    .map(|(item, count, error)| {
                        Value::Object(
                            [
                                ("item".to_string(), Value::Integer(item as i64)),
                                ("count".to_string(), Value::Integer(count as i64)),
                                ("error".to_string(), Value::Integer(error as i64)),
                            ]
                            .into_iter()
                            .collect(),
                        )
                    })
                    .collect();
                Value::Array(arr)
            }
            AggAccum::ArrayAgg { values } => Value::Array(values),
            AggAccum::ArrayAggDistinct { values, .. } => Value::Array(values),
            AggAccum::PercentileCont { mut values, pct } => {
                if values.is_empty() {
                    return Value::Null;
                }
                values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let idx = (pct * (values.len() - 1) as f64).clamp(0.0, (values.len() - 1) as f64);
                let lo = idx.floor() as usize;
                let hi = idx.ceil() as usize;
                let frac = idx - lo as f64;
                Value::Float(values[lo] * (1.0 - frac) + values[hi] * frac)
            }
            AggAccum::StringAgg { parts } => Value::String(parts.join(",")),
        }
    }
}

/// Per-group running state: one `AggAccum` per aggregate spec.
///
/// Serializable so that `GroupBySpiller` can spill partial states to disk.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct GroupState {
    pub(super) accums: Vec<AggAccum>,
}

impl GroupState {
    pub(super) fn new(aggregates: &[AggregateSpec]) -> Self {
        Self {
            accums: aggregates.iter().map(AggAccum::new).collect(),
        }
    }

    pub(super) fn feed(&mut self, aggregates: &[AggregateSpec], doc: &[u8]) {
        for (accum, agg) in self.accums.iter_mut().zip(aggregates) {
            accum.feed(agg, doc);
        }
    }

    /// Merge a partial `GroupState` from a spilled run into `self`.
    ///
    /// Delegates to `merge::merge_group_state`.
    pub(super) fn merge_from(&mut self, other: GroupState) {
        merge::merge_group_state(self, other);
    }

    pub(super) fn finalize(self, aggregates: &[AggregateSpec]) -> Vec<(String, Value)> {
        self.accums
            .into_iter()
            .zip(aggregates)
            .map(|(accum, agg)| (agg.alias.clone(), accum.finalize(agg)))
            .collect()
    }
}

/// FNV-1a hash (matches the implementation in nodedb-query aggregate.rs).
#[inline]
pub(super) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Extract the actual field name from "prefix:field" format (e.g. "0.95:latency").
#[inline]
pub(super) fn field_after_colon(field: &str) -> &str {
    field.find(':').map(|i| &field[i + 1..]).unwrap_or(field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::physical_plan::AggregateSpec;

    fn make_spec(func: &str, field: &str) -> AggregateSpec {
        AggregateSpec {
            function: func.to_string(),
            field: field.to_string(),
            alias: format!("{func}({field})"),
            user_alias: None,
            expr: None,
        }
    }

    /// Build a minimal bare-msgpack doc with one integer field.
    ///
    /// Uses `value_to_msgpack` (not `zerompk::to_msgpack_vec`) so the output
    /// is a standard msgpack map that `extract_field` / `map_header` can scan.
    fn make_doc_i64(field: &str, value: i64) -> Vec<u8> {
        use nodedb_types::Value;
        let mut map = std::collections::HashMap::new();
        map.insert(field.to_string(), Value::Integer(value));
        nodedb_types::value_to_msgpack(&Value::Object(map)).expect("encode doc")
    }

    fn make_doc_f64(field: &str, value: f64) -> Vec<u8> {
        use nodedb_types::Value;
        let mut map = std::collections::HashMap::new();
        map.insert(field.to_string(), Value::Float(value));
        nodedb_types::value_to_msgpack(&Value::Object(map)).expect("encode doc")
    }

    fn make_doc_str(field: &str, value: &str) -> Vec<u8> {
        use nodedb_types::Value;
        let mut map = std::collections::HashMap::new();
        map.insert(field.to_string(), Value::String(value.to_string()));
        nodedb_types::value_to_msgpack(&Value::Object(map)).expect("encode doc")
    }

    #[test]
    fn merge_from_count() {
        let spec = make_spec("count", "*");
        let docs_a: Vec<Vec<u8>> = (0..5).map(|_| make_doc_i64("x", 1)).collect();
        let docs_b: Vec<Vec<u8>> = (0..7).map(|_| make_doc_i64("x", 2)).collect();

        let mut combined = AggAccum::new(&spec);
        for d in &docs_a {
            combined.feed(&spec, d);
        }
        for d in &docs_b {
            combined.feed(&spec, d);
        }

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        assert_eq!(
            combined.finalize(&spec),
            a.finalize(&spec),
            "merge_from count"
        );
    }

    #[test]
    fn merge_from_sum_avg() {
        let sum_spec = make_spec("sum", "v");
        let avg_spec = make_spec("avg", "v");

        let vals_a: Vec<f64> = [1.0, 2.0, 3.0].into();
        let vals_b: Vec<f64> = [4.0, 5.0, 6.0].into();

        let docs_a: Vec<Vec<u8>> = vals_a.iter().map(|&v| make_doc_f64("v", v)).collect();
        let docs_b: Vec<Vec<u8>> = vals_b.iter().map(|&v| make_doc_f64("v", v)).collect();

        // Combined baseline.
        let mut combined_sum = AggAccum::new(&sum_spec);
        let mut combined_avg = AggAccum::new(&avg_spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined_sum.feed(&sum_spec, d);
            combined_avg.feed(&avg_spec, d);
        }

        // Merge path.
        let mut a_sum = AggAccum::new(&sum_spec);
        let mut a_avg = AggAccum::new(&avg_spec);
        for d in &docs_a {
            a_sum.feed(&sum_spec, d);
            a_avg.feed(&avg_spec, d);
        }
        let mut b_sum = AggAccum::new(&sum_spec);
        let mut b_avg = AggAccum::new(&avg_spec);
        for d in &docs_b {
            b_sum.feed(&sum_spec, d);
            b_avg.feed(&avg_spec, d);
        }
        a_sum.merge_from(b_sum);
        a_avg.merge_from(b_avg);

        let Value::Float(cs) = combined_sum.finalize(&sum_spec) else {
            panic!("expected float");
        };
        let Value::Float(ms) = a_sum.finalize(&sum_spec) else {
            panic!("expected float");
        };
        assert!((cs - ms).abs() < 1e-9, "sum mismatch: {cs} vs {ms}");

        let Value::Float(ca) = combined_avg.finalize(&avg_spec) else {
            panic!("expected float");
        };
        let Value::Float(ma) = a_avg.finalize(&avg_spec) else {
            panic!("expected float");
        };
        assert!((ca - ma).abs() < 1e-9, "avg mismatch: {ca} vs {ma}");
    }

    #[test]
    fn merge_from_min_max() {
        let min_spec = make_spec("min", "v");
        let max_spec = make_spec("max", "v");

        let docs_a: Vec<Vec<u8>> = [3i64, 1, 7].iter().map(|&v| make_doc_i64("v", v)).collect();
        let docs_b: Vec<Vec<u8>> = [2i64, 9, 4].iter().map(|&v| make_doc_i64("v", v)).collect();

        let mut combined_min = AggAccum::new(&min_spec);
        let mut combined_max = AggAccum::new(&max_spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined_min.feed(&min_spec, d);
            combined_max.feed(&max_spec, d);
        }

        let mut a_min = AggAccum::new(&min_spec);
        let mut a_max = AggAccum::new(&max_spec);
        for d in &docs_a {
            a_min.feed(&min_spec, d);
            a_max.feed(&max_spec, d);
        }
        let mut b_min = AggAccum::new(&min_spec);
        let mut b_max = AggAccum::new(&max_spec);
        for d in &docs_b {
            b_min.feed(&min_spec, d);
            b_max.feed(&max_spec, d);
        }
        a_min.merge_from(b_min);
        a_max.merge_from(b_max);

        assert_eq!(
            combined_min.finalize(&min_spec),
            a_min.finalize(&min_spec),
            "min"
        );
        assert_eq!(
            combined_max.finalize(&max_spec),
            a_max.finalize(&max_spec),
            "max"
        );
    }

    #[test]
    fn merge_from_welford() {
        let spec = make_spec("variance", "v");

        let vals_a: Vec<f64> = (1..=10).map(|i| i as f64).collect();
        let vals_b: Vec<f64> = (11..=20).map(|i| i as f64).collect();
        let docs_a: Vec<Vec<u8>> = vals_a.iter().map(|&v| make_doc_f64("v", v)).collect();
        let docs_b: Vec<Vec<u8>> = vals_b.iter().map(|&v| make_doc_f64("v", v)).collect();

        let mut combined = AggAccum::new(&spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined.feed(&spec, d);
        }

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        let Value::Float(cv) = combined.finalize(&spec) else {
            panic!("expected float");
        };
        let Value::Float(mv) = a.finalize(&spec) else {
            panic!("expected float");
        };
        let rel = (cv - mv).abs() / cv.abs().max(1e-12);
        assert!(
            rel < 1e-9,
            "Welford merge variance: {cv} vs {mv} (rel={rel})"
        );
    }

    #[test]
    fn merge_from_count_distinct() {
        let spec = make_spec("count_distinct", "v");

        let docs_a: Vec<Vec<u8>> = ["a", "b", "c"]
            .iter()
            .map(|&s| make_doc_str("v", s))
            .collect();
        let docs_b: Vec<Vec<u8>> = ["c", "d", "e"]
            .iter()
            .map(|&s| make_doc_str("v", s))
            .collect();

        let mut combined = AggAccum::new(&spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined.feed(&spec, d);
        }
        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        assert_eq!(
            combined.finalize(&spec),
            a.finalize(&spec),
            "count_distinct"
        );
    }

    #[test]
    fn merge_from_hll() {
        let spec = make_spec("approx_count_distinct", "v");

        let docs_a: Vec<Vec<u8>> = (0..500u64).map(|i| make_doc_i64("v", i as i64)).collect();
        let docs_b: Vec<Vec<u8>> = (500..1000u64)
            .map(|i| make_doc_i64("v", i as i64))
            .collect();

        let mut combined = AggAccum::new(&spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined.feed(&spec, d);
        }

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        let Value::Integer(cv) = combined.finalize(&spec) else {
            panic!("expected int");
        };
        let Value::Integer(mv) = a.finalize(&spec) else {
            panic!("expected int");
        };
        // HLL is approximate; require within 5% of expected 1000.
        let diff = (cv - mv).abs() as f64 / 1000.0;
        assert!(diff < 0.05, "HLL merge: combined={cv}, merged={mv}");
    }

    #[test]
    fn merge_from_tdigest() {
        let spec = make_spec("approx_percentile", "0.5:v");

        let docs_a: Vec<Vec<u8>> = (0..100).map(|i| make_doc_f64("v", i as f64)).collect();
        let docs_b: Vec<Vec<u8>> = (100..200).map(|i| make_doc_f64("v", i as f64)).collect();

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        // p50 of 0..200 should be close to 100.
        let Value::Float(p50) = a.finalize(&spec) else {
            panic!("expected float");
        };
        assert!((50.0..150.0).contains(&p50), "TDigest merge p50={p50}");
    }

    #[test]
    fn merge_from_topk() {
        let spec = make_spec("approx_topk", "3:v");
        // TopK is heuristic; just verify the top item count is right.
        let docs_a: Vec<Vec<u8>> = (0..50).map(|i| make_doc_i64("v", i % 3)).collect();
        let docs_b: Vec<Vec<u8>> = (0..50).map(|i| make_doc_i64("v", i % 3)).collect();

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        let Value::Array(arr) = a.finalize(&spec) else {
            panic!("expected array");
        };
        assert_eq!(arr.len(), 3, "TopK should return k=3 items");
    }

    #[test]
    fn merge_from_array_agg() {
        let spec = make_spec("array_agg", "v");

        let docs_a: Vec<Vec<u8>> = [1i64, 2, 3].iter().map(|&v| make_doc_i64("v", v)).collect();
        let docs_b: Vec<Vec<u8>> = [4i64, 5, 6].iter().map(|&v| make_doc_i64("v", v)).collect();

        let mut combined = AggAccum::new(&spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined.feed(&spec, d);
        }

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        assert_eq!(combined.finalize(&spec), a.finalize(&spec), "array_agg");
    }

    #[test]
    fn merge_from_string_agg() {
        let spec = make_spec("string_agg", "v");

        let docs_a: Vec<Vec<u8>> = ["hello", "world"]
            .iter()
            .map(|&s| make_doc_str("v", s))
            .collect();
        let docs_b: Vec<Vec<u8>> = ["foo", "bar"]
            .iter()
            .map(|&s| make_doc_str("v", s))
            .collect();

        let mut combined = AggAccum::new(&spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined.feed(&spec, d);
        }

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        assert_eq!(combined.finalize(&spec), a.finalize(&spec), "string_agg");
    }

    #[test]
    fn merge_from_percentile_cont() {
        let spec = make_spec("percentile_cont", "0.5:v");

        let docs_a: Vec<Vec<u8>> = [1.0f64, 3.0, 5.0]
            .iter()
            .map(|&v| make_doc_f64("v", v))
            .collect();
        let docs_b: Vec<Vec<u8>> = [2.0f64, 4.0, 6.0]
            .iter()
            .map(|&v| make_doc_f64("v", v))
            .collect();

        let mut combined = AggAccum::new(&spec);
        for d in docs_a.iter().chain(docs_b.iter()) {
            combined.feed(&spec, d);
        }

        let mut a = AggAccum::new(&spec);
        for d in &docs_a {
            a.feed(&spec, d);
        }
        let mut b = AggAccum::new(&spec);
        for d in &docs_b {
            b.feed(&spec, d);
        }
        a.merge_from(b);

        assert_eq!(
            combined.finalize(&spec),
            a.finalize(&spec),
            "percentile_cont"
        );
    }
}
