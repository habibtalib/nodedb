// SPDX-License-Identifier: Apache-2.0

//! Parser for database DDL:
//!   CREATE DATABASE, DROP DATABASE, ALTER DATABASE,
//!   SHOW DATABASES, USE DATABASE,
//!   CLONE DATABASE, MIRROR DATABASE, MOVE TENANT,
//!   BACKUP DATABASE, RESTORE DATABASE.

use crate::ddl_ast::statement::{AlterDatabaseOperation, NodedbStatement};
use crate::error::SqlError;

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
        "ALTER" if second == "DATABASE" => Some(parse_alter_database(parts)),
        "USE" if second == "DATABASE" => Some(parse_use_database(parts)),
        "CLONE" if second == "DATABASE" => Some(parse_clone_database(parts, original)),
        "MIRROR" if second == "DATABASE" => Some(parse_mirror_database(parts)),
        "MOVE" if second == "TENANT" => Some(parse_move_tenant(parts)),
        "BACKUP" if second == "DATABASE" => Some(parse_backup_database(parts)),
        "RESTORE" if second == "DATABASE" => Some(parse_restore_database(parts)),
        "SHOW" if second == "DATABASES" && parts.len() == 2 => {
            // "SHOW DATABASES" only. Other `SHOW DATABASE ...` shapes are
            // intentionally unhandled and fall through to other parsers.
            Some(Ok(NodedbStatement::ShowDatabases))
        }
        _ => None,
    }
}

// ── CREATE DATABASE ─────────────────────────────────────────────────────────

fn parse_create_database(parts: &[&str], original: &str) -> Result<NodedbStatement, SqlError> {
    // CREATE DATABASE [IF NOT EXISTS] <name> [WITH (...)]
    let mut idx = 2usize; // skip CREATE DATABASE
    let if_not_exists = if parts.get(idx).map(|w| w.to_uppercase()).as_deref() == Some("IF")
        && parts.get(idx + 1).map(|w| w.to_uppercase()).as_deref() == Some("NOT")
        && parts.get(idx + 2).map(|w| w.to_uppercase()).as_deref() == Some("EXISTS")
    {
        idx += 3;
        true
    } else {
        false
    };

    let name = parts
        .get(idx)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "CREATE DATABASE requires a name".into(),
        })?
        .to_string();
    let name = name.trim_matches('"').to_string();

    // Parse optional WITH (...) clause — collect key=value pairs.
    let options = parse_with_options(original);

    Ok(NodedbStatement::CreateDatabase {
        name,
        if_not_exists,
        options,
    })
}

// ── DROP DATABASE ───────────────────────────────────────────────────────────

fn parse_drop_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // DROP DATABASE [IF EXISTS] <name> [CASCADE | FORCE]
    //
    // FORCE is accepted as a synonym for CASCADE: both set `cascade = true`.
    // Any token after the name that is not CASCADE/FORCE is a parse error —
    // silently ignoring trailing garbage masks typos in user SQL.
    let mut idx = 2usize;
    let if_exists = if parts.get(idx).map(|w| w.to_uppercase()).as_deref() == Some("IF")
        && parts.get(idx + 1).map(|w| w.to_uppercase()).as_deref() == Some("EXISTS")
    {
        idx += 2;
        true
    } else {
        false
    };

    let name = parts
        .get(idx)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "DROP DATABASE requires a name".into(),
        })?
        .to_string();
    let name = name.trim_matches('"').to_string();
    idx += 1;

    let mut cascade = false;
    for w in &parts[idx..] {
        match w.to_uppercase().as_str() {
            "CASCADE" | "FORCE" => cascade = true,
            other => {
                return Err(SqlError::Parse {
                    detail: format!("DROP DATABASE: unexpected token '{other}'"),
                });
            }
        }
    }

    Ok(NodedbStatement::DropDatabase {
        name,
        if_exists,
        cascade,
    })
}

// ── ALTER DATABASE ──────────────────────────────────────────────────────────

fn parse_alter_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // ALTER DATABASE <name> { RENAME TO <new> | SET QUOTA (<id>) | SET DEFAULT |
    //                         MATERIALIZE | PROMOTE }
    let name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "ALTER DATABASE requires a name".into(),
        })?
        .trim_matches('"')
        .to_string();

    let verb = parts
        .get(3)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "ALTER DATABASE requires an operation keyword".into(),
        })?
        .to_uppercase();

    let operation = match verb.as_str() {
        "RENAME" => {
            // RENAME TO <new_name>
            let to_kw = parts.get(4).map(|w| w.to_uppercase()).unwrap_or_default();
            if to_kw != "TO" {
                return Err(SqlError::Parse {
                    detail: format!(
                        "ALTER DATABASE RENAME requires keyword 'TO', got '{to_kw}'"
                    ),
                });
            }
            let new_name = parts
                .get(5)
                .copied()
                .ok_or_else(|| SqlError::Parse {
                    detail: "ALTER DATABASE RENAME TO requires a new name".into(),
                })?
                .trim_matches('"')
                .to_string();
            AlterDatabaseOperation::Rename { new_name }
        }
        "SET" => {
            let target = parts.get(4).map(|w| w.to_uppercase()).unwrap_or_default();
            match target.as_str() {
                "QUOTA" => {
                    // SET QUOTA (<quota_id>)  or  SET QUOTA <quota_id>
                    let raw = parts.get(5).copied().unwrap_or("0");
                    let raw = raw.trim_matches(|c| c == '(' || c == ')');
                    let quota_id = raw.parse::<u64>().map_err(|_| SqlError::Parse {
                        detail: format!("ALTER DATABASE SET QUOTA: invalid quota id '{raw}'"),
                    })?;
                    AlterDatabaseOperation::SetQuota { quota_id }
                }
                "DEFAULT" => AlterDatabaseOperation::SetDefault,
                other => {
                    return Err(SqlError::Parse {
                        detail: format!("ALTER DATABASE SET: unknown target '{other}'"),
                    });
                }
            }
        }
        "MATERIALIZE" => AlterDatabaseOperation::Materialize,
        "PROMOTE" => AlterDatabaseOperation::Promote,
        other => {
            return Err(SqlError::Parse {
                detail: format!("ALTER DATABASE: unknown operation '{other}'"),
            });
        }
    };

    Ok(NodedbStatement::AlterDatabase { name, operation })
}

// ── USE DATABASE ─────────────────────────────────────────────────────────────

fn parse_use_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    let name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "USE DATABASE requires a name".into(),
        })?
        .trim_matches('"')
        .to_string();
    Ok(NodedbStatement::UseDatabase { name })
}

// ── CLONE DATABASE ───────────────────────────────────────────────────────────

fn parse_clone_database(parts: &[&str], upper: &str) -> Result<NodedbStatement, SqlError> {
    // CLONE DATABASE <new> FROM <source> [AS OF SYSTEM TIME <ms> | LATEST]
    let new_name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "CLONE DATABASE requires a target name".into(),
        })?
        .trim_matches('"')
        .to_string();
    // find FROM
    let from_idx = parts
        .iter()
        .position(|w| w.to_uppercase() == "FROM")
        .ok_or_else(|| SqlError::Parse {
            detail: "CLONE DATABASE requires FROM <source>".into(),
        })?;
    let source_name = parts
        .get(from_idx + 1)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "CLONE DATABASE FROM requires a source name".into(),
        })?
        .trim_matches('"')
        .to_string();

    // AS OF SYSTEM TIME <ms>
    let as_of_ms = if upper.contains("AS OF SYSTEM TIME") {
        let aost_idx = parts
            .iter()
            .position(|w| w.to_uppercase() == "AS")
            .and_then(|i| {
                if parts.get(i + 1).map(|w| w.to_uppercase()).as_deref() == Some("OF")
                    && parts.get(i + 2).map(|w| w.to_uppercase()).as_deref() == Some("SYSTEM")
                    && parts.get(i + 3).map(|w| w.to_uppercase()).as_deref() == Some("TIME")
                {
                    Some(i + 4)
                } else {
                    None
                }
            });
        if let Some(ms_idx) = aost_idx {
            if let Some(ms_str) = parts.get(ms_idx) {
                ms_str.parse::<u64>().ok()
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(NodedbStatement::CloneDatabase {
        new_name,
        source_name,
        as_of_ms,
    })
}

// ── MIRROR DATABASE ──────────────────────────────────────────────────────────

fn parse_mirror_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // MIRROR DATABASE <replica> FROM <source> [MODE = sync | async]
    let replica_name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "MIRROR DATABASE requires a replica name".into(),
        })?
        .trim_matches('"')
        .to_string();
    let from_idx = parts
        .iter()
        .position(|w| w.to_uppercase() == "FROM")
        .ok_or_else(|| SqlError::Parse {
            detail: "MIRROR DATABASE requires FROM <source>".into(),
        })?;
    let source_name = parts
        .get(from_idx + 1)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "MIRROR DATABASE FROM requires a source name".into(),
        })?
        .trim_matches('"')
        .to_string();

    // MODE = sync | async (optional; default "async")
    let mode = parts
        .windows(3)
        .find(|w| w[0].to_uppercase() == "MODE" && w[1] == "=")
        .map(|w| w[2].to_lowercase())
        .unwrap_or_else(|| "async".to_string());

    Ok(NodedbStatement::MirrorDatabase {
        replica_name,
        source_name,
        mode,
    })
}

// ── MOVE TENANT ──────────────────────────────────────────────────────────────

fn parse_move_tenant(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // MOVE TENANT <tenant> FROM <db_a> TO <db_b>
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

// ── BACKUP DATABASE ──────────────────────────────────────────────────────────

fn parse_backup_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // BACKUP DATABASE <name> TO <uri>
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

// ── RESTORE DATABASE ─────────────────────────────────────────────────────────

fn parse_restore_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // RESTORE DATABASE <name> FROM <uri>
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

// ── helpers ──────────────────────────────────────────────────────────────────

/// Extract `WITH (key=value, ...)` pairs from a raw SQL string.
/// Returns an empty vec if no WITH clause is present.
fn parse_with_options(sql: &str) -> Vec<(String, String)> {
    let upper = sql.to_uppercase();
    let with_start = match upper.find("WITH") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let after = &sql[with_start + 4..];
    let paren_start = match after.find('(') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let inner = &after[paren_start + 1..];
    let paren_end = match inner.find(')') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let inner = &inner[..paren_end];
    inner
        .split(',')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = it.next()?.trim().to_string();
            let v = it
                .next()
                .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
                .unwrap_or_default();
            if k.is_empty() { None } else { Some((k, v)) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(sql: &str) -> NodedbStatement {
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        try_parse(&upper, &parts, sql)
            .expect("expected Some")
            .expect("expected Ok")
    }

    #[test]
    fn parse_create_database_simple() {
        let stmt = ok("CREATE DATABASE mydb");
        assert_eq!(
            stmt,
            NodedbStatement::CreateDatabase {
                name: "mydb".into(),
                if_not_exists: false,
                options: vec![],
            }
        );
    }

    #[test]
    fn parse_create_database_if_not_exists() {
        let stmt = ok("CREATE DATABASE IF NOT EXISTS mydb");
        match stmt {
            NodedbStatement::CreateDatabase {
                name,
                if_not_exists,
                ..
            } => {
                assert_eq!(name, "mydb");
                assert!(if_not_exists);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_drop_database_cascade() {
        let stmt = ok("DROP DATABASE mydb CASCADE");
        assert_eq!(
            stmt,
            NodedbStatement::DropDatabase {
                name: "mydb".into(),
                if_exists: false,
                cascade: true,
            }
        );
    }

    #[test]
    fn parse_drop_database_force_is_cascade() {
        let stmt = ok("DROP DATABASE mydb FORCE");
        assert_eq!(
            stmt,
            NodedbStatement::DropDatabase {
                name: "mydb".into(),
                if_exists: false,
                cascade: true,
            }
        );
    }

    #[test]
    fn parse_drop_database_if_exists() {
        let stmt = ok("DROP DATABASE IF EXISTS mydb");
        assert_eq!(
            stmt,
            NodedbStatement::DropDatabase {
                name: "mydb".into(),
                if_exists: true,
                cascade: false,
            }
        );
    }

    #[test]
    fn parse_drop_database_rejects_unknown_trailing_token() {
        let sql = "DROP DATABASE mydb GARBAGE";
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        let err = try_parse(&upper, &parts, sql).unwrap().unwrap_err();
        match err {
            SqlError::Parse { detail } => {
                assert!(detail.contains("GARBAGE"), "unexpected: {detail}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_alter_database_rename() {
        let stmt = ok("ALTER DATABASE mydb RENAME TO newdb");
        assert_eq!(
            stmt,
            NodedbStatement::AlterDatabase {
                name: "mydb".into(),
                operation: AlterDatabaseOperation::Rename {
                    new_name: "newdb".into()
                },
            }
        );
    }

    #[test]
    fn parse_alter_database_set_quota() {
        let stmt = ok("ALTER DATABASE mydb SET QUOTA 42");
        assert_eq!(
            stmt,
            NodedbStatement::AlterDatabase {
                name: "mydb".into(),
                operation: AlterDatabaseOperation::SetQuota { quota_id: 42 },
            }
        );
    }

    #[test]
    fn parse_show_databases() {
        let sql = "SHOW DATABASES";
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        let stmt = try_parse(&upper, &parts, sql).unwrap().unwrap();
        assert_eq!(stmt, NodedbStatement::ShowDatabases);
    }

    #[test]
    fn parse_use_database() {
        let stmt = ok("USE DATABASE mydb");
        assert_eq!(
            stmt,
            NodedbStatement::UseDatabase {
                name: "mydb".into()
            }
        );
    }
}
