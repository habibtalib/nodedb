// SPDX-License-Identifier: Apache-2.0

//! Parser for tenant-scoped DDL statements.
//!
//! Handles:
//! - `ALTER TENANT <name> IN DATABASE <db> SET QUOTA (...)`
//! - `SHOW TENANT QUOTA FOR <name> IN DATABASE <db>`
//! - `SHOW TENANT USAGE FOR <name> IN DATABASE <db>`

use crate::ddl_ast::statement::{AlterTenantOperation, DatabaseStmt, NodedbStatement};
use crate::error::SqlError;

use super::database::parse_quota_spec;

/// Try to parse a tenant DDL statement from the upper-cased token slice.
///
/// Returns `None` if the statement is not a recognized tenant DDL form.
/// Returns `Some(Err(...))` on a syntax error in an otherwise-recognized form.
pub(super) fn try_parse(
    upper: &str,
    parts: &[&str],
    original: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    match parts.first().map(|s| s.to_uppercase()).as_deref() {
        Some("ALTER") => try_parse_alter_tenant(upper, parts, original),
        Some("SHOW") => try_parse_show_tenant(parts),
        _ => None,
    }
}

// ── ALTER TENANT ──────────────────────────────────────────────────────────────

fn try_parse_alter_tenant(
    _upper: &str,
    parts: &[&str],
    original: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    // ALTER TENANT <name> IN DATABASE <db> SET QUOTA (...)
    // parts: [ALTER, TENANT, <name>, IN, DATABASE, <db>, SET, QUOTA, ...]
    if parts.len() < 3 {
        return None;
    }
    if !parts[1].eq_ignore_ascii_case("TENANT") {
        return None;
    }

    // Must have IN DATABASE at positions 3,4 (0-indexed).
    if parts.len() < 8 {
        return None;
    }
    if !parts[3].eq_ignore_ascii_case("IN") || !parts[4].eq_ignore_ascii_case("DATABASE") {
        return None;
    }
    if !parts[6].eq_ignore_ascii_case("SET") || !parts[7].eq_ignore_ascii_case("QUOTA") {
        return None;
    }

    let name = parts[2].to_string();
    let database = parts[5].to_string();

    let spec = match parse_quota_spec(original, "ALTER TENANT IN DATABASE SET QUOTA") {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };

    Some(Ok(NodedbStatement::Database(DatabaseStmt::AlterTenant {
        name,
        database,
        operation: AlterTenantOperation::SetQuota(spec),
    })))
}

// ── SHOW TENANT ───────────────────────────────────────────────────────────────

fn try_parse_show_tenant(parts: &[&str]) -> Option<Result<NodedbStatement, SqlError>> {
    // SHOW TENANT QUOTA FOR <name> IN DATABASE <db>
    // SHOW TENANT USAGE FOR <name> IN DATABASE <db>
    // parts: [SHOW, TENANT, QUOTA|USAGE, FOR, <name>, IN, DATABASE, <db>]
    if parts.len() < 2 || !parts[1].eq_ignore_ascii_case("TENANT") {
        return None;
    }
    if parts.len() < 3 {
        return None;
    }

    let is_quota = parts[2].eq_ignore_ascii_case("QUOTA");
    let is_usage = parts[2].eq_ignore_ascii_case("USAGE");
    if !is_quota && !is_usage {
        return None;
    }

    // Must have: FOR <name> IN DATABASE <db>
    if parts.len() < 8 {
        return Some(Err(SqlError::Parse {
            detail: format!(
                "expected: SHOW TENANT {kw} FOR <name> IN DATABASE <db>",
                kw = parts[2].to_uppercase()
            ),
        }));
    }
    if !parts[3].eq_ignore_ascii_case("FOR") {
        return Some(Err(SqlError::Parse {
            detail: "expected FOR after SHOW TENANT QUOTA/USAGE".into(),
        }));
    }
    if !parts[5].eq_ignore_ascii_case("IN") || !parts[6].eq_ignore_ascii_case("DATABASE") {
        return Some(Err(SqlError::Parse {
            detail: "expected IN DATABASE <db> after tenant name".into(),
        }));
    }

    let name = parts[4].to_string();
    let database = parts[7].to_string();

    if is_quota {
        Some(Ok(NodedbStatement::Database(
            DatabaseStmt::ShowTenantQuotaInDatabase { name, database },
        )))
    } else {
        Some(Ok(NodedbStatement::Database(
            DatabaseStmt::ShowTenantUsageInDatabase { name, database },
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl_ast::statement::AlterTenantOperation;

    fn parse(sql: &str) -> Option<Result<NodedbStatement, SqlError>> {
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        try_parse(&upper, &parts, sql)
    }

    fn ok(sql: &str) -> NodedbStatement {
        parse(sql)
            .expect("expected Some, got None")
            .expect("expected Ok, got Err")
    }

    #[test]
    fn alter_tenant_set_quota() {
        let stmt = ok(
            "ALTER TENANT acme IN DATABASE production SET QUOTA (max_memory_bytes = 1073741824)",
        );
        match stmt {
            NodedbStatement::Database(DatabaseStmt::AlterTenant {
                name,
                database,
                operation: AlterTenantOperation::SetQuota(spec),
            }) => {
                assert_eq!(name, "acme");
                assert_eq!(database, "production");
                assert_eq!(spec.max_memory_bytes, Some(1_073_741_824));
            }
            other => panic!("expected AlterTenant, got {other:?}"),
        }
    }

    #[test]
    fn show_tenant_quota_in_database() {
        let stmt = ok("SHOW TENANT QUOTA FOR acme IN DATABASE production");
        assert_eq!(
            stmt,
            NodedbStatement::Database(DatabaseStmt::ShowTenantQuotaInDatabase {
                name: "acme".into(),
                database: "production".into(),
            })
        );
    }

    #[test]
    fn show_tenant_usage_in_database() {
        let stmt = ok("SHOW TENANT USAGE FOR acme IN DATABASE production");
        assert_eq!(
            stmt,
            NodedbStatement::Database(DatabaseStmt::ShowTenantUsageInDatabase {
                name: "acme".into(),
                database: "production".into(),
            })
        );
    }

    #[test]
    fn non_tenant_returns_none() {
        assert!(parse("ALTER DATABASE foo RENAME TO bar").is_none());
        assert!(parse("SHOW DATABASES").is_none());
        assert!(parse("SELECT 1").is_none());
    }

    #[test]
    fn show_tenant_quota_missing_in_database() {
        let result = parse("SHOW TENANT QUOTA FOR acme");
        assert!(matches!(result, Some(Err(SqlError::Parse { .. }))));
    }
}
