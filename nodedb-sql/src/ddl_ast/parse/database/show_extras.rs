// SPDX-License-Identifier: Apache-2.0

//! `SHOW DATABASE QUOTA FOR <name>`, `SHOW DATABASE USAGE FOR <name>`,
//! and `SHOW DATABASE LINEAGE FOR <name>`.

use crate::ddl_ast::statement::NodedbStatement;
use crate::error::SqlError;

/// Parse `SHOW DATABASE QUOTA FOR <name>` or `SHOW DATABASE USAGE FOR <name>`.
/// `is_usage = false` → quota, `is_usage = true` → usage.
pub(super) fn parse_show_database_quota_or_usage(
    parts: &[&str],
    is_usage: bool,
) -> Result<NodedbStatement, SqlError> {
    // SHOW DATABASE {QUOTA|USAGE} FOR <name>
    // parts: [SHOW, DATABASE, QUOTA|USAGE, FOR, <name>]
    let for_idx = parts
        .iter()
        .position(|w| w.eq_ignore_ascii_case("FOR"))
        .ok_or_else(|| SqlError::Parse {
            detail: "SHOW DATABASE QUOTA / USAGE requires FOR <name>".into(),
        })?;
    let name = parts
        .get(for_idx + 1)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "SHOW DATABASE QUOTA / USAGE FOR requires a database name".into(),
        })?
        .trim_matches('"')
        .to_string();

    if is_usage {
        Ok(NodedbStatement::ShowDatabaseUsage { name })
    } else {
        Ok(NodedbStatement::ShowDatabaseQuota { name })
    }
}

pub(super) fn parse_show_database_lineage(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // SHOW DATABASE LINEAGE FOR <name>
    // parts: [SHOW, DATABASE, LINEAGE, FOR, <name>]
    let for_idx = parts
        .iter()
        .position(|w| w.eq_ignore_ascii_case("FOR"))
        .ok_or_else(|| SqlError::Parse {
            detail: "SHOW DATABASE LINEAGE requires FOR <name>".into(),
        })?;
    let name = parts
        .get(for_idx + 1)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "SHOW DATABASE LINEAGE FOR requires a database name".into(),
        })?
        .trim_matches('"')
        .to_string();
    Ok(NodedbStatement::ShowDatabaseLineage { name })
}

#[cfg(test)]
mod tests {
    use super::super::dispatch::try_parse;
    use super::*;

    fn ok(sql: &str) -> NodedbStatement {
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        try_parse(&upper, &parts, sql)
            .expect("expected Some")
            .expect("expected Ok")
    }

    #[test]
    fn parse_show_database_lineage() {
        let stmt = ok("SHOW DATABASE LINEAGE FOR mydb");
        assert_eq!(
            stmt,
            NodedbStatement::ShowDatabaseLineage {
                name: "mydb".into()
            }
        );
    }

    #[test]
    fn parse_show_database_lineage_missing_name_errors() {
        let sql = "SHOW DATABASE LINEAGE FOR";
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        let err = try_parse(&upper, &parts, sql).unwrap().unwrap_err();
        match err {
            SqlError::Parse { detail } => {
                assert!(detail.contains("requires a database name"), "{detail}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
