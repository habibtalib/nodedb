//! Binary operation evaluation.

use std::collections::BTreeMap;

use super::super::ast::BinOp;
use super::super::error::PromqlError;
use super::super::types::*;
use super::helpers::match_key;

pub fn eval_binary_op(
    op: BinOp,
    lhs: Value,
    rhs: Value,
    return_bool: bool,
    ts: i64,
) -> Result<Value, PromqlError> {
    match (&lhs, &rhs) {
        (Value::Scalar(a, _), Value::Scalar(b, _)) => {
            Ok(Value::Scalar(apply_binop(op, *a, *b, return_bool), ts))
        }
        (Value::Vector(vec), Value::Scalar(s, _)) => {
            let result: Vec<InstantSample> = vec
                .iter()
                .map(|v| InstantSample {
                    labels: v.labels.clone(),
                    value: apply_binop(op, v.value, *s, return_bool),
                    timestamp_ms: v.timestamp_ms,
                })
                .collect();
            Ok(Value::Vector(result))
        }
        (Value::Scalar(s, _), Value::Vector(vec)) => {
            let result: Vec<InstantSample> = vec
                .iter()
                .map(|v| InstantSample {
                    labels: v.labels.clone(),
                    value: apply_binop(op, *s, v.value, return_bool),
                    timestamp_ms: v.timestamp_ms,
                })
                .collect();
            Ok(Value::Vector(result))
        }
        (Value::Vector(left), Value::Vector(right)) => {
            eval_vector_binop(op, left, right, return_bool, ts)
        }
        _ => Err(PromqlError::TypeError {
            context: "binary op".to_string(),
            detail: "unsupported operation between matrix types".to_string(),
        }),
    }
}

fn eval_vector_binop(
    op: BinOp,
    left: &[InstantSample],
    right: &[InstantSample],
    return_bool: bool,
    ts: i64,
) -> Result<Value, PromqlError> {
    let right_map: BTreeMap<String, &InstantSample> =
        right.iter().map(|s| (match_key(&s.labels), s)).collect();

    let mut result = Vec::new();
    for l in left {
        let key = match_key(&l.labels);
        if let Some(r) = right_map.get(&key) {
            let val = apply_binop(op, l.value, r.value, return_bool);
            if !val.is_nan() || !op.is_comparison() || return_bool {
                let mut labels = l.labels.clone();
                labels.remove("__name__");
                result.push(InstantSample {
                    labels,
                    value: val,
                    timestamp_ms: ts,
                });
            }
        } else if op.is_set_op() && matches!(op, BinOp::Or) {
            result.push(l.clone());
        }
    }

    if matches!(op, BinOp::Or) {
        let left_keys: std::collections::HashSet<String> =
            left.iter().map(|s| match_key(&s.labels)).collect();
        for r in right {
            if !left_keys.contains(&match_key(&r.labels)) {
                result.push(r.clone());
            }
        }
    }

    Ok(Value::Vector(result))
}

fn apply_binop(op: BinOp, a: f64, b: f64, return_bool: bool) -> f64 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::Mod => a % b,
        BinOp::Pow => a.powf(b),
        BinOp::Eq => bool_result(a == b, return_bool, a),
        BinOp::Neq => bool_result(a != b, return_bool, a),
        BinOp::Lt => bool_result(a < b, return_bool, a),
        BinOp::Gt => bool_result(a > b, return_bool, a),
        BinOp::Lte => bool_result(a <= b, return_bool, a),
        BinOp::Gte => bool_result(a >= b, return_bool, a),
        BinOp::And | BinOp::Or | BinOp::Unless => a,
    }
}

fn bool_result(cond: bool, return_bool: bool, original: f64) -> f64 {
    if return_bool {
        if cond { 1.0 } else { 0.0 }
    } else if cond {
        original
    } else {
        f64::NAN
    }
}
