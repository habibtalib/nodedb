// SPDX-License-Identifier: Apache-2.0

//! `CLONE DATABASE <new> FROM <source> [AS OF SYSTEM TIME <ms> | LATEST]`.

use crate::ddl_ast::statement::{CloneAsOf, NodedbStatement};
use crate::error::SqlError;

pub(super) fn parse_clone_database(
    parts: &[&str],
    upper: &str,
) -> Result<NodedbStatement, SqlError> {
    let new_name = parts
        .get(2)
        .copied()
        .ok_or_else(|| SqlError::Parse {
            detail: "CLONE DATABASE requires a target name".into(),
        })?
        .trim_matches('"')
        .to_string();
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

    // Determine the AS OF clause.
    // Accepted forms:
    //   (no AS OF)                            → Latest
    //   AS OF SYSTEM TIME LATEST              → Latest
    //   AS OF SYSTEM TIME <ms>                → SystemTimeMs(<ms>)
    let as_of = if upper.contains("AS OF SYSTEM TIME") {
        let ms_idx = parts
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
        match ms_idx {
            Some(idx) => match parts.get(idx) {
                Some(tok) if tok.to_uppercase() == "LATEST" => CloneAsOf::Latest,
                Some(ms_str) => {
                    let ms = ms_str.parse::<i64>().map_err(|_| SqlError::Parse {
                        detail: format!(
                            "CLONE DATABASE AS OF SYSTEM TIME: expected integer milliseconds \
                             or LATEST, got '{ms_str}'"
                        ),
                    })?;
                    CloneAsOf::SystemTimeMs(ms)
                }
                None => {
                    return Err(SqlError::Parse {
                        detail: "CLONE DATABASE AS OF SYSTEM TIME requires a timestamp or LATEST"
                            .into(),
                    });
                }
            },
            None => CloneAsOf::Latest,
        }
    } else {
        CloneAsOf::Latest
    };

    Ok(NodedbStatement::CloneDatabase {
        new_name,
        source_name,
        as_of,
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
    fn parse_clone_database_as_of_system_time() {
        let stmt = ok("CLONE DATABASE newdb FROM srcdb AS OF SYSTEM TIME 1730000000000");
        assert_eq!(
            stmt,
            NodedbStatement::CloneDatabase {
                new_name: "newdb".into(),
                source_name: "srcdb".into(),
                as_of: CloneAsOf::SystemTimeMs(1_730_000_000_000),
            }
        );
    }

    #[test]
    fn parse_clone_database_latest() {
        let stmt = ok("CLONE DATABASE newdb FROM srcdb AS OF LATEST");
        assert_eq!(
            stmt,
            NodedbStatement::CloneDatabase {
                new_name: "newdb".into(),
                source_name: "srcdb".into(),
                as_of: CloneAsOf::Latest,
            }
        );
    }

    #[test]
    fn parse_clone_database_no_as_of_defaults_to_latest() {
        let stmt = ok("CLONE DATABASE newdb FROM srcdb");
        assert_eq!(
            stmt,
            NodedbStatement::CloneDatabase {
                new_name: "newdb".into(),
                source_name: "srcdb".into(),
                as_of: CloneAsOf::Latest,
            }
        );
    }
}
