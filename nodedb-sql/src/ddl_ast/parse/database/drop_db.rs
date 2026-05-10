// SPDX-License-Identifier: Apache-2.0

//! `DROP DATABASE [IF EXISTS] <name> [CASCADE | FORCE]`.

use crate::ddl_ast::statement::NodedbStatement;
use crate::error::SqlError;

pub(super) fn parse_drop_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
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
}
