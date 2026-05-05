//! Native columnar aggregation for GROUP BY queries on columnar memtables.
//!
//! Bypasses the generic document-style aggregation path that converts
//! columnar data → serde_json::Map → msgpack → JSON string keys.
//! Instead, filters and groups directly on column vectors using integer
//! symbol IDs as group keys. Resolves symbol names only for the final
//! response.
//!
//! Performance: ~36x faster than the generic path for high-cardinality
//! GROUP BY (e.g., 10K+ unique qnames) because it eliminates:
//! - Per-row serde_json::Map construction
//! - Per-row msgpack encode/decode roundtrip
//! - JSON-serialized string HashMap keys
//! - Per-row symbol dictionary string allocation

use std::collections::HashMap;

use crate::engine::timeseries::columnar_memtable::{ColumnData, ColumnType, ColumnarMemtable};
use nodedb_query::agg_key::canonical_agg_key;

use super::columnar_filter;

#[path = "columnar_agg_support.rs"]
mod columnar_agg_support;
use columnar_agg_support::{
    AggAccum, DenseSymbolParams, GroupKey, GroupKeyPart, aggregate_dense_symbol,
    extract_group_key_part, for_each_set_bit, resolve_key_part,
};

/// Result of native columnar aggregation.
pub(super) struct ColumnarAggResult {
    pub rows: Vec<serde_json::Value>,
}

/// Try to execute an aggregate query natively on a columnar memtable.
///
/// Returns `None` if the query can't be handled natively (complex filters,
/// string comparison filters, etc.), in which case the caller should fall
/// back to the generic document-style path.
pub(super) fn try_columnar_aggregate(
    mt: &ColumnarMemtable,
    group_by: &[String],
    aggregates: &[(String, String)],
    filters: &[crate::bridge::scan_filter::ScanFilter],
    limit: usize,
    scan_limit: usize,
) -> Option<ColumnarAggResult> {
    let schema = mt.schema();
    let row_count = (mt.row_count() as usize).min(scan_limit);

    if row_count == 0 {
        return Some(ColumnarAggResult { rows: Vec::new() });
    }

    // --- Phase 1: Resolve column indices for group-by and aggregate fields ---

    let group_col_info: Vec<(usize, ColumnType)> = group_by
        .iter()
        .map(|name| {
            schema
                .columns
                .iter()
                .enumerate()
                .find(|(_, (n, _))| n == name)
                .map(|(i, (_, ty))| (i, *ty))
        })
        .collect::<Option<Vec<_>>>()?;

    // For each aggregate, find the column index (None for count(*)).
    let agg_col_info: Vec<(usize, ColumnType)> = aggregates
        .iter()
        .filter(|(_, field)| field != "*")
        .map(|(_, field)| {
            schema
                .columns
                .iter()
                .enumerate()
                .find(|(_, (n, _))| n == field)
                .map(|(i, (_, ty))| (i, *ty))
        })
        .collect::<Option<Vec<_>>>()?;

    // Only handle numeric aggregation columns (Float64, Int64, Timestamp).
    for (_, ty) in &agg_col_info {
        if *ty == ColumnType::Symbol {
            return None; // Can't SUM/AVG a symbol column
        }
    }

    // --- Phase 2: Build filter mask ---

    // Try SIMD bitmask path first; fall back to dense bool mask; fail on complex filters.
    enum FilterResult {
        Bitmask(Vec<u64>),
        BoolMask(Vec<bool>),
        None,
    }

    let filter_result = if filters.is_empty() {
        FilterResult::None
    } else {
        match columnar_filter::eval_filters_bitmask(mt, filters, row_count) {
            Some(bm) => FilterResult::Bitmask(bm),
            None => match columnar_filter::eval_filters_dense(mt, filters, row_count) {
                Some(mask) => FilterResult::BoolMask(mask),
                None => return None, // Complex filters — fall back to generic path
            },
        }
    };

    // Pre-fetch aggregate column data. For count(*), we don't need column data.
    let agg_col_data: Vec<Option<(usize, &ColumnData)>> = aggregates
        .iter()
        .map(|(_, field)| {
            if field == "*" {
                None
            } else {
                schema
                    .columns
                    .iter()
                    .enumerate()
                    .find(|(_, (n, _))| n == field)
                    .map(|(i, _)| (i, mt.column(i)))
            }
        })
        .collect();

    // --- Phase 3: Group and accumulate ---

    // Fast path: single Symbol GROUP BY column with cardinality ≤ 65536.
    // Replaces HashMap with a dense array indexed by symbol ID.
    let dense_result: Option<Vec<(u32, Vec<AggAccum>)>> =
        if group_col_info.len() == 1 && group_col_info[0].1 == ColumnType::Symbol {
            let col_idx = group_col_info[0].0;
            if let Some(dict) = mt.symbol_dict(col_idx) {
                let cardinality = dict.len();
                if cardinality <= 65_536 {
                    let (bm, boolm) = match &filter_result {
                        FilterResult::Bitmask(bm) => (Some(bm.as_slice()), None),
                        FilterResult::BoolMask(m) => (None, Some(m.as_slice())),
                        FilterResult::None => (None, None),
                    };
                    Some(aggregate_dense_symbol(&DenseSymbolParams {
                        mt,
                        group_col_idx: col_idx,
                        agg_col_data: &agg_col_data,
                        aggregates,
                        bitmask: bm,
                        bool_mask: boolm,
                        row_count,
                        cardinality,
                    }))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

    let groups: HashMap<GroupKey, Vec<AggAccum>> = if let Some(dense) = dense_result {
        dense
            .into_iter()
            .map(|(sym_id, accums)| (vec![GroupKeyPart::SymbolId(sym_id)], accums))
            .collect()
    } else {
        // HashMap path: multi-column GROUP BY, numeric keys, or high-cardinality symbols.
        let num_aggs = aggregates.len();
        let group_col_data: Vec<_> = group_col_info
            .iter()
            .map(|&(idx, ty)| (idx, ty, mt.column(idx)))
            .collect();

        let mut groups: HashMap<GroupKey, Vec<AggAccum>> = HashMap::with_capacity(1024);

        let mut process_row = |row_idx: usize| {
            let key: GroupKey = if group_by.is_empty() {
                Vec::new()
            } else {
                group_col_data
                    .iter()
                    .map(|(_, col_type, col_data)| {
                        extract_group_key_part(col_type, col_data, row_idx)
                    })
                    .collect()
            };

            let accums = groups
                .entry(key)
                .or_insert_with(|| (0..num_aggs).map(|_| AggAccum::new()).collect());

            for (agg_idx, (op, _)) in aggregates.iter().enumerate() {
                match &agg_col_data[agg_idx] {
                    None => accums[agg_idx].feed_count_only(),
                    Some((_, col_data)) => {
                        let val = match col_data {
                            ColumnData::Float64(vals) => vals[row_idx],
                            ColumnData::Int64(vals) => vals[row_idx] as f64,
                            ColumnData::Timestamp(vals) => vals[row_idx] as f64,
                            _ => return,
                        };
                        if op == "count" {
                            accums[agg_idx].feed_count_only();
                        } else {
                            accums[agg_idx].feed(val);
                        }
                    }
                }
            }
        };

        match &filter_result {
            FilterResult::Bitmask(bm) => {
                for_each_set_bit(bm, row_count, &mut process_row);
            }
            FilterResult::BoolMask(mask) => {
                for (row_idx, &passes) in mask.iter().enumerate().take(row_count) {
                    if passes {
                        process_row(row_idx);
                    }
                }
            }
            FilterResult::None => {
                for row_idx in 0..row_count {
                    process_row(row_idx);
                }
            }
        }

        groups
    };

    // --- Phase 4: Build result rows (resolve symbols only here) ---

    let mut results: Vec<serde_json::Value> = Vec::with_capacity(groups.len().min(limit));

    for (group_key, accums) in &groups {
        let mut row = serde_json::Map::new();

        // Resolve group key parts to display values.
        for (i, field) in group_by.iter().enumerate() {
            let (col_idx, _) = group_col_info[i];
            let val = if i < group_key.len() {
                resolve_key_part(mt, col_idx, &group_key[i])
            } else {
                serde_json::Value::Null
            };
            row.insert(field.clone(), val);
        }

        // Emit aggregate values.
        for (agg_idx, (op, field)) in aggregates.iter().enumerate() {
            let agg_key = canonical_agg_key(op, field);
            let accum = &accums[agg_idx];
            let val = match op.as_str() {
                "count" => serde_json::json!(accum.count),
                "sum" => {
                    if accum.count == 0 {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(accum.sum)
                    }
                }
                "avg" => {
                    if accum.count == 0 {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(accum.sum / accum.count as f64)
                    }
                }
                "min" => {
                    if accum.count == 0 {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(accum.min)
                    }
                }
                "max" => {
                    if accum.count == 0 {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(accum.max)
                    }
                }
                _ => serde_json::Value::Null,
            };
            row.insert(agg_key, val);
        }

        results.push(serde_json::Value::Object(row));
        if results.len() >= limit {
            break;
        }
    }

    Some(ColumnarAggResult { rows: results })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timeseries::columnar_memtable::{
        ColumnType, ColumnarMemtable, ColumnarMemtableConfig, ColumnarSchema,
    };
    use nodedb_types::timeseries::SeriesId;

    fn make_test_memtable() -> ColumnarMemtable {
        let schema = ColumnarSchema {
            columns: vec![
                ("timestamp".into(), ColumnType::Timestamp),
                ("value".into(), ColumnType::Float64),
                ("qname".into(), ColumnType::Symbol),
                ("qtype".into(), ColumnType::Symbol),
            ],
            timestamp_idx: 0,
            codecs: vec![],
        };
        let config = ColumnarMemtableConfig::default();
        let mut mt = ColumnarMemtable::new(schema, config);

        use crate::engine::timeseries::columnar_memtable::ColumnValue;

        // Insert test rows with varying qnames (high cardinality) and qtypes (low cardinality)
        let qnames = [
            "example.com",
            "google.com",
            "github.com",
            "reddit.com",
            "rust-lang.org",
        ];
        let qtypes = ["A", "AAAA"];

        for i in 0..100 {
            let series_id: SeriesId = i as u64;
            let values = [
                ColumnValue::Timestamp(i as i64 * 1000),
                ColumnValue::Float64(i as f64 * 100.0),
                ColumnValue::Symbol(qnames[i % qnames.len()]),
                ColumnValue::Symbol(qtypes[i % qtypes.len()]),
            ];
            mt.ingest_row(series_id, &values).unwrap();
        }

        mt
    }

    #[test]
    fn group_by_symbol_column() {
        let mt = make_test_memtable();
        let result = try_columnar_aggregate(
            &mt,
            &["qtype".into()],
            &[("count".into(), "*".into()), ("avg".into(), "value".into())],
            &[],
            100,
            100_000,
        )
        .unwrap();

        assert_eq!(result.rows.len(), 2); // A and AAAA
        for row in &result.rows {
            let count = row.get("count(*)").and_then(|v| v.as_u64()).unwrap();
            assert_eq!(count, 50); // 100 rows / 2 types
        }
    }

    #[test]
    fn group_by_high_cardinality() {
        let mt = make_test_memtable();
        let result = try_columnar_aggregate(
            &mt,
            &["qname".into()],
            &[("count".into(), "*".into()), ("sum".into(), "value".into())],
            &[],
            100,
            100_000,
        )
        .unwrap();

        assert_eq!(result.rows.len(), 5); // 5 unique qnames
    }

    #[test]
    fn filter_and_group() {
        let mt = make_test_memtable();
        let filter = crate::bridge::scan_filter::ScanFilter {
            field: "value".into(),
            op: "gt".into(),
            value: nodedb_types::Value::Float(5000.0),
            clauses: vec![],
            expr: None,
        };
        let result = try_columnar_aggregate(
            &mt,
            &["qname".into()],
            &[("count".into(), "*".into()), ("avg".into(), "value".into())],
            &[filter],
            100,
            100_000,
        )
        .unwrap();

        // Only rows with value > 5000 (i >= 51, value >= 5100)
        assert!(!result.rows.is_empty());
        for row in &result.rows {
            let avg = row.get("avg(value)").and_then(|v| v.as_f64()).unwrap();
            assert!(avg > 5000.0);
        }
    }

    #[test]
    fn no_group_by_aggregate_all() {
        let mt = make_test_memtable();
        let result = try_columnar_aggregate(
            &mt,
            &[],
            &[("count".into(), "*".into()), ("sum".into(), "value".into())],
            &[],
            100,
            100_000,
        )
        .unwrap();

        assert_eq!(result.rows.len(), 1);
        let count = result.rows[0]
            .get("count(*)")
            .and_then(|v| v.as_u64())
            .unwrap();
        assert_eq!(count, 100);
    }
}
