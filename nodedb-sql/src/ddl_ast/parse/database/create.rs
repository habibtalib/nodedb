// SPDX-License-Identifier: Apache-2.0

//! `CREATE DATABASE [IF NOT EXISTS] <name> [WITH (...)]`.

use crate::ddl_ast::statement::{DatabaseStmt, NodedbStatement};
use crate::error::SqlError;

use super::with_options::parse_with_options;

pub(super) fn parse_create_database(
    parts: &[&str],
    original: &str,
) -> Result<NodedbStatement, SqlError> {
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

    let options = parse_with_options(original);

    Ok(NodedbStatement::Database(DatabaseStmt::CreateDatabase {
        name,
        if_not_exists,
        options,
    }))
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
    fn parse_create_database_simple() {
        let stmt = ok("CREATE DATABASE mydb");
        assert_eq!(
            stmt,
            NodedbStatement::Database(DatabaseStmt::CreateDatabase {
                name: "mydb".into(),
                if_not_exists: false,
                options: vec![],
            })
        );
    }

    #[test]
    fn parse_create_database_if_not_exists() {
        let stmt = ok("CREATE DATABASE IF NOT EXISTS mydb");
        match stmt {
            NodedbStatement::Database(DatabaseStmt::CreateDatabase {
                name,
                if_not_exists,
                ..
            }) => {
                assert_eq!(name, "mydb");
                assert!(if_not_exists);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
