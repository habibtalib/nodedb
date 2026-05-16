// SPDX-License-Identifier: Apache-2.0

//! Value-native window-function evaluator for the Lite embedded engine.
//!
//! Operates on `Vec<Vec<nodedb_types::Value>>` rows directly, without any
//! serde_json dependency. Each spec appends one `Value` per row; the caller
//! appends the returned column names to its `columns` vec.

use std::collections::HashMap;

use nodedb_types::Value;

use super::spec::WindowFuncSpec;
use super::value_agg::apply_v_aggregate;
use crate::expr::types::SqlExpr;
use crate::value_ops::compare_values;

/// Error type for Value-mode window evaluation.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("window column '{name}' not found in result columns")]
    ColumnNotFound { name: String },

    #[error("window function argument error: {detail}")]
    ArgEval { detail: String },

    #[error("window frame error: {detail}")]
    BadFrame { detail: String },
}

/// Evaluate window functions over a `Vec<Vec<Value>>` result set.
///
/// `column_index` maps column name → position in each row slice.
/// For each spec, one `Value` is appended to every row. Returns the list of
/// new column names, one per spec in spec order.
pub fn evaluate_window_functions_value(
    rows: &mut [Vec<Value>],
    column_index: &HashMap<String, usize>,
    specs: &[WindowFuncSpec],
) -> Result<Vec<String>, WindowError> {
    let mut new_cols: Vec<String> = Vec::with_capacity(specs.len());

    for spec in specs {
        let partitions = build_value_partitions(rows, column_index, spec)?;
        let write_col = rows.first().map(|r| r.len()).unwrap_or(0);

        for row in rows.iter_mut() {
            row.push(Value::Null);
        }

        for partition_indices in &partitions {
            match spec.func_name.as_str() {
                "row_number" => apply_v_row_number(rows, partition_indices, write_col),
                "rank" => apply_v_rank(rows, partition_indices, column_index, spec, write_col),
                "dense_rank" => {
                    apply_v_dense_rank(rows, partition_indices, column_index, spec, write_col)
                }
                "ntile" => apply_v_ntile(rows, partition_indices, spec, write_col)?,
                "percent_rank" => {
                    apply_v_percent_rank(rows, partition_indices, column_index, spec, write_col)
                }
                "cume_dist" => {
                    apply_v_cume_dist(rows, partition_indices, column_index, spec, write_col)
                }
                "lag" => apply_v_lag(rows, partition_indices, column_index, spec, write_col)?,
                "lead" => apply_v_lead(rows, partition_indices, column_index, spec, write_col)?,
                "nth_value" => {
                    apply_v_nth_value(rows, partition_indices, column_index, spec, write_col)?
                }
                "sum" | "count" | "avg" | "min" | "max" | "first_value" | "last_value" => {
                    apply_v_aggregate(rows, partition_indices, column_index, spec, write_col)
                }
                other => {
                    return Err(WindowError::ArgEval {
                        detail: format!(
                            "unknown window function '{other}'; valid names: row_number, rank, \
                             dense_rank, ntile, percent_rank, cume_dist, lag, lead, nth_value, \
                             sum, count, avg, min, max, first_value, last_value"
                        ),
                    });
                }
            }
        }

        new_cols.push(spec.alias.clone());
    }

    Ok(new_cols)
}

// ── Partition building ────────────────────────────────────────────────────────

fn build_value_partitions(
    rows: &[Vec<Value>],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
) -> Result<Vec<Vec<usize>>, WindowError> {
    if spec.partition_by.is_empty() {
        return Ok(vec![(0..rows.len()).collect()]);
    }

    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for (i, row) in rows.iter().enumerate() {
        let key = partition_key(row, column_index, &spec.partition_by);
        let entry = groups.entry(key.clone()).or_default();
        if entry.is_empty() {
            order.push(key);
        }
        entry.push(i);
    }

    Ok(order.iter().filter_map(|k| groups.remove(k)).collect())
}

fn partition_key(
    row: &[Value],
    column_index: &HashMap<String, usize>,
    partition_by: &[SqlExpr],
) -> String {
    partition_by
        .iter()
        .map(|expr| {
            let v = eval_arg_for_row(expr, row, column_index);
            format!("{v:?}")
        })
        .collect::<Vec<_>>()
        .join("\x00")
}

// ── Value comparison helpers (pub(super) for value_agg) ───────────────────────

pub(super) fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        (va, vb) => compare_values(va, vb),
    }
}

pub(super) fn order_keys_equal_v(
    rows: &[Vec<Value>],
    a: usize,
    b: usize,
    column_index: &HashMap<String, usize>,
    order_by: &[(SqlExpr, bool)],
) -> bool {
    order_by.iter().all(|(expr, _)| {
        let row_a = rows.get(a).map(|r| r.as_slice()).unwrap_or(&[]);
        let row_b = rows.get(b).map(|r| r.as_slice()).unwrap_or(&[]);
        let va = eval_arg_for_row(expr, row_a, column_index);
        let vb = eval_arg_for_row(expr, row_b, column_index);
        matches!(cmp_values(&va, &vb), std::cmp::Ordering::Equal)
    })
}

// ── Argument evaluation (pub(super) for value_agg) ────────────────────────────

pub(super) fn eval_arg_for_row(
    expr: &SqlExpr,
    row: &[Value],
    column_index: &HashMap<String, usize>,
) -> Value {
    match expr {
        SqlExpr::Column(name) => column_index
            .get(name.as_str())
            .and_then(|&idx| row.get(idx))
            .cloned()
            .unwrap_or(Value::Null),
        SqlExpr::Literal(v) => v.clone(),
        other => {
            let doc = row_to_obj(row, column_index);
            other.eval(&doc)
        }
    }
}

fn row_to_obj(row: &[Value], column_index: &HashMap<String, usize>) -> Value {
    let mut map = HashMap::new();
    for (name, &idx) in column_index {
        if let Some(v) = row.get(idx) {
            map.insert(name.clone(), v.clone());
        }
    }
    Value::Object(map)
}

fn usize_arg(spec: &WindowFuncSpec, idx: usize, default: usize) -> usize {
    spec.args
        .get(idx)
        .and_then(|e| match e {
            SqlExpr::Literal(v) => v.as_f64().map(|n| n as usize),
            _ => None,
        })
        .unwrap_or(default)
}

fn default_arg_value(spec: &WindowFuncSpec, idx: usize) -> Value {
    spec.args
        .get(idx)
        .and_then(|e| match e {
            SqlExpr::Literal(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap_or(Value::Null)
}

// ── Cell write helper (pub(super) for value_agg) ──────────────────────────────

pub(super) fn set_cell(rows: &mut [Vec<Value>], row_idx: usize, col_idx: usize, val: Value) {
    if let Some(row) = rows.get_mut(row_idx)
        && let Some(cell) = row.get_mut(col_idx)
    {
        *cell = val;
    }
}

// ── Ranking functions ─────────────────────────────────────────────────────────

fn apply_v_row_number(rows: &mut [Vec<Value>], indices: &[usize], write_col: usize) {
    for (rank, &i) in indices.iter().enumerate() {
        set_cell(rows, i, write_col, Value::Integer((rank + 1) as i64));
    }
}

fn apply_v_rank(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) {
    if indices.is_empty() {
        return;
    }
    let mut current_rank = 1usize;
    set_cell(rows, indices[0], write_col, Value::Integer(1));
    for pos in 1..indices.len() {
        if !order_keys_equal_v(
            rows,
            indices[pos - 1],
            indices[pos],
            column_index,
            &spec.order_by,
        ) {
            current_rank = pos + 1;
        }
        set_cell(
            rows,
            indices[pos],
            write_col,
            Value::Integer(current_rank as i64),
        );
    }
}

fn apply_v_dense_rank(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) {
    if indices.is_empty() {
        return;
    }
    let mut current_rank = 1usize;
    set_cell(rows, indices[0], write_col, Value::Integer(1));
    for pos in 1..indices.len() {
        if !order_keys_equal_v(
            rows,
            indices[pos - 1],
            indices[pos],
            column_index,
            &spec.order_by,
        ) {
            current_rank += 1;
        }
        set_cell(
            rows,
            indices[pos],
            write_col,
            Value::Integer(current_rank as i64),
        );
    }
}

fn apply_v_ntile(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    spec: &WindowFuncSpec,
    write_col: usize,
) -> Result<(), WindowError> {
    let n = usize_arg(spec, 0, 1).max(1);
    let total = indices.len();
    if total == 0 {
        return Ok(());
    }
    for (pos, &i) in indices.iter().enumerate() {
        let bucket = (pos * n / total) + 1;
        set_cell(rows, i, write_col, Value::Integer(bucket as i64));
    }
    Ok(())
}

fn apply_v_percent_rank(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) {
    let total = indices.len();
    if total == 0 {
        return;
    }
    if total == 1 {
        set_cell(rows, indices[0], write_col, Value::Float(0.0));
        return;
    }
    let denom = (total - 1) as f64;
    let mut current_rank = 1usize;
    set_cell(rows, indices[0], write_col, Value::Float(0.0));
    for pos in 1..total {
        if !order_keys_equal_v(
            rows,
            indices[pos - 1],
            indices[pos],
            column_index,
            &spec.order_by,
        ) {
            current_rank = pos + 1;
        }
        let pr = (current_rank - 1) as f64 / denom;
        set_cell(rows, indices[pos], write_col, Value::Float(pr));
    }
}

fn apply_v_cume_dist(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) {
    let total = indices.len();
    if total == 0 {
        return;
    }
    let denom = total as f64;
    let mut group_start = 0;
    while group_start < total {
        let mut group_end = group_start + 1;
        while group_end < total
            && order_keys_equal_v(
                rows,
                indices[group_start],
                indices[group_end],
                column_index,
                &spec.order_by,
            )
        {
            group_end += 1;
        }
        let cd = group_end as f64 / denom;
        for &idx in &indices[group_start..group_end] {
            set_cell(rows, idx, write_col, Value::Float(cd));
        }
        group_start = group_end;
    }
}

// ── Offset functions ──────────────────────────────────────────────────────────

fn collect_arg_values(
    rows: &[Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
) -> Vec<Value> {
    indices
        .iter()
        .map(|&i| {
            rows.get(i)
                .map(|row| {
                    spec.args
                        .first()
                        .map(|expr| eval_arg_for_row(expr, row, column_index))
                        .unwrap_or(Value::Null)
                })
                .unwrap_or(Value::Null)
        })
        .collect()
}

fn apply_v_lag(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) -> Result<(), WindowError> {
    let offset = usize_arg(spec, 1, 1);
    let default = default_arg_value(spec, 2);
    let values = collect_arg_values(rows, indices, column_index, spec);
    for (pos, &i) in indices.iter().enumerate() {
        let val = if pos >= offset {
            values[pos - offset].clone()
        } else {
            default.clone()
        };
        set_cell(rows, i, write_col, val);
    }
    Ok(())
}

fn apply_v_lead(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) -> Result<(), WindowError> {
    let offset = usize_arg(spec, 1, 1);
    let default = default_arg_value(spec, 2);
    let values = collect_arg_values(rows, indices, column_index, spec);
    for (pos, &i) in indices.iter().enumerate() {
        let val = if pos + offset < indices.len() {
            values[pos + offset].clone()
        } else {
            default.clone()
        };
        set_cell(rows, i, write_col, val);
    }
    Ok(())
}

fn apply_v_nth_value(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) -> Result<(), WindowError> {
    let n = usize_arg(spec, 1, 1).max(1);
    let values = collect_arg_values(rows, indices, column_index, spec);
    for (pos, &i) in indices.iter().enumerate() {
        let val = if pos + 1 >= n {
            values[n - 1].clone()
        } else {
            Value::Null
        };
        set_cell(rows, i, write_col, val);
    }
    Ok(())
}
