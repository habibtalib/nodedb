// SPDX-License-Identifier: BUSL-1.1

//! Expression AST and evaluator for virtual-table queries.

use super::table::VTable;
use super::value::VValue;

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(VValue),
    Column(String),
    Star, // sentinel for COUNT(*)
    BinaryOp(Box<Expr>, BinOp, Box<Expr>),
    UnaryNot(Box<Expr>),
    UnaryNeg(Box<Expr>),
    IsNull(Box<Expr>, bool /*negated*/),
    InList(Box<Expr>, Vec<Expr>, bool /*negated*/),
    Between(Box<Expr>, Box<Expr>, Box<Expr>, bool /*negated*/),
    Like(Box<Expr>, String, bool /*negated*/),
    Aggregate(AggFn, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFn {
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("unknown column: {0}")]
    UnknownColumn(String),
    #[error("type mismatch in expression: {0}")]
    TypeMismatch(String),
    #[error("unsupported expression in virtual-table query: {0}")]
    Unsupported(String),
    #[error("invalid LIKE pattern: {0}")]
    InvalidLike(String),
}

/// Evaluate an expression in the context of a single row.
pub fn eval(expr: &Expr, row: &[VValue], table: &VTable) -> Result<VValue, EvalError> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Star => Ok(VValue::Null),
        Expr::Column(name) => {
            let idx = table
                .column_index(name)
                .ok_or_else(|| EvalError::UnknownColumn(name.clone()))?;
            Ok(row[idx].clone())
        }
        Expr::UnaryNot(e) => {
            let v = eval(e, row, table)?;
            match v {
                VValue::Null => Ok(VValue::Null),
                VValue::Bool(b) => Ok(VValue::Bool(!b)),
                _ => Err(EvalError::TypeMismatch("NOT requires boolean".into())),
            }
        }
        Expr::UnaryNeg(e) => {
            let v = eval(e, row, table)?;
            match v {
                VValue::Null => Ok(VValue::Null),
                VValue::Int4(i) => Ok(VValue::Int4(-i)),
                VValue::Int8(i) => Ok(VValue::Int8(-i)),
                _ => Err(EvalError::TypeMismatch("unary - on non-integer".into())),
            }
        }
        Expr::IsNull(e, negated) => {
            let v = eval(e, row, table)?;
            let is_null = v.is_null();
            Ok(VValue::Bool(if *negated { !is_null } else { is_null }))
        }
        Expr::BinaryOp(l, op, r) => {
            let lv = eval(l, row, table)?;
            let rv = eval(r, row, table)?;
            apply_binary(op, &lv, &rv)
        }
        Expr::InList(e, items, negated) => {
            let v = eval(e, row, table)?;
            if v.is_null() {
                return Ok(VValue::Null);
            }
            let mut found = false;
            let mut any_null = false;
            for item in items {
                let iv = eval(item, row, table)?;
                if iv.is_null() {
                    any_null = true;
                    continue;
                }
                if let Some(std::cmp::Ordering::Equal) = v.sql_cmp(&iv) {
                    found = true;
                    break;
                }
            }
            let result = if found {
                true
            } else if any_null {
                return Ok(VValue::Null);
            } else {
                false
            };
            Ok(VValue::Bool(if *negated { !result } else { result }))
        }
        Expr::Between(e, lo, hi, negated) => {
            let v = eval(e, row, table)?;
            let lov = eval(lo, row, table)?;
            let hiv = eval(hi, row, table)?;
            if v.is_null() || lov.is_null() || hiv.is_null() {
                return Ok(VValue::Null);
            }
            let in_range = matches!(
                v.sql_cmp(&lov),
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            ) && matches!(
                v.sql_cmp(&hiv),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            );
            Ok(VValue::Bool(if *negated { !in_range } else { in_range }))
        }
        Expr::Like(e, pattern, negated) => {
            let v = eval(e, row, table)?;
            let Some(s) = v.as_text() else {
                if v.is_null() {
                    return Ok(VValue::Null);
                }
                return Err(EvalError::TypeMismatch("LIKE requires text".into()));
            };
            let m = like_match(s, pattern);
            Ok(VValue::Bool(if *negated { !m } else { m }))
        }
        Expr::Aggregate(_, _) => Err(EvalError::Unsupported(
            "aggregate functions only allowed in projection".into(),
        )),
    }
}

fn apply_binary(op: &BinOp, l: &VValue, r: &VValue) -> Result<VValue, EvalError> {
    // Logical short-circuit semantics for AND / OR with NULL.
    match op {
        BinOp::And => {
            return Ok(match (l.as_bool(), r.as_bool()) {
                (Some(true), Some(true)) => VValue::Bool(true),
                (Some(false), _) | (_, Some(false)) => VValue::Bool(false),
                _ => VValue::Null,
            });
        }
        BinOp::Or => {
            return Ok(match (l.as_bool(), r.as_bool()) {
                (Some(true), _) | (_, Some(true)) => VValue::Bool(true),
                (Some(false), Some(false)) => VValue::Bool(false),
                _ => VValue::Null,
            });
        }
        _ => {}
    }
    if l.is_null() || r.is_null() {
        return Ok(VValue::Null);
    }
    match op {
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            let Some(ord) = l.sql_cmp(r) else {
                return Err(EvalError::TypeMismatch(format!(
                    "incompatible comparison: {l:?} vs {r:?}"
                )));
            };
            let result = match op {
                BinOp::Eq => ord == std::cmp::Ordering::Equal,
                BinOp::NotEq => ord != std::cmp::Ordering::Equal,
                BinOp::Lt => ord == std::cmp::Ordering::Less,
                BinOp::LtEq => ord != std::cmp::Ordering::Greater,
                BinOp::Gt => ord == std::cmp::Ordering::Greater,
                BinOp::GtEq => ord != std::cmp::Ordering::Less,
                _ => unreachable!(),
            };
            Ok(VValue::Bool(result))
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
            let (Some(x), Some(y)) = (l.as_i64(), r.as_i64()) else {
                return Err(EvalError::TypeMismatch(
                    "arithmetic requires integer operands".into(),
                ));
            };
            let result = match op {
                BinOp::Add => x.wrapping_add(y),
                BinOp::Sub => x.wrapping_sub(y),
                BinOp::Mul => x.wrapping_mul(y),
                BinOp::Div => {
                    if y == 0 {
                        return Err(EvalError::TypeMismatch("division by zero".into()));
                    }
                    x / y
                }
                _ => unreachable!(),
            };
            Ok(VValue::Int8(result))
        }
        BinOp::And | BinOp::Or => unreachable!(),
    }
}

/// Treat the predicate value as a SQL truth: NULL → false, true → true,
/// false → false.
pub fn truthy(v: &VValue) -> bool {
    matches!(v, VValue::Bool(true))
}

/// SQL `LIKE` with `%` (any string) and `_` (any single char). No escape
/// handling — virtual-table data does not embed literal `%` or `_`.
fn like_match(s: &str, pattern: &str) -> bool {
    let s_chars: Vec<char> = s.chars().collect();
    let p_chars: Vec<char> = pattern.chars().collect();
    like_match_recursive(&s_chars, &p_chars)
}

fn like_match_recursive(s: &[char], p: &[char]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        '%' => {
            // Skip consecutive '%' to bound recursion.
            let mut i = 1;
            while i < p.len() && p[i] == '%' {
                i += 1;
            }
            let rest = &p[i..];
            if rest.is_empty() {
                return true;
            }
            (0..=s.len()).any(|k| like_match_recursive(&s[k..], rest))
        }
        '_' => !s.is_empty() && like_match_recursive(&s[1..], &p[1..]),
        c => !s.is_empty() && s[0] == c && like_match_recursive(&s[1..], &p[1..]),
    }
}
