// SPDX-License-Identifier: Apache-2.0

//! `MIRROR DATABASE <local_name> FROM <source_cluster>.<source_database> [MODE = sync | async]`
//! and `SHOW DATABASE MIRROR STATUS [FOR <name>]`.

use nodedb_types::MirrorMode;

use crate::ddl_ast::statement::NodedbStatement;
use crate::error::SqlError;

pub(super) fn parse_mirror_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // The source is specified as `<cluster_id>.<database_name>`, allowing the
    // handler to set up a cross-cluster QUIC link to the correct source cluster.
    let local_name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "MIRROR DATABASE requires a local name".into(),
        })?
        .trim_matches('"')
        .to_string();
    let from_idx = parts
        .iter()
        .position(|w| w.to_uppercase() == "FROM")
        .ok_or_else(|| SqlError::Parse {
            detail: "MIRROR DATABASE requires FROM <source_cluster>.<source_database>".into(),
        })?;
    let source_token = parts
        .get(from_idx + 1)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "MIRROR DATABASE FROM requires <source_cluster>.<source_database>".into(),
        })?
        .trim_matches('"');

    // Split on the first '.' to extract cluster.database. If there is no dot
    // the whole token is treated as the source_cluster and source_database
    // defaults to the same identifier (same-name convention for local testing).
    let (source_cluster, source_database) = match source_token.find('.') {
        Some(dot_pos) => {
            let cluster = source_token[..dot_pos].trim_matches('"').to_string();
            let database = source_token[dot_pos + 1..].trim_matches('"').to_string();
            if cluster.is_empty() || database.is_empty() {
                return Err(SqlError::Parse {
                    detail: format!(
                        "MIRROR DATABASE FROM: invalid source '{source_token}'; \
                         expected <source_cluster>.<source_database>"
                    ),
                });
            }
            (cluster, database)
        }
        None => {
            let name = source_token.to_string();
            (name.clone(), name)
        }
    };

    // MODE = sync | async (optional; default async)
    let mode = parts
        .windows(3)
        .find(|w| w[0].to_uppercase() == "MODE" && w[1] == "=")
        .map(|w| match w[2].to_uppercase().as_str() {
            "SYNC" => Ok(MirrorMode::Sync),
            "ASYNC" => Ok(MirrorMode::Async),
            other => Err(SqlError::Parse {
                detail: format!("MIRROR DATABASE MODE: expected 'sync' or 'async', got '{other}'"),
            }),
        })
        .transpose()?
        .unwrap_or(MirrorMode::Async);

    Ok(NodedbStatement::MirrorDatabase {
        local_name,
        source_cluster,
        source_database,
        mode,
    })
}

pub(super) fn parse_show_database_mirror_status(
    parts: &[&str],
) -> Result<NodedbStatement, SqlError> {
    // SHOW DATABASE MIRROR STATUS [FOR <name>]
    // parts: [SHOW, DATABASE, MIRROR, STATUS, ...]
    let name = if parts.get(4).map(|w| w.to_uppercase()).as_deref() == Some("FOR") {
        let n = parts
            .get(5)
            .copied()
            .ok_or_else(|| SqlError::Parse {
                detail: "SHOW DATABASE MIRROR STATUS FOR requires a database name".into(),
            })?
            .trim_matches('"')
            .to_string();
        Some(n)
    } else {
        None
    };
    Ok(NodedbStatement::ShowDatabaseMirrorStatus { name })
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
    fn parse_mirror_database_dotted_source_async_default() {
        let stmt = ok("MIRROR DATABASE replica FROM prod-us.mydb");
        match stmt {
            NodedbStatement::MirrorDatabase {
                local_name,
                source_cluster,
                source_database,
                mode,
            } => {
                assert_eq!(local_name, "replica");
                assert_eq!(source_cluster, "prod-us");
                assert_eq!(source_database, "mydb");
                assert_eq!(mode, MirrorMode::Async);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_mirror_database_sync_mode() {
        let stmt = ok("MIRROR DATABASE replica FROM prod-us.mydb MODE = sync");
        match stmt {
            NodedbStatement::MirrorDatabase { mode, .. } => {
                assert_eq!(mode, MirrorMode::Sync);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_mirror_database_invalid_mode_errors() {
        let sql = "MIRROR DATABASE replica FROM prod-us.mydb MODE = invalid";
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        let err = try_parse(&upper, &parts, sql).unwrap().unwrap_err();
        match err {
            SqlError::Parse { detail } => {
                assert!(
                    detail.contains("async") || detail.contains("sync"),
                    "{detail}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_show_database_mirror_status_all() {
        let stmt = ok("SHOW DATABASE MIRROR STATUS");
        assert_eq!(
            stmt,
            NodedbStatement::ShowDatabaseMirrorStatus { name: None }
        );
    }

    #[test]
    fn parse_show_database_mirror_status_for_name() {
        let stmt = ok("SHOW DATABASE MIRROR STATUS FOR replica");
        assert_eq!(
            stmt,
            NodedbStatement::ShowDatabaseMirrorStatus {
                name: Some("replica".into())
            }
        );
    }

    #[test]
    fn parse_show_database_mirror_status_missing_name_errors() {
        let sql = "SHOW DATABASE MIRROR STATUS FOR";
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
