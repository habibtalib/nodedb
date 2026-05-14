// SPDX-License-Identifier: Apache-2.0

//! Parse `ALTER COLLECTION <name> SET ON CONFLICT <policy> FOR <kind>`
//! and `SHOW CONFLICT POLICY ON <name>`.

use crate::ddl_ast::alter_ops::{ConflictPolicyKind, ConstraintKindKeyword};
use crate::ddl_ast::statement::{AlterCollectionOp, CollectionStmt, NodedbStatement, PolicyStmt};
use crate::error::SqlError;

/// Try to parse conflict-policy DDL statements.
///
/// Handles:
/// - `ALTER COLLECTION <name> SET ON CONFLICT <policy> FOR <kind>`
/// - `SHOW CONFLICT POLICY ON <name>`
pub(super) fn try_parse(
    upper: &str,
    parts: &[&str],
    _trimmed: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    if upper.starts_with("SHOW CONFLICT POLICY ON ") {
        return Some(parse_show_conflict_policy(parts));
    }
    // ALTER COLLECTION <name> SET ON CONFLICT is handled inside the collection
    // parser's alter_ops path, but we also intercept it here so the prefix
    // "ALTER COLLECTION ... SET ON CONFLICT" routes before the generic
    // collection handler inspects "SET ON".
    if upper.starts_with("ALTER COLLECTION ") && upper.contains(" SET ON CONFLICT ") {
        return Some(parse_alter_set_on_conflict(parts));
    }
    None
}

fn parse_show_conflict_policy(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // SHOW CONFLICT POLICY ON <name>
    // parts: [SHOW, CONFLICT, POLICY, ON, <name>]
    if parts.len() < 5 {
        return Err(SqlError::Parse {
            detail: "syntax: SHOW CONFLICT POLICY ON <collection>".to_string(),
        });
    }
    let on_idx = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("ON"))
        .ok_or_else(|| SqlError::Parse {
            detail: "SHOW CONFLICT POLICY: missing ON keyword".to_string(),
        })?;
    let name = parts.get(on_idx + 1).ok_or_else(|| SqlError::Parse {
        detail: "SHOW CONFLICT POLICY ON: missing collection name".to_string(),
    })?;
    Ok(NodedbStatement::Policy(PolicyStmt::ShowConflictPolicy {
        collection: name.to_lowercase(),
    }))
}

fn parse_alter_set_on_conflict(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // ALTER COLLECTION <name> SET ON CONFLICT <policy> FOR <kind>
    // parts: [ALTER, COLLECTION, <name>, SET, ON, CONFLICT, <policy>, FOR, <kind>]
    let name = parts.get(2).ok_or_else(|| SqlError::Parse {
        detail: "ALTER COLLECTION SET ON CONFLICT: missing collection name".to_string(),
    })?;

    // Find CONFLICT keyword position, then policy is the token after it.
    let conflict_idx = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("CONFLICT"))
        .ok_or_else(|| SqlError::Parse {
            detail: "ALTER COLLECTION SET ON CONFLICT: missing CONFLICT keyword".to_string(),
        })?;

    let policy_str = parts.get(conflict_idx + 1).ok_or_else(|| SqlError::Parse {
        detail: "ALTER COLLECTION SET ON CONFLICT: missing policy keyword".to_string(),
    })?;

    let for_idx = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("FOR"))
        .ok_or_else(|| SqlError::Parse {
            detail: "ALTER COLLECTION SET ON CONFLICT <policy>: missing FOR keyword".to_string(),
        })?;

    let kind_str = parts.get(for_idx + 1).ok_or_else(|| SqlError::Parse {
        detail: "ALTER COLLECTION SET ON CONFLICT <policy> FOR: missing constraint kind"
            .to_string(),
    })?;

    let policy = parse_policy_keyword(policy_str)?;
    let constraint_kind = parse_constraint_kind(kind_str)?;

    Ok(NodedbStatement::Collection(
        CollectionStmt::AlterCollection {
            name: name.to_lowercase(),
            operation: AlterCollectionOp::SetOnConflict {
                policy,
                constraint_kind,
            },
        },
    ))
}

fn parse_policy_keyword(s: &str) -> Result<ConflictPolicyKind, SqlError> {
    match s.to_uppercase().as_str() {
        "LAST_WRITER_WINS" => Ok(ConflictPolicyKind::LastWriterWins),
        "RENAME_SUFFIX" | "RENAME_APPEND_SUFFIX" => Ok(ConflictPolicyKind::RenameSuffix),
        "CASCADE_DEFER" => Ok(ConflictPolicyKind::CascadeDefer),
        "ESCALATE_TO_DLQ" => Ok(ConflictPolicyKind::EscalateToDlq),
        "CUSTOM" => Err(SqlError::Parse {
            detail: "CUSTOM conflict policy is not supported via SQL DDL; \
                     use the native NodeDB protocol (TextFields.policy) to configure webhook-based policies"
                .to_string(),
        }),
        other => Err(SqlError::Parse {
            detail: format!(
                "unknown conflict policy keyword '{other}'; \
                 valid options: LAST_WRITER_WINS, RENAME_SUFFIX, CASCADE_DEFER, ESCALATE_TO_DLQ"
            ),
        }),
    }
}

fn parse_constraint_kind(s: &str) -> Result<ConstraintKindKeyword, SqlError> {
    match s.to_uppercase().as_str() {
        "UNIQUE" => Ok(ConstraintKindKeyword::Unique),
        "FOREIGN_KEY" => Ok(ConstraintKindKeyword::ForeignKey),
        "NOT_NULL" => Ok(ConstraintKindKeyword::NotNull),
        "CHECK" => Ok(ConstraintKindKeyword::Check),
        other => Err(SqlError::Parse {
            detail: format!(
                "unknown constraint kind '{other}'; \
                 valid options: UNIQUE, FOREIGN_KEY, NOT_NULL, CHECK"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl_ast::parse::dispatch::parse;

    fn ok(sql: &str) -> NodedbStatement {
        parse(sql).expect("expected Some").expect("expected Ok")
    }

    fn err(sql: &str) -> SqlError {
        parse(sql)
            .expect("expected Some")
            .expect_err("expected Err")
    }

    #[test]
    fn alter_last_writer_wins_unique() {
        let stmt = ok("ALTER COLLECTION agents SET ON CONFLICT LAST_WRITER_WINS FOR UNIQUE");
        match stmt {
            NodedbStatement::Collection(CollectionStmt::AlterCollection { name, operation }) => {
                assert_eq!(name, "agents");
                assert_eq!(
                    operation,
                    AlterCollectionOp::SetOnConflict {
                        policy: ConflictPolicyKind::LastWriterWins,
                        constraint_kind: ConstraintKindKeyword::Unique,
                    }
                );
            }
            other => panic!("expected AlterCollection, got {other:?}"),
        }
    }

    #[test]
    fn alter_cascade_defer_foreign_key() {
        let stmt = ok("ALTER COLLECTION posts SET ON CONFLICT CASCADE_DEFER FOR FOREIGN_KEY");
        match stmt {
            NodedbStatement::Collection(CollectionStmt::AlterCollection { name, operation }) => {
                assert_eq!(name, "posts");
                assert_eq!(
                    operation,
                    AlterCollectionOp::SetOnConflict {
                        policy: ConflictPolicyKind::CascadeDefer,
                        constraint_kind: ConstraintKindKeyword::ForeignKey,
                    }
                );
            }
            other => panic!("expected AlterCollection, got {other:?}"),
        }
    }

    #[test]
    fn alter_rename_suffix_legacy_keyword() {
        let stmt = ok("ALTER COLLECTION x SET ON CONFLICT RENAME_APPEND_SUFFIX FOR NOT_NULL");
        match stmt {
            NodedbStatement::Collection(CollectionStmt::AlterCollection { operation, .. }) => {
                assert_eq!(
                    operation,
                    AlterCollectionOp::SetOnConflict {
                        policy: ConflictPolicyKind::RenameSuffix,
                        constraint_kind: ConstraintKindKeyword::NotNull,
                    }
                );
            }
            other => panic!("expected AlterCollection, got {other:?}"),
        }
    }

    #[test]
    fn alter_escalate_to_dlq_check() {
        let stmt = ok("ALTER COLLECTION x SET ON CONFLICT ESCALATE_TO_DLQ FOR CHECK");
        match stmt {
            NodedbStatement::Collection(CollectionStmt::AlterCollection { operation, .. }) => {
                assert_eq!(
                    operation,
                    AlterCollectionOp::SetOnConflict {
                        policy: ConflictPolicyKind::EscalateToDlq,
                        constraint_kind: ConstraintKindKeyword::Check,
                    }
                );
            }
            other => panic!("expected AlterCollection, got {other:?}"),
        }
    }

    #[test]
    fn show_conflict_policy() {
        let stmt = ok("SHOW CONFLICT POLICY ON mydb");
        assert_eq!(
            stmt,
            NodedbStatement::Policy(PolicyStmt::ShowConflictPolicy {
                collection: "mydb".to_string()
            })
        );
    }

    #[test]
    fn custom_policy_rejected() {
        let e = err("ALTER COLLECTION x SET ON CONFLICT CUSTOM FOR UNIQUE");
        assert!(matches!(e, SqlError::Parse { .. }));
        assert!(e.to_string().contains("native NodeDB protocol"));
    }

    #[test]
    fn unknown_policy_rejected() {
        let e = err("ALTER COLLECTION x SET ON CONFLICT FOOBAR FOR UNIQUE");
        assert!(matches!(e, SqlError::Parse { .. }));
    }

    #[test]
    fn unknown_constraint_kind_rejected() {
        let e = err("ALTER COLLECTION x SET ON CONFLICT LAST_WRITER_WINS FOR BADKIND");
        assert!(matches!(e, SqlError::Parse { .. }));
    }
}
