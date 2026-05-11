// SPDX-License-Identifier: Apache-2.0

//! Conversion of sqlparser window-frame ASTs to the executor's `WindowFrame`.
//!
//! Includes the semantic checks PostgreSQL enforces at plan time: `GROUPS`
//! mode requires an `ORDER BY`, and `RANGE` with a numeric `PRECEDING`/
//! `FOLLOWING` offset requires exactly one `ORDER BY` column.

use sqlparser::ast;

use crate::error::{Result, SqlError};
use crate::types::SortKey;
use nodedb_query::{FrameBound, WindowFrame};

/// Convert a sqlparser `WindowFrame` to the executor's `WindowFrame`.
///
/// `order_by` is needed for semantic validation:
/// - GROUPS without ORDER BY is invalid (PostgreSQL parity).
/// - RANGE with numeric offsets (Preceding(N)/Following(N)) requires a single
///   numeric ORDER BY column; without one the semantics are undefined and we
///   reject at plan time.
pub(super) fn convert_window_frame(
    frame: &ast::WindowFrame,
    order_by: &[SortKey],
) -> Result<WindowFrame> {
    let mode = match frame.units {
        ast::WindowFrameUnits::Rows => "rows",
        ast::WindowFrameUnits::Range => "range",
        ast::WindowFrameUnits::Groups => {
            if order_by.is_empty() {
                return Err(SqlError::InvalidWindowFrame {
                    detail: "GROUPS mode requires an ORDER BY clause in the window specification"
                        .into(),
                });
            }
            "groups"
        }
    };

    let start = convert_window_frame_bound(&frame.start_bound)?;
    let end = match &frame.end_bound {
        Some(b) => convert_window_frame_bound(b)?,
        None => FrameBound::CurrentRow,
    };

    // RANGE with numeric offsets requires a single-column ORDER BY so we can
    // compare values. Reject if ORDER BY is absent or has more than one key
    // (multi-key RANGE offsets are undefined in SQL standards).
    if mode == "range" {
        let needs_order = matches!(start, FrameBound::Preceding(n) if n > 0)
            || matches!(start, FrameBound::Following(n) if n > 0)
            || matches!(end, FrameBound::Preceding(n) if n > 0)
            || matches!(end, FrameBound::Following(n) if n > 0);
        if needs_order && order_by.len() != 1 {
            return Err(SqlError::InvalidWindowFrame {
                detail: "RANGE with numeric PRECEDING/FOLLOWING offset requires exactly one ORDER BY column".into(),
            });
        }
    }

    Ok(WindowFrame {
        mode: mode.into(),
        start,
        end,
    })
}

fn convert_window_frame_bound(bound: &ast::WindowFrameBound) -> Result<FrameBound> {
    match bound {
        ast::WindowFrameBound::CurrentRow => Ok(FrameBound::CurrentRow),
        ast::WindowFrameBound::Preceding(None) => Ok(FrameBound::UnboundedPreceding),
        ast::WindowFrameBound::Following(None) => Ok(FrameBound::UnboundedFollowing),
        ast::WindowFrameBound::Preceding(Some(expr)) => {
            Ok(FrameBound::Preceding(extract_frame_offset(expr)?))
        }
        ast::WindowFrameBound::Following(Some(expr)) => {
            Ok(FrameBound::Following(extract_frame_offset(expr)?))
        }
    }
}

/// Parse a frame offset from a literal. Only non-negative integer literals that
/// fit in a `u64` are accepted; anything else (expression, float, oversized
/// literal that fails to parse) is rejected at plan time rather than silently
/// truncated.
fn extract_frame_offset(expr: &ast::Expr) -> Result<u64> {
    if let ast::Expr::Value(v) = expr
        && let ast::Value::Number(n, _) = &v.value
        && let Ok(parsed) = n.parse::<u64>()
    {
        return Ok(parsed);
    }
    Err(SqlError::Unsupported {
        detail: format!("window frame offset must be a non-negative integer literal, got {expr}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_offset_rejects_non_integer_and_oversized_literals() {
        // Float literal — not an integer offset.
        let float_lit = ast::Expr::Value(ast::Value::Number("1.5".into(), false).into());
        assert!(extract_frame_offset(&float_lit).is_err());

        // Literal larger than u64::MAX — must error, never wrap/truncate.
        let huge =
            ast::Expr::Value(ast::Value::Number("99999999999999999999999999".into(), false).into());
        assert!(extract_frame_offset(&huge).is_err());

        // u64::MAX itself parses fine.
        let max = ast::Expr::Value(ast::Value::Number(u64::MAX.to_string(), false).into());
        assert_eq!(extract_frame_offset(&max).unwrap(), u64::MAX);
    }
}
