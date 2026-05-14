// SPDX-License-Identifier: Apache-2.0

//! Database-DDL dispatch: matches the leading verb tokens and delegates to
//! the per-operation parser. Keep this file thin — the actual parsing logic
//! belongs in the sibling modules.

use crate::ddl_ast::statement::{DatabaseStmt, NodedbStatement};
use crate::error::SqlError;

use super::alter::parse_alter_database;
use super::backup_restore::{parse_backup_database, parse_restore_database};
use super::clone::parse_clone_database;
use super::create::parse_create_database;
use super::drop_db::parse_drop_database;
use super::mirror::{parse_mirror_database, parse_show_database_mirror_status};
use super::move_tenant::parse_move_tenant;
use super::show_extras::{parse_show_database_lineage, parse_show_database_quota_or_usage};
use super::use_db::parse_use_database;

/// Try to parse a database-level DDL statement.
///
/// Returns `None` for SQL that does not match any database-DDL prefix.
/// Returns `Some(Err(...))` for structurally valid database DDL that contains a
/// parse error (e.g. missing required name token).
pub fn try_parse(
    _upper: &str,
    parts: &[&str],
    original: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    let first = parts.first().copied().unwrap_or("");
    let second = parts.get(1).copied().unwrap_or("").to_uppercase();

    match first.to_uppercase().as_str() {
        "CREATE" if second == "DATABASE" => Some(parse_create_database(parts, original)),
        "DROP" if second == "DATABASE" => Some(parse_drop_database(parts)),
        "ALTER" if second == "DATABASE" => Some(parse_alter_database(parts, original)),
        "USE" if second == "DATABASE" => Some(parse_use_database(parts)),
        "CLONE" if second == "DATABASE" => Some(parse_clone_database(parts, original)),
        "MIRROR" if second == "DATABASE" => Some(parse_mirror_database(parts)),
        "MOVE" if second == "TENANT" => Some(parse_move_tenant(parts)),
        "BACKUP" if second == "DATABASE" => Some(parse_backup_database(parts)),
        "RESTORE" if second == "DATABASE" => Some(parse_restore_database(parts)),
        "SHOW" if second == "DATABASES" && parts.len() == 2 => {
            Some(Ok(NodedbStatement::Database(DatabaseStmt::ShowDatabases)))
        }
        // SHOW DATABASE QUOTA FOR <name>
        "SHOW"
            if second == "DATABASE"
                && parts.get(2).map(|w| w.to_uppercase()).as_deref() == Some("QUOTA") =>
        {
            Some(parse_show_database_quota_or_usage(parts, false))
        }
        // SHOW DATABASE USAGE FOR <name>
        "SHOW"
            if second == "DATABASE"
                && parts.get(2).map(|w| w.to_uppercase()).as_deref() == Some("USAGE") =>
        {
            Some(parse_show_database_quota_or_usage(parts, true))
        }
        // SHOW DATABASE LINEAGE FOR <name>
        "SHOW"
            if second == "DATABASE"
                && parts.get(2).map(|w| w.to_uppercase()).as_deref() == Some("LINEAGE") =>
        {
            Some(parse_show_database_lineage(parts))
        }
        // SHOW DATABASE MIRROR STATUS [FOR <name>]
        "SHOW"
            if second == "DATABASE"
                && parts.get(2).map(|w| w.to_uppercase()).as_deref() == Some("MIRROR")
                && parts.get(3).map(|w| w.to_uppercase()).as_deref() == Some("STATUS") =>
        {
            Some(parse_show_database_mirror_status(parts))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_show_databases() {
        let sql = "SHOW DATABASES";
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        let stmt = try_parse(&upper, &parts, sql).unwrap().unwrap();
        assert_eq!(stmt, NodedbStatement::Database(DatabaseStmt::ShowDatabases));
    }
}
