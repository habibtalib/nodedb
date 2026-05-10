// SPDX-License-Identifier: Apache-2.0

//! `USE DATABASE <name>`.

use crate::ddl_ast::statement::NodedbStatement;
use crate::error::SqlError;

pub(super) fn parse_use_database(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
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
