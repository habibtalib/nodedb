// SPDX-License-Identifier: BUSL-1.1

//! Row-level WHERE predicate evaluation for memtable scans.

use nodedb_query::scan_filter::{FilterOp, ScanFilter};

/// Check whether a memtable row satisfies all filter predicates.
///
/// Returns `true` if every filter passes (AND semantics). Uses the full
/// `ScanFilter::matches_value` path which handles `FilterOp::Expr` predicates
/// (scalar functions, JSON operators, column arithmetic) in addition to simple
/// comparison operators.
pub(in crate::data::executor) fn row_matches_filters(
    row: &[nodedb_types::value::Value],
    schema: &nodedb_types::columnar::ColumnarSchema,
    filters: &[ScanFilter],
) -> bool {
    // Build a Value::Object so that ScanFilter::matches_value can navigate
    // field paths and expression predicates (e.g. pg_json_get_text).
    let mut map = std::collections::HashMap::with_capacity(schema.columns.len());
    for (i, col_def) in schema.columns.iter().enumerate() {
        if i < row.len() {
            map.insert(col_def.name.clone(), row[i].clone());
        }
    }
    let doc = nodedb_types::Value::Object(map);

    for filter in filters {
        if filter.op == FilterOp::MatchAll {
            continue;
        }
        if !filter.matches_value(&doc) {
            return false;
        }
    }
    true
}
