// SPDX-License-Identifier: Apache-2.0

//! Parse `SHOW GRAPH STATS ['<collection>'] [VERBOSE] [AS OF SYSTEM TIME <ms>]`.
//!
//! Read-only persistence-rooted stats readout. The collection literal is
//! single-quoted to disambiguate from future bare-keyword extensions. The
//! collection is optional; absence selects a tenant-wide aggregate over
//! all graph collections owned by the session tenant.

use crate::ddl_ast::statement::{GraphStmt, NodedbStatement};
use crate::error::SqlError;

pub(super) fn try_parse(
    upper: &str,
    _parts: &[&str],
    trimmed: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    if !upper.starts_with("SHOW GRAPH STATS") {
        return None;
    }

    // Consume the leading keyword span exactly so any trailing chars are
    // available for arg parsing (quoted collection, VERBOSE, AS OF ...).
    let after = trimmed[16..].trim_start();
    let after_upper = after.to_ascii_uppercase();

    // Collection (optional single-quoted literal).
    let (collection, rest, rest_upper) = if let Some(stripped) = after.strip_prefix('\'') {
        match stripped.find('\'') {
            Some(end) => {
                let name = &stripped[..end];
                let tail = stripped[end + 1..].trim_start();
                let tail_upper = tail.to_ascii_uppercase();
                (Some(name.to_string()), tail, tail_upper)
            }
            None => {
                return Some(Err(SqlError::Parse {
                    detail: "SHOW GRAPH STATS: unterminated quoted collection name".to_string(),
                }));
            }
        }
    } else {
        (None, after, after_upper)
    };

    // VERBOSE flag (optional; order-insensitive relative to AS OF).
    let mut verbose = false;
    let mut working = rest;
    let mut working_upper = rest_upper;
    if working_upper.starts_with("VERBOSE") {
        verbose = true;
        working = working[7..].trim_start();
        working_upper = working.to_ascii_uppercase();
    }

    // AS OF SYSTEM TIME <ms> (optional). Ordering: if VERBOSE wasn't first,
    // check it again here so either order is accepted.
    let mut as_of: Option<i64> = None;
    if working_upper.starts_with("AS OF SYSTEM TIME") {
        let after_kw = working[17..].trim_start();
        let ms_token: String = after_kw
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        if ms_token.is_empty() {
            return Some(Err(SqlError::Parse {
                detail:
                    "SHOW GRAPH STATS: AS OF SYSTEM TIME expects an integer millisecond literal"
                        .to_string(),
            }));
        }
        match ms_token.parse::<i64>() {
            Ok(v) => as_of = Some(v),
            Err(_) => {
                return Some(Err(SqlError::Parse {
                    detail: format!(
                        "SHOW GRAPH STATS: invalid AS OF SYSTEM TIME value '{ms_token}'"
                    ),
                }));
            }
        }
        working = after_kw[ms_token.len()..].trim_start();
        working_upper = working.to_ascii_uppercase();
    }

    // VERBOSE may appear after AS OF too.
    if !verbose && working_upper.starts_with("VERBOSE") {
        verbose = true;
        working = working[7..].trim_start();
    }

    if !working.is_empty() {
        return Some(Err(SqlError::Parse {
            detail: format!("SHOW GRAPH STATS: unexpected trailing input '{working}'"),
        }));
    }

    Some(Ok(NodedbStatement::Graph(GraphStmt::ShowGraphStats {
        collection,
        verbose,
        as_of,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(sql: &str) -> Option<Result<NodedbStatement, SqlError>> {
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        try_parse(&upper, &parts, sql)
    }

    fn expect_stats(sql: &str) -> (Option<String>, bool, Option<i64>) {
        match parse(sql).expect("recognised prefix").expect("parses") {
            NodedbStatement::Graph(GraphStmt::ShowGraphStats {
                collection,
                verbose,
                as_of,
            }) => (collection, verbose, as_of),
            other => panic!("expected ShowGraphStats, got {other:?}"),
        }
    }

    #[test]
    fn bare_form_means_tenant_aggregate() {
        let (col, verbose, as_of) = expect_stats("SHOW GRAPH STATS");
        assert_eq!(col, None);
        assert!(!verbose);
        assert_eq!(as_of, None);
    }

    #[test]
    fn collection_form() {
        let (col, verbose, as_of) = expect_stats("SHOW GRAPH STATS 'edges'");
        assert_eq!(col.as_deref(), Some("edges"));
        assert!(!verbose);
        assert_eq!(as_of, None);
    }

    #[test]
    fn verbose_flag() {
        let (col, verbose, as_of) = expect_stats("SHOW GRAPH STATS 'edges' VERBOSE");
        assert_eq!(col.as_deref(), Some("edges"));
        assert!(verbose);
        assert_eq!(as_of, None);
    }

    #[test]
    fn as_of_system_time() {
        let (col, verbose, as_of) =
            expect_stats("SHOW GRAPH STATS 'edges' AS OF SYSTEM TIME 1700000000000");
        assert_eq!(col.as_deref(), Some("edges"));
        assert!(!verbose);
        assert_eq!(as_of, Some(1_700_000_000_000));
    }

    #[test]
    fn verbose_then_as_of() {
        let (_, verbose, as_of) =
            expect_stats("SHOW GRAPH STATS 'e' VERBOSE AS OF SYSTEM TIME 100");
        assert!(verbose);
        assert_eq!(as_of, Some(100));
    }

    #[test]
    fn as_of_then_verbose() {
        let (_, verbose, as_of) =
            expect_stats("SHOW GRAPH STATS 'e' AS OF SYSTEM TIME 100 VERBOSE");
        assert!(verbose);
        assert_eq!(as_of, Some(100));
    }

    #[test]
    fn tenant_aggregate_with_verbose() {
        let (col, verbose, _) = expect_stats("SHOW GRAPH STATS VERBOSE");
        assert_eq!(col, None);
        assert!(verbose);
    }

    #[test]
    fn unterminated_quote_errors() {
        let r = parse("SHOW GRAPH STATS 'unterminated").expect("matched prefix");
        assert!(r.is_err());
    }

    #[test]
    fn unexpected_trailing_errors() {
        let r = parse("SHOW GRAPH STATS 'e' GARBAGE").expect("matched prefix");
        assert!(r.is_err());
    }

    #[test]
    fn non_matching_returns_none() {
        assert!(parse("SHOW COLLECTIONS").is_none());
        assert!(parse("SHOW GRAPH").is_none());
    }
}
