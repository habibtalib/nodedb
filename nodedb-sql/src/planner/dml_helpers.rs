// SPDX-License-Identifier: Apache-2.0

use sqlparser::ast;

use crate::error::{Result, SqlError};
use crate::parser::normalize::{normalize_ident, normalize_object_name_checked};
use crate::resolver::expr::convert_value;
use crate::types::*;

pub(super) fn convert_value_rows(
    columns: &[String],
    rows: &[Vec<ast::Expr>],
) -> Result<Vec<Vec<(String, SqlValue)>>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, expr)| {
                    let col = columns.get(i).cloned().unwrap_or_else(|| format!("col{i}"));
                    let val = expr_to_sql_value(expr)?;
                    Ok((col, val))
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect()
}

pub(super) fn expr_to_sql_value(expr: &ast::Expr) -> Result<SqlValue> {
    match expr {
        ast::Expr::Value(v) => convert_value(&v.value),
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr: inner,
        } => {
            let val = expr_to_sql_value(inner)?;
            match val {
                SqlValue::Int(n) => Ok(SqlValue::Int(-n)),
                SqlValue::Float(f) => Ok(SqlValue::Float(-f)),
                SqlValue::Decimal(d) => Ok(SqlValue::Decimal(-d)),
                _ => Err(SqlError::TypeMismatch {
                    detail: "cannot negate non-numeric value".into(),
                }),
            }
        }
        ast::Expr::Array(ast::Array { elem, .. }) => {
            let vals = elem.iter().map(expr_to_sql_value).collect::<Result<_>>()?;
            Ok(SqlValue::Array(vals))
        }
        ast::Expr::Function(func) => {
            let func_name = func
                .name
                .0
                .iter()
                .map(|p| match p {
                    ast::ObjectNamePart::Identifier(ident) => normalize_ident(ident),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join(".")
                .to_lowercase();
            match func_name.as_str() {
                "st_point" => {
                    let args = super::select::extract_func_args(func)?;
                    if args.len() >= 2 {
                        let lon = super::select::extract_float(&args[0])?;
                        let lat = super::select::extract_float(&args[1])?;
                        Ok(SqlValue::String(format!(
                            r#"{{"type":"Point","coordinates":[{lon},{lat}]}}"#
                        )))
                    } else {
                        Ok(SqlValue::String(format!("{expr}")))
                    }
                }
                "st_geomfromgeojson" => {
                    let args = super::select::extract_func_args(func)?;
                    if !args.is_empty() {
                        let s = super::select::extract_string_literal(&args[0])?;
                        Ok(SqlValue::String(s))
                    } else {
                        Ok(SqlValue::String(format!("{expr}")))
                    }
                }
                _ => {
                    if let Ok(sql_expr) = crate::resolver::expr::convert_expr(expr)
                        && let Some(v) = super::const_fold::fold_constant_default(&sql_expr)
                    {
                        Ok(v)
                    } else {
                        Ok(SqlValue::String(format!("{expr}")))
                    }
                }
            }
        }
        _ => Err(SqlError::Unsupported {
            detail: format!("value expression: {expr}"),
        }),
    }
}

pub(super) fn extract_table_name_from_table_with_joins(
    table: &ast::TableWithJoins,
) -> Result<String> {
    match &table.relation {
        ast::TableFactor::Table { name, .. } => Ok(normalize_object_name_checked(name)?),
        _ => Err(SqlError::Unsupported {
            detail: "non-table target in DML".into(),
        }),
    }
}

/// Extract point-operation keys from WHERE clause (WHERE pk = literal OR pk IN (...)).
pub fn extract_point_keys(selection: Option<&ast::Expr>, info: &CollectionInfo) -> Vec<SqlValue> {
    let pk = match &info.primary_key {
        Some(pk) => pk.clone(),
        None => return Vec::new(),
    };

    let expr = match selection {
        Some(e) => e,
        None => return Vec::new(),
    };

    let mut keys = Vec::new();
    collect_pk_equalities(expr, &pk, &mut keys);
    keys
}

fn collect_pk_equalities(expr: &ast::Expr, pk: &str, keys: &mut Vec<SqlValue>) {
    match expr {
        ast::Expr::BinaryOp {
            left,
            op: ast::BinaryOperator::Eq,
            right,
        } => {
            if is_column(left, pk)
                && let Ok(v) = expr_to_sql_value(right)
            {
                keys.push(v);
            } else if is_column(right, pk)
                && let Ok(v) = expr_to_sql_value(left)
            {
                keys.push(v);
            }
        }
        ast::Expr::BinaryOp {
            left,
            op: ast::BinaryOperator::Or,
            right,
        } => {
            collect_pk_equalities(left, pk, keys);
            collect_pk_equalities(right, pk, keys);
        }
        ast::Expr::InList {
            expr: inner,
            list,
            negated: false,
        } if is_column(inner, pk) => {
            for item in list {
                if let Ok(v) = expr_to_sql_value(item) {
                    keys.push(v);
                }
            }
        }
        _ => {}
    }
}

fn is_column(expr: &ast::Expr, name: &str) -> bool {
    match expr {
        ast::Expr::Identifier(ident) => normalize_ident(ident) == name,
        // Three or more parts: schema.table.col — never matches a plain pk name.
        ast::Expr::CompoundIdentifier(parts) if parts.len() >= 3 => false,
        ast::Expr::CompoundIdentifier(parts) if parts.len() == 2 => {
            normalize_ident(&parts[1]) == name
        }
        _ => false,
    }
}

/// Build a `SqlPlan::VectorPrimaryInsert` from parsed rows.
///
/// Extracts the vector-field column into `vector: Vec<f32>` and collects
/// all remaining columns into `payload_fields`. Rows missing the vector
/// column are rejected.
pub(super) fn build_vector_primary_insert_plan(
    collection: &str,
    vpc: &nodedb_types::VectorPrimaryConfig,
    _columns: &[String],
    rows: Vec<Vec<(String, SqlValue)>>,
) -> Result<Vec<SqlPlan>> {
    let mut result_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let mut vector: Option<Vec<f32>> = None;
        let mut payload_fields = std::collections::HashMap::new();

        for (col, val) in row {
            if col == vpc.vector_field {
                match val {
                    SqlValue::Array(items) => {
                        let floats: Result<Vec<f32>> = items
                            .iter()
                            .map(|v| match v {
                                SqlValue::Float(f) => Ok(*f as f32),
                                SqlValue::Int(i) => Ok(*i as f32),
                                SqlValue::Decimal(d) => {
                                    use rust_decimal::prelude::ToPrimitive;
                                    d.to_f32().ok_or_else(|| SqlError::Parse {
                                        detail: format!(
                                            "vector element decimal '{d}' is out of f32 range"
                                        ),
                                    })
                                }
                                other => Err(SqlError::Parse {
                                    detail: format!(
                                        "vector field must contain numbers, got {other:?}"
                                    ),
                                }),
                            })
                            .collect();
                        vector = Some(floats?);
                    }
                    other => {
                        return Err(SqlError::Parse {
                            detail: format!(
                                "vector field '{}' must be an array literal, got {other:?}",
                                vpc.vector_field
                            ),
                        });
                    }
                }
            } else {
                payload_fields.insert(col, val);
            }
        }

        let vector = vector.ok_or_else(|| SqlError::Parse {
            detail: format!(
                "vector-primary INSERT missing required vector field '{}'",
                vpc.vector_field
            ),
        })?;

        result_rows.push(VectorPrimaryRow {
            surrogate: nodedb_types::Surrogate::ZERO,
            vector,
            payload_fields,
        });
    }

    Ok(vec![SqlPlan::VectorPrimaryInsert {
        collection: collection.to_string(),
        field: vpc.vector_field.clone(),
        quantization: vpc.quantization,
        payload_indexes: vpc.payload_indexes.clone(),
        rows: result_rows,
    }])
}

/// Build a `SqlPlan::KvInsert` from a VALUES clause. Shared by plain INSERT,
/// UPSERT, and `INSERT ... ON CONFLICT (key) DO UPDATE` — the three paths
/// differ only in `intent` and `on_conflict_updates`, never in how entries
/// are extracted from the row exprs.
///
/// `pk_col` is the schema-defined primary-key column name from
/// `CollectionInfo::primary_key`.  When supplied, that column is used as
/// the KV key regardless of whether it is named `"key"`.  Falls back to
/// the literal name `"key"` when `pk_col` is `None` (legacy / generic
/// KV collections that use the built-in key/value column convention).
pub(super) fn build_kv_insert_plan(
    table_name: String,
    columns: &[String],
    rows_ast: &[Vec<ast::Expr>],
    intent: KvInsertIntent,
    on_conflict_updates: Vec<(String, SqlExpr)>,
    pk_col: Option<&str>,
) -> Result<Vec<SqlPlan>> {
    let key_col_name = pk_col.unwrap_or("key");
    let key_idx = columns.iter().position(|c| c == key_col_name);
    let ttl_idx = columns.iter().position(|c| c == "ttl");
    // When using a named primary-key column (e.g. `k STRING PRIMARY KEY`), we
    // store the key bytes in the KV key slot AND also keep the column in the
    // value map.  This allows scan filters on the primary-key column (e.g.
    // `WHERE k = 'x'`) and projection (e.g. `SELECT k FROM ...`) to work
    // without teaching the KV scan handler to inspect the raw key bytes.
    // The only column we exclude from the value map is the built-in `"key"`
    // sentinel (used by raw key/value KV collections) and `"ttl"`.
    let exclude_from_value: std::collections::HashSet<usize> = {
        let mut s = std::collections::HashSet::new();
        // Exclude the raw "key" sentinel column (not a named PK column).
        if key_col_name == "key"
            && let Some(idx) = key_idx
        {
            s.insert(idx);
        }
        if let Some(idx) = ttl_idx {
            s.insert(idx);
        }
        s
    };
    let mut entries = Vec::with_capacity(rows_ast.len());
    let mut ttl_secs: u64 = 0;
    for row_exprs in rows_ast {
        let key_val = match key_idx {
            Some(idx) => expr_to_sql_value(&row_exprs[idx])?,
            None => SqlValue::String(String::new()),
        };
        if let Some(idx) = ttl_idx {
            match expr_to_sql_value(&row_exprs[idx]) {
                Ok(SqlValue::Int(n)) => ttl_secs = n.max(0) as u64,
                Ok(SqlValue::Float(f)) => ttl_secs = f.max(0.0) as u64,
                _ => {}
            }
        }
        let value_cols: Vec<(String, SqlValue)> = columns
            .iter()
            .enumerate()
            .filter(|(i, _)| !exclude_from_value.contains(i))
            .map(|(i, col)| {
                let val = expr_to_sql_value(&row_exprs[i])?;
                Ok((col.clone(), val))
            })
            .collect::<Result<Vec<_>>>()?;
        entries.push((key_val, value_cols));
    }
    Ok(vec![SqlPlan::KvInsert {
        collection: table_name,
        entries,
        ttl_secs,
        intent,
        on_conflict_updates,
    }])
}
