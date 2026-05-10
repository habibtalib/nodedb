// SPDX-License-Identifier: Apache-2.0

//! `MOVE TENANT <tenant> FROM <db_a> TO <db_b>`.

use crate::ddl_ast::statement::NodedbStatement;
use crate::error::SqlError;

pub(super) fn parse_move_tenant(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    let tenant_name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "MOVE TENANT requires a tenant name".into(),
        })?
        .trim_matches('"')
        .to_string();
    let from_idx = parts
        .iter()
        .position(|w| w.to_uppercase() == "FROM")
        .ok_or_else(|| SqlError::Parse {
            detail: "MOVE TENANT requires FROM <db>".into(),
        })?;
    let from_db = parts
        .get(from_idx + 1)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "MOVE TENANT FROM requires a source database name".into(),
        })?
        .trim_matches('"')
        .to_string();
    let to_idx = parts
        .iter()
        .position(|w| w.to_uppercase() == "TO")
        .ok_or_else(|| SqlError::Parse {
            detail: "MOVE TENANT requires TO <db>".into(),
        })?;
    let to_db = parts
        .get(to_idx + 1)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "MOVE TENANT TO requires a destination database name".into(),
        })?
        .trim_matches('"')
        .to_string();

    Ok(NodedbStatement::MoveTenant {
        tenant_name,
        from_db,
        to_db,
    })
}
