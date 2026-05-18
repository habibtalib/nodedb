// SPDX-License-Identifier: Apache-2.0

//! Aggregate window functions (sum, count, avg, min, max, first_value, last_value)
//! and frame-bound resolution for the Value-native evaluator.

use std::collections::HashMap;

use nodedb_types::Value;

use super::spec::{FrameBound, WindowFrame, WindowFuncSpec};
use super::value_eval::{cmp_values, eval_arg_for_row, order_keys_equal_v, set_cell};
use crate::simd_agg;

pub(super) fn apply_v_aggregate(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) {
    let use_running = spec.frame.mode == "range"
        && matches!(spec.frame.start, FrameBound::UnboundedPreceding)
        && matches!(spec.frame.end, FrameBound::CurrentRow);

    if use_running {
        apply_v_running_aggregate(rows, indices, column_index, spec, write_col);
    } else {
        apply_v_per_row_aggregate(rows, indices, column_index, spec, write_col);
    }
}

fn eval_arg(spec: &WindowFuncSpec, row: &[Value], column_index: &HashMap<String, usize>) -> Value {
    spec.args
        .first()
        .map(|expr| eval_arg_for_row(expr, row, column_index))
        .unwrap_or(Value::Null)
}

fn apply_v_running_aggregate(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) {
    let len = indices.len();
    if len == 0 {
        return;
    }

    let mut running_sum = 0.0f64;
    let mut running_count = 0u64;
    let mut running_min: Option<f64> = None;
    let mut running_max: Option<f64> = None;
    let mut peer_start = 0usize;

    for pos in 0..len {
        let i = indices[pos];
        let val = rows
            .get(i)
            .map(|row| eval_arg(spec, row, column_index))
            .unwrap_or(Value::Null);

        if let Some(n) = val.as_f64() {
            running_sum += n;
            running_count += 1;
            running_min = Some(running_min.map_or(n, |m: f64| m.min(n)));
            running_max = Some(running_max.map_or(n, |m: f64| m.max(n)));
        } else if spec.func_name == "count" {
            running_count += 1;
        }

        let is_last_in_group = pos + 1 == len
            || !order_keys_equal_v(rows, i, indices[pos + 1], column_index, &spec.order_by);

        if is_last_in_group {
            let first_val = rows
                .get(indices[0])
                .map(|row| eval_arg(spec, row, column_index))
                .unwrap_or(Value::Null);
            let last_val = rows
                .get(indices[pos])
                .map(|row| eval_arg(spec, row, column_index))
                .unwrap_or(Value::Null);

            let result = match spec.func_name.as_str() {
                "sum" => Value::Float(running_sum),
                "count" => Value::Integer(running_count as i64),
                "avg" => {
                    if running_count > 0 {
                        Value::Float(running_sum / running_count as f64)
                    } else {
                        Value::Null
                    }
                }
                "min" => running_min.map(Value::Float).unwrap_or(Value::Null),
                "max" => running_max.map(Value::Float).unwrap_or(Value::Null),
                "first_value" => first_val,
                "last_value" => last_val,
                _ => Value::Null,
            };

            for &peer_idx in &indices[peer_start..=pos] {
                set_cell(rows, peer_idx, write_col, result.clone());
            }
            peer_start = pos + 1;
        }
    }
}

fn apply_v_per_row_aggregate(
    rows: &mut [Vec<Value>],
    indices: &[usize],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    write_col: usize,
) {
    let len = indices.len();
    if len == 0 {
        return;
    }

    let order_expr = spec.order_by.first().map(|(expr, _)| expr);
    let order_values: Vec<Value> = indices
        .iter()
        .map(|&i| {
            order_expr
                .and_then(|expr| {
                    rows.get(i)
                        .map(|row| eval_arg_for_row(expr, row, column_index))
                })
                .unwrap_or(Value::Null)
        })
        .collect();

    let peer_groups: Vec<usize> = if spec.frame.mode == "groups" {
        build_v_peer_groups(&order_values)
    } else {
        Vec::new()
    };

    let all_vals: Vec<Option<f64>> = indices
        .iter()
        .map(|&i| {
            rows.get(i)
                .map(|row| eval_arg(spec, row, column_index).as_f64())
                .unwrap_or(None)
        })
        .collect();

    let results: Vec<Value> = (0..len)
        .map(|pos| {
            let (start_idx, end_idx) =
                evaluate_v_frame_bounds(&spec.frame, pos, len, &order_values, &peer_groups);
            aggregate_v_slice(
                &all_vals,
                indices,
                rows,
                column_index,
                spec,
                start_idx,
                end_idx,
            )
        })
        .collect();

    for (pos, result) in results.into_iter().enumerate() {
        set_cell(rows, indices[pos], write_col, result);
    }
}

fn aggregate_v_slice(
    all_vals: &[Option<f64>],
    indices: &[usize],
    rows: &[Vec<Value>],
    column_index: &HashMap<String, usize>,
    spec: &WindowFuncSpec,
    start_idx: usize,
    end_idx: usize,
) -> Value {
    let slice_vals: Vec<f64> = all_vals[start_idx..=end_idx]
        .iter()
        .filter_map(|v| *v)
        .collect();
    let slice_count = end_idx - start_idx + 1;

    match spec.func_name.as_str() {
        "sum" => {
            let rt = simd_agg::ts_runtime();
            Value::Float((rt.sum_f64)(&slice_vals))
        }
        "count" => Value::Integer(slice_count as i64),
        "avg" => {
            if slice_vals.is_empty() {
                Value::Null
            } else {
                let rt = simd_agg::ts_runtime();
                Value::Float((rt.sum_f64)(&slice_vals) / slice_vals.len() as f64)
            }
        }
        "min" => {
            if slice_vals.is_empty() {
                Value::Null
            } else {
                let rt = simd_agg::ts_runtime();
                Value::Float((rt.min_f64)(&slice_vals))
            }
        }
        "max" => {
            if slice_vals.is_empty() {
                Value::Null
            } else {
                let rt = simd_agg::ts_runtime();
                Value::Float((rt.max_f64)(&slice_vals))
            }
        }
        "first_value" => indices
            .get(start_idx)
            .and_then(|&i| rows.get(i))
            .map(|row| {
                eval_arg_for_row(
                    spec.args
                        .first()
                        .unwrap_or(&crate::expr::types::SqlExpr::Literal(Value::Null)),
                    row,
                    column_index,
                )
            })
            .unwrap_or(Value::Null),
        "last_value" => indices
            .get(end_idx)
            .and_then(|&i| rows.get(i))
            .map(|row| {
                eval_arg_for_row(
                    spec.args
                        .first()
                        .unwrap_or(&crate::expr::types::SqlExpr::Literal(Value::Null)),
                    row,
                    column_index,
                )
            })
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn build_v_peer_groups(order_values: &[Value]) -> Vec<usize> {
    let mut groups = Vec::with_capacity(order_values.len());
    let mut current_group = 0usize;
    for (i, val) in order_values.iter().enumerate() {
        if i > 0
            && !matches!(
                cmp_values(val, &order_values[i - 1]),
                std::cmp::Ordering::Equal
            )
        {
            current_group += 1;
        }
        groups.push(current_group);
    }
    groups
}

pub(super) fn evaluate_v_frame_bounds(
    frame: &WindowFrame,
    pos: usize,
    len: usize,
    order_values: &[Value],
    peer_groups: &[usize],
) -> (usize, usize) {
    match frame.mode.as_str() {
        "rows" => v_rows_bounds(&frame.start, &frame.end, pos, len),
        "range" => v_range_bounds(&frame.start, &frame.end, pos, len, order_values),
        "groups" => v_groups_bounds(&frame.start, &frame.end, pos, len, peer_groups),
        _ => (0, len.saturating_sub(1)),
    }
}

fn v_rows_bounds(start: &FrameBound, end: &FrameBound, pos: usize, len: usize) -> (usize, usize) {
    let s = v_rows_bound_to_idx(start, pos, len);
    let e = v_rows_bound_to_idx(end, pos, len);
    (s.min(e), s.max(e))
}

fn v_rows_bound_to_idx(bound: &FrameBound, pos: usize, len: usize) -> usize {
    match bound {
        FrameBound::UnboundedPreceding => 0,
        FrameBound::Preceding(n) => pos.saturating_sub(*n as usize),
        FrameBound::CurrentRow => pos,
        FrameBound::Following(n) => (pos + *n as usize).min(len.saturating_sub(1)),
        FrameBound::UnboundedFollowing => len.saturating_sub(1),
    }
}

fn v_range_bounds(
    start: &FrameBound,
    end: &FrameBound,
    pos: usize,
    len: usize,
    order_values: &[Value],
) -> (usize, usize) {
    let current_val = order_values.get(pos).and_then(|v| v.as_f64());
    let s = v_range_bound_to_idx(start, pos, len, order_values, current_val, true);
    let e = v_range_bound_to_idx(end, pos, len, order_values, current_val, false);
    (s.min(e), s.max(e))
}

fn v_range_bound_to_idx(
    bound: &FrameBound,
    pos: usize,
    len: usize,
    order_values: &[Value],
    current_val: Option<f64>,
    is_start: bool,
) -> usize {
    match bound {
        FrameBound::UnboundedPreceding => 0,
        FrameBound::UnboundedFollowing => len.saturating_sub(1),
        FrameBound::CurrentRow => {
            if is_start {
                let mut idx = pos;
                while idx > 0
                    && matches!(
                        cmp_values(
                            order_values.get(idx - 1).unwrap_or(&Value::Null),
                            order_values.get(pos).unwrap_or(&Value::Null),
                        ),
                        std::cmp::Ordering::Equal
                    )
                {
                    idx -= 1;
                }
                idx
            } else {
                let mut idx = pos;
                while idx + 1 < len
                    && matches!(
                        cmp_values(
                            order_values.get(idx + 1).unwrap_or(&Value::Null),
                            order_values.get(pos).unwrap_or(&Value::Null),
                        ),
                        std::cmp::Ordering::Equal
                    )
                {
                    idx += 1;
                }
                idx
            }
        }
        FrameBound::Preceding(n) => {
            let threshold = match current_val {
                Some(cv) => cv - *n as f64,
                None => return pos,
            };
            let mut idx = 0;
            for (i, v) in order_values.iter().enumerate() {
                if v.as_f64().is_some_and(|fv| fv >= threshold) {
                    idx = i;
                    break;
                }
                idx = i + 1;
            }
            idx.min(len.saturating_sub(1))
        }
        FrameBound::Following(n) => {
            let threshold = match current_val {
                Some(cv) => cv + *n as f64,
                None => return pos,
            };
            let mut idx = pos;
            for (i, v) in order_values.iter().enumerate().skip(pos) {
                if v.as_f64().is_none_or(|fv| fv > threshold) {
                    break;
                }
                idx = i;
            }
            idx.min(len.saturating_sub(1))
        }
    }
}

fn v_groups_bounds(
    start: &FrameBound,
    end: &FrameBound,
    pos: usize,
    len: usize,
    peer_groups: &[usize],
) -> (usize, usize) {
    let current_group = peer_groups.get(pos).copied().unwrap_or(0);
    let max_group = peer_groups.last().copied().unwrap_or(0);
    let start_group = v_groups_bound_to_group(start, current_group, max_group);
    let end_group = v_groups_bound_to_group(end, current_group, max_group);
    let start_idx = peer_groups
        .iter()
        .position(|&g| g == start_group)
        .unwrap_or(0);
    let end_idx = peer_groups
        .iter()
        .rposition(|&g| g == end_group)
        .unwrap_or(len.saturating_sub(1));
    (start_idx, end_idx)
}

fn v_groups_bound_to_group(bound: &FrameBound, current_group: usize, max_group: usize) -> usize {
    match bound {
        FrameBound::UnboundedPreceding => 0,
        FrameBound::UnboundedFollowing => max_group,
        FrameBound::CurrentRow => current_group,
        FrameBound::Preceding(n) => current_group.saturating_sub(*n as usize),
        FrameBound::Following(n) => (current_group + *n as usize).min(max_group),
    }
}
