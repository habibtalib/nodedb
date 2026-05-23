// SPDX-License-Identifier: BUSL-1.1

//! Top-level virtual-query executor: filter → aggregate-or-project → sort → limit.

use super::expr::{AggFn, EvalError, Expr, eval, truthy};
use super::select::{ParseError, VProj, VSelect};
use super::table::VTable;
use super::value::{VColumn, VType, VValue};

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("eval: {0}")]
    Eval(#[from] EvalError),
    #[error("{0}")]
    Parse(#[from] ParseError),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// Output schema name + value type for one projected column.
#[derive(Debug, Clone)]
pub struct OutColumn {
    pub name: String,
    pub ty: VType,
}

#[derive(Debug)]
pub struct ResultSet {
    pub columns: Vec<OutColumn>,
    pub rows: Vec<Vec<VValue>>,
}

pub fn execute(select: &VSelect, input: VTable) -> Result<ResultSet, ExecError> {
    // 1. Apply WHERE.
    let mut filtered: Vec<Vec<VValue>> = Vec::with_capacity(input.rows.len());
    for row in &input.rows {
        let keep = match &select.filter {
            Some(predicate) => {
                let v = eval(predicate, row, &input)?;
                truthy(&v)
            }
            None => true,
        };
        if keep {
            filtered.push(row.clone());
        }
    }

    // 2. Projection — aggregate vs. row-wise.
    let (mut out_cols, mut out_rows) = if select.has_aggregate {
        project_aggregate(select, &filtered, &input)?
    } else {
        project_rowwise(select, &filtered, &input)?
    };

    // 3. ORDER BY. Aggregate result is a single row; sorting it is a no-op
    //    but harmless.
    if !select.order_by.is_empty() && !select.has_aggregate {
        sort_rows(&mut out_rows, &select.order_by, &input)?;
    }

    // 4. OFFSET / LIMIT.
    if select.offset > 0 {
        let skip = select.offset.min(out_rows.len());
        out_rows.drain(..skip);
    }
    if let Some(limit) = select.limit
        && out_rows.len() > limit
    {
        out_rows.truncate(limit);
    }

    // out_cols built above already reflects projection; trim unused mut.
    let _ = &mut out_cols;
    Ok(ResultSet {
        columns: out_cols,
        rows: out_rows,
    })
}

fn project_rowwise(
    select: &VSelect,
    rows: &[Vec<VValue>],
    table: &VTable,
) -> Result<(Vec<OutColumn>, Vec<Vec<VValue>>), ExecError> {
    // Build output schema.
    let mut out_cols: Vec<OutColumn> = Vec::new();
    for item in &select.projection {
        match item {
            VProj::Star => {
                for col in &table.columns {
                    out_cols.push(OutColumn {
                        name: col.name.to_string(),
                        ty: col.ty,
                    });
                }
            }
            VProj::Expr { expr, alias } => {
                out_cols.push(OutColumn {
                    name: alias.clone().unwrap_or_else(|| projection_name(expr)),
                    ty: infer_type(expr, table),
                });
            }
        }
    }

    let mut out_rows: Vec<Vec<VValue>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut out_row: Vec<VValue> = Vec::with_capacity(out_cols.len());
        for item in &select.projection {
            match item {
                VProj::Star => {
                    out_row.extend_from_slice(row);
                }
                VProj::Expr { expr, .. } => {
                    out_row.push(eval(expr, row, table)?);
                }
            }
        }
        out_rows.push(out_row);
    }
    Ok((out_cols, out_rows))
}

fn project_aggregate(
    select: &VSelect,
    rows: &[Vec<VValue>],
    table: &VTable,
) -> Result<(Vec<OutColumn>, Vec<Vec<VValue>>), ExecError> {
    let mut out_cols: Vec<OutColumn> = Vec::with_capacity(select.projection.len());
    let mut out_row: Vec<VValue> = Vec::with_capacity(select.projection.len());

    for item in &select.projection {
        let VProj::Expr { expr, alias } = item else {
            return Err(ExecError::Unsupported(
                "cannot mix * with aggregate projection on virtual tables".into(),
            ));
        };
        let Expr::Aggregate(agg, arg) = expr else {
            return Err(ExecError::Unsupported(
                "non-aggregate expressions in an aggregate projection are not supported \
                 (use GROUP BY)"
                    .into(),
            ));
        };

        let (value, ty) = compute_aggregate(*agg, arg, rows, table)?;
        out_cols.push(OutColumn {
            name: alias.clone().unwrap_or_else(|| aggregate_name(*agg)),
            ty,
        });
        out_row.push(value);
    }

    Ok((out_cols, vec![out_row]))
}

fn compute_aggregate(
    agg: AggFn,
    arg: &Expr,
    rows: &[Vec<VValue>],
    table: &VTable,
) -> Result<(VValue, VType), ExecError> {
    match agg {
        AggFn::Count => {
            let n = match arg {
                Expr::Star => rows.len() as i64,
                _ => {
                    let mut c: i64 = 0;
                    for row in rows {
                        let v = eval(arg, row, table)?;
                        if !v.is_null() {
                            c += 1;
                        }
                    }
                    c
                }
            };
            Ok((VValue::Int8(n), VType::Int8))
        }
        AggFn::Sum => {
            let mut acc: i64 = 0;
            let mut saw_any = false;
            for row in rows {
                let v = eval(arg, row, table)?;
                if let Some(i) = v.as_i64() {
                    acc = acc.wrapping_add(i);
                    saw_any = true;
                }
            }
            Ok((
                if saw_any {
                    VValue::Int8(acc)
                } else {
                    VValue::Null
                },
                VType::Int8,
            ))
        }
        AggFn::Min | AggFn::Max => {
            let mut best: Option<VValue> = None;
            for row in rows {
                let v = eval(arg, row, table)?;
                if v.is_null() {
                    continue;
                }
                best = Some(match best {
                    None => v,
                    Some(cur) => {
                        let cmp = cur.sql_cmp(&v);
                        let take_new = matches!(
                            (agg, cmp),
                            (AggFn::Min, Some(std::cmp::Ordering::Greater))
                                | (AggFn::Max, Some(std::cmp::Ordering::Less))
                        );
                        if take_new { v } else { cur }
                    }
                });
            }
            let ty = infer_type(arg, table);
            Ok((best.unwrap_or(VValue::Null), ty))
        }
        AggFn::Avg => {
            let mut sum: i64 = 0;
            let mut n: i64 = 0;
            for row in rows {
                let v = eval(arg, row, table)?;
                if let Some(i) = v.as_i64() {
                    sum = sum.wrapping_add(i);
                    n += 1;
                }
            }
            Ok((
                if n == 0 {
                    VValue::Null
                } else {
                    VValue::Int8(sum / n)
                },
                VType::Int8,
            ))
        }
    }
}

fn sort_rows(
    rows: &mut [Vec<VValue>],
    keys: &[(Expr, bool)],
    table: &VTable,
) -> Result<(), ExecError> {
    // Pre-compute each row's sort key tuple.
    let mut indexed: Vec<(usize, Vec<VValue>)> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let mut key = Vec::with_capacity(keys.len());
        for (expr, _) in keys {
            key.push(eval(expr, row, table)?);
        }
        indexed.push((i, key));
    }
    indexed.sort_by(|a, b| {
        for (i, (_, asc)) in keys.iter().enumerate() {
            let ord = match a.1[i].sql_cmp(&b.1[i]) {
                Some(o) => o,
                None => match (a.1[i].is_null(), b.1[i].is_null()) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => std::cmp::Ordering::Equal,
                },
            };
            if ord != std::cmp::Ordering::Equal {
                return if *asc { ord } else { ord.reverse() };
            }
        }
        std::cmp::Ordering::Equal
    });

    // Apply the permutation.
    let original: Vec<Vec<VValue>> = rows.to_vec();
    for (new_pos, (orig_idx, _)) in indexed.into_iter().enumerate() {
        rows[new_pos] = original[orig_idx].clone();
    }
    Ok(())
}

fn projection_name(expr: &Expr) -> String {
    match expr {
        Expr::Column(c) => c.clone(),
        Expr::Aggregate(agg, _) => aggregate_name(*agg),
        _ => "?column?".to_string(),
    }
}

fn aggregate_name(agg: AggFn) -> String {
    match agg {
        AggFn::Count => "count".into(),
        AggFn::Sum => "sum".into(),
        AggFn::Min => "min".into(),
        AggFn::Max => "max".into(),
        AggFn::Avg => "avg".into(),
    }
}

fn infer_type(expr: &Expr, table: &VTable) -> VType {
    match expr {
        Expr::Literal(VValue::Bool(_)) => VType::Bool,
        Expr::Literal(VValue::Int4(_)) => VType::Int4,
        Expr::Literal(VValue::Int8(_)) => VType::Int8,
        Expr::Literal(VValue::Text(_)) => VType::Text,
        Expr::Literal(VValue::Null) => VType::Text,
        Expr::Column(name) => table
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .map(|c| c.ty)
            .unwrap_or(VType::Text),
        Expr::BinaryOp(_, op, _) => match op {
            crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::Eq
            | crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::NotEq
            | crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::Lt
            | crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::LtEq
            | crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::Gt
            | crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::GtEq
            | crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::And
            | crate::control::server::pgwire::pg_catalog::vquery::expr::BinOp::Or => VType::Bool,
            _ => VType::Int8,
        },
        Expr::UnaryNot(_)
        | Expr::IsNull(_, _)
        | Expr::InList(_, _, _)
        | Expr::Between(_, _, _, _)
        | Expr::Like(_, _, _) => VType::Bool,
        Expr::UnaryNeg(_) => VType::Int8,
        Expr::Aggregate(AggFn::Count, _) => VType::Int8,
        Expr::Aggregate(_, e) => infer_type(e, table),
        Expr::Star => VType::Text,
    }
}

#[allow(dead_code)]
fn _vcolumn_marker(_: &VColumn) {}
