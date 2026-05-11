// SPDX-License-Identifier: Apache-2.0

//! `WINDOW <name> AS (...)` clause resolution.
//!
//! Collects the named definitions, follows `WINDOW a AS b` alias chains (with
//! cycle detection), and flattens an inline-or-named `WindowSpec` into a
//! concrete partition/order/frame triple per the SQL window-inheritance rules:
//! the referenced window supplies `PARTITION BY` (the referencing spec must not
//! add its own), the referencing spec may add `ORDER BY` (only if the
//! referenced one has none) and the frame (the referenced one must have none).

use std::collections::HashMap;

use sqlparser::ast;

use crate::error::{Result, SqlError};
use crate::parser::normalize::normalize_ident;

/// A window spec with every `WINDOW <name> AS (...)` reference resolved
/// and merged: just partition / order / frame.
pub(super) struct FlatWindow {
    pub(super) partition_by: Vec<ast::Expr>,
    pub(super) order_by: Vec<ast::OrderByExpr>,
    pub(super) frame: Option<ast::WindowFrame>,
}

/// Build the `WINDOW <name> AS <expr>` definition map (names normalised).
/// References between definitions are resolved lazily on use.
pub(super) fn collect_named_windows(
    defs: &[ast::NamedWindowDefinition],
) -> Result<HashMap<String, &ast::NamedWindowExpr>> {
    let mut map: HashMap<String, &ast::NamedWindowExpr> = HashMap::with_capacity(defs.len());
    for ast::NamedWindowDefinition(ident, expr) in defs {
        let name = normalize_ident(ident);
        if map.insert(name.clone(), expr).is_some() {
            return Err(SqlError::Unsupported {
                detail: format!("duplicate WINDOW definition '{name}'"),
            });
        }
    }
    Ok(map)
}

/// Resolve a named-window reference to its concrete `WindowSpec`, following
/// `WINDOW a AS b` aliases. `seen` tracks the alias chain for cycle
/// detection. Errors on undefined names and cycles.
pub(super) fn resolve_named_def<'a>(
    name: &str,
    named: &HashMap<String, &'a ast::NamedWindowExpr>,
    seen: &mut Vec<String>,
) -> Result<&'a ast::WindowSpec> {
    let expr = *named.get(name).ok_or_else(|| SqlError::InvalidFunction {
        detail: format!("window '{name}' is not defined in the WINDOW clause"),
    })?;
    match expr {
        ast::NamedWindowExpr::WindowSpec(spec) => Ok(spec),
        ast::NamedWindowExpr::NamedWindow(other) => {
            let other = normalize_ident(other);
            if seen.contains(&other) {
                return Err(SqlError::Unsupported {
                    detail: format!("circular WINDOW definition involving '{other}'"),
                });
            }
            seen.push(other.clone());
            resolve_named_def(&other, named, seen)
        }
    }
}

/// Flatten an inline-or-named `WindowSpec`, merging in any referenced window
/// per the SQL inheritance rules (see module docs).
pub(super) fn flatten_window_spec<'a>(
    spec: &'a ast::WindowSpec,
    named: &HashMap<String, &'a ast::NamedWindowExpr>,
    seen: &mut Vec<String>,
) -> Result<FlatWindow> {
    let Some(ref_ident) = &spec.window_name else {
        return Ok(FlatWindow {
            partition_by: spec.partition_by.clone(),
            order_by: spec.order_by.clone(),
            frame: spec.window_frame.clone(),
        });
    };
    let ref_name = normalize_ident(ref_ident);
    if seen.contains(&ref_name) {
        return Err(SqlError::Unsupported {
            detail: format!("circular WINDOW definition involving '{ref_name}'"),
        });
    }
    if !spec.partition_by.is_empty() {
        return Err(SqlError::Unsupported {
            detail: format!(
                "window referencing '{ref_name}' cannot also declare its own PARTITION BY"
            ),
        });
    }
    seen.push(ref_name.clone());
    let base_spec = resolve_named_def(&ref_name, named, seen)?;
    let base = flatten_window_spec(base_spec, named, seen)?;
    if base.frame.is_some() {
        return Err(SqlError::Unsupported {
            detail: format!(
                "window '{ref_name}' declares a frame clause and cannot be referenced by another window"
            ),
        });
    }
    let order_by = if spec.order_by.is_empty() {
        base.order_by
    } else if base.order_by.is_empty() {
        spec.order_by.clone()
    } else {
        return Err(SqlError::Unsupported {
            detail: format!(
                "window referencing '{ref_name}' cannot add ORDER BY because the referenced window already has one"
            ),
        });
    };
    Ok(FlatWindow {
        partition_by: base.partition_by,
        order_by,
        frame: spec.window_frame.clone(),
    })
}
