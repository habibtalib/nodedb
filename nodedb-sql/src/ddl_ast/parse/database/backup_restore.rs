// SPDX-License-Identifier: Apache-2.0

//! `BACKUP DATABASE <name> TO <uri>` and `RESTORE DATABASE <name> FROM <uri>`.

use crate::ddl_ast::statement::NodedbStatement;
use crate::error::SqlError;

pub(super) fn parse_backup_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    let name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "BACKUP DATABASE requires a name".into(),
        })?
        .trim_matches('"')
        .to_string();
    let to_idx = parts
        .iter()
        .position(|w| w.to_uppercase() == "TO")
        .ok_or_else(|| SqlError::Parse {
            detail: "BACKUP DATABASE requires TO <uri>".into(),
        })?;
    let uri = parts[to_idx + 1..].join(" ").trim_matches('\'').to_string();

    Ok(NodedbStatement::BackupDatabase { name, uri })
}

pub(super) fn parse_restore_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    let name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "RESTORE DATABASE requires a name".into(),
        })?
        .trim_matches('"')
        .to_string();
    let from_idx = parts
        .iter()
        .position(|w| w.to_uppercase() == "FROM")
        .ok_or_else(|| SqlError::Parse {
            detail: "RESTORE DATABASE requires FROM <uri>".into(),
        })?;
    let uri = parts[from_idx + 1..]
        .join(" ")
        .trim_matches('\'')
        .to_string();

    Ok(NodedbStatement::RestoreDatabase { name, uri })
}
