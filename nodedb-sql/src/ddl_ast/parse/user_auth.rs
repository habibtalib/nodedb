// SPDX-License-Identifier: Apache-2.0

//! Parse users/roles/permissions/grants + audit/tenants/constraints/typeguards.

use crate::ddl_ast::statement::{
    AlterRoleOp, AlterUserOp, AuthStmt, DatabaseStmt, MiscStmt, NodedbStatement, TenantSelector,
};
use crate::error::SqlError;

pub(super) fn try_parse(
    upper: &str,
    parts: &[&str],
    trimmed: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    try_parse_inner(upper, parts, trimmed)
}

fn try_parse_inner(
    upper: &str,
    parts: &[&str],
    trimmed: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    if upper.starts_with("CREATE USER ") {
        return Some(Ok(parse_create_user(parts, trimmed)));
    }
    if upper.starts_with("DROP USER ") {
        let username = parts.get(2)?.to_string();
        return Some(Ok(NodedbStatement::Auth(AuthStmt::DropUser { username })));
    }
    if upper.starts_with("ALTER USER ") {
        return Some(parse_alter_user(parts, trimmed));
    }
    if upper.starts_with("SHOW USERS") {
        return Some(Ok(NodedbStatement::Auth(AuthStmt::ShowUsers)));
    }
    if upper.starts_with("ALTER ROLE ") {
        return Some(parse_alter_role(parts, trimmed));
    }
    // `GRANT` / `REVOKE` (role-membership and object-permission) are parsed
    // by the dedicated `grant` family, which runs before this one.
    if upper.starts_with("SHOW PERMISSIONS") {
        // SHOW PERMISSIONS [ON <collection>] [FOR <grantee>]
        let on_collection = parts
            .iter()
            .position(|p| p.eq_ignore_ascii_case("ON"))
            .and_then(|i| parts.get(i + 1))
            .map(|s| s.to_string());
        let for_grantee = parts
            .iter()
            .position(|p| p.eq_ignore_ascii_case("FOR"))
            .and_then(|i| parts.get(i + 1))
            .map(|s| s.to_string());
        return Some(Ok(NodedbStatement::Auth(AuthStmt::ShowPermissions {
            on_collection,
            for_grantee,
        })));
    }
    if upper.starts_with("SHOW GRANTS") {
        let username = parts.get(2).map(|s| s.to_string());
        return Some(Ok(NodedbStatement::Auth(AuthStmt::ShowGrants { username })));
    }
    // Only the bare `SHOW TENANTS` form lands here. `SHOW TENANTS WITH
    // NAME <name>` and `SHOW TENANT <ident>` are parsed by `tenant.rs`
    // into typed variants — matching them by prefix here would silently
    // drop the filter / identifier and list every tenant.
    if upper == "SHOW TENANTS" {
        return Some(Ok(NodedbStatement::Database(DatabaseStmt::ShowTenants)));
    }
    if upper.starts_with("SHOW AUDIT") {
        return Some(Ok(NodedbStatement::Misc(MiscStmt::ShowAuditLog)));
    }
    if upper.starts_with("SHOW CONSTRAINTS ") {
        let collection = parts.get(2)?.to_string();
        return Some(Ok(NodedbStatement::Misc(MiscStmt::ShowConstraints {
            collection,
        })));
    }
    if upper.starts_with("SHOW TYPEGUARD") {
        let collection = parts.get(2)?.to_string();
        return Some(Ok(NodedbStatement::Misc(MiscStmt::ShowTypeGuards {
            collection,
        })));
    }
    None
}

/// Parse `CREATE USER [IF NOT EXISTS] <name> WITH PASSWORD '<password>'
/// [ROLE <role>] [TENANT <id>]`.
///
/// Extracts fields as primitive types; the handler converts role strings to
/// the `Role` enum and tenant IDs to `TenantId`.
fn parse_create_user(parts: &[&str], _trimmed: &str) -> NodedbStatement {
    // parts[0] = CREATE, parts[1] = USER, then an optional `IF NOT EXISTS`
    // clause, then parts[name_idx] = <name>.
    let if_not_exists = parts.len() > 5
        && parts[2].eq_ignore_ascii_case("IF")
        && parts[3].eq_ignore_ascii_case("NOT")
        && parts[4].eq_ignore_ascii_case("EXISTS");
    let name_idx = if if_not_exists { 5 } else { 2 };
    let username = parts
        .get(name_idx)
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Find PASSWORD token and extract the quoted string that follows.
    let password = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("PASSWORD"))
        .and_then(|pi| extract_quoted_string_from_parts(parts, pi + 1))
        .unwrap_or_default();

    // ROLE <role> — find after PASSWORD section.
    let role = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("ROLE"))
        .and_then(|ri| {
            // Make sure this ROLE keyword isn't before WITH/PASSWORD
            // (i.e., it appears after the password argument).
            let pw_pos = parts
                .iter()
                .position(|p| p.eq_ignore_ascii_case("PASSWORD"))
                .unwrap_or(0);
            if ri > pw_pos {
                parts.get(ri + 1).map(|s| s.to_lowercase())
            } else {
                None
            }
        });

    // TENANT <id> | TENANT '<name>' — numeric ids resolve directly, names
    // are resolved against the catalog by the handler.
    let tenant = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("TENANT"))
        .and_then(|ti| parts.get(ti + 1))
        .map(|s| parse_tenant_selector(s));

    NodedbStatement::Auth(AuthStmt::CreateUser {
        username,
        password,
        role,
        tenant,
        if_not_exists,
    })
}

/// Parse a tenant reference: a bare integer is an id, anything else
/// (optionally single-quoted) is a name.
fn parse_tenant_selector(token: &str) -> TenantSelector {
    match token.parse::<u64>() {
        Ok(id) => TenantSelector::Id(id),
        Err(_) => TenantSelector::Name(token.trim_matches('\'').to_string()),
    }
}

/// Parse all `ALTER USER <name> ...` forms.
///
/// Supported forms:
/// - `ALTER USER <name> SET PASSWORD '<password>'`
/// - `ALTER USER <name> SET ROLE <role>`
/// - `ALTER USER <name> ROLE <role>` (alias for `SET ROLE`, matches the
///   spelling many clients try by analogy with `CREATE USER ... ROLE ...`)
/// - `ALTER USER <name> MUST CHANGE PASSWORD`
/// - `ALTER USER <name> PASSWORD NEVER EXPIRES`
/// - `ALTER USER <name> PASSWORD EXPIRES '<iso8601>'`
/// - `ALTER USER <name> PASSWORD EXPIRES IN <N> DAYS`
///
/// Unknown sub-commands return a parse error naming the offending token —
/// they are never silently rewritten into a default `AlterUserOp` variant.
fn parse_alter_user(parts: &[&str], _trimmed: &str) -> Result<NodedbStatement, SqlError> {
    // parts[0] = ALTER, parts[1] = USER, parts[2] = <name>, parts[3] = sub-cmd
    let username = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
    if username.is_empty() {
        return Err(SqlError::Parse {
            detail: alter_user_syntax_msg("missing user name"),
        });
    }
    let Some(sub_owned) = parts.get(3).map(|s| s.to_uppercase()) else {
        return Err(SqlError::Parse {
            detail: alter_user_syntax_msg("ALTER USER requires a sub-command"),
        });
    };
    let sub = sub_owned.as_str();

    let op = match sub {
        "SET" => {
            // parts[4] = action
            let action = parts.get(4).map(|s| s.to_uppercase()).unwrap_or_default();
            match action.as_str() {
                "PASSWORD" => {
                    let password = extract_quoted_string_from_parts(parts, 5).unwrap_or_default();
                    AlterUserOp::SetPassword { password }
                }
                "ROLE" => {
                    let role = parts.get(5).map(|s| s.to_string()).unwrap_or_default();
                    AlterUserOp::SetRole { role }
                }
                "DEFAULT" => {
                    // SET DEFAULT DATABASE <name>
                    let db_name = parts.get(6).map(|s| s.to_string()).unwrap_or_default();
                    AlterUserOp::SetDefaultDatabase { db_name }
                }
                "" => {
                    return Err(SqlError::Parse {
                        detail: alter_user_syntax_msg(
                            "ALTER USER ... SET requires PASSWORD | ROLE | DEFAULT DATABASE",
                        ),
                    });
                }
                other => {
                    return Err(SqlError::Parse {
                        detail: alter_user_syntax_msg(&format!(
                            "unknown ALTER USER ... SET action '{other}' \
                             (expected PASSWORD | ROLE | DEFAULT DATABASE)"
                        )),
                    });
                }
            }
        }
        "ROLE" => {
            // PostgreSQL-compatible alias: `ALTER USER <name> ROLE <role>` —
            // accept it the same as `SET ROLE`, since `CREATE USER ... ROLE ...`
            // uses the keyword without `SET` and clients reach for the
            // parallel form. The empty-role case is enforced by the handler.
            let role = parts.get(4).map(|s| s.to_string()).unwrap_or_default();
            if role.is_empty() {
                return Err(SqlError::Parse {
                    detail: alter_user_syntax_msg("ALTER USER ... ROLE requires a role name"),
                });
            }
            AlterUserOp::SetRole { role }
        }
        "WITH" => {
            // `ALTER USER <name> WITH ROLE <role>` — also seen in the wild.
            // Recognise the `WITH ROLE` form; anything else under WITH is
            // rejected with a clear message.
            let next = parts.get(4).map(|s| s.to_uppercase()).unwrap_or_default();
            if next == "ROLE" {
                let role = parts.get(5).map(|s| s.to_string()).unwrap_or_default();
                if role.is_empty() {
                    return Err(SqlError::Parse {
                        detail: alter_user_syntax_msg(
                            "ALTER USER ... WITH ROLE requires a role name",
                        ),
                    });
                }
                AlterUserOp::SetRole { role }
            } else {
                return Err(SqlError::Parse {
                    detail: alter_user_syntax_msg(&format!(
                        "unknown ALTER USER ... WITH clause '{}' (expected WITH ROLE <role>)",
                        parts.get(4).copied().unwrap_or("")
                    )),
                });
            }
        }
        "MUST" => {
            // ALTER USER <name> MUST CHANGE PASSWORD
            AlterUserOp::MustChangePassword
        }
        "PASSWORD" => {
            // parts[4] = NEVER | EXPIRES
            let next_raw = parts.get(4).copied().unwrap_or("");
            let next = next_raw.to_uppercase();
            match next.as_str() {
                "NEVER" => AlterUserOp::PasswordNeverExpires,
                "EXPIRES" => {
                    // parts[5] = '<iso8601>' or IN
                    let part5 = parts.get(5).map(|s| s.to_uppercase()).unwrap_or_default();
                    if part5 == "IN" {
                        // PASSWORD EXPIRES IN <N> DAYS
                        let days: u32 = parts.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
                        AlterUserOp::PasswordExpiresInDays { days }
                    } else {
                        // PASSWORD EXPIRES '<iso8601>'
                        let iso8601 =
                            extract_quoted_string_from_parts(parts, 5).unwrap_or_default();
                        AlterUserOp::PasswordExpiresAt { iso8601 }
                    }
                }
                "" => {
                    return Err(SqlError::Parse {
                        detail: alter_user_syntax_msg(
                            "ALTER USER ... PASSWORD requires NEVER EXPIRES | EXPIRES ...",
                        ),
                    });
                }
                _ => {
                    return Err(SqlError::Parse {
                        detail: alter_user_syntax_msg(&format!(
                            "unknown ALTER USER ... PASSWORD clause '{next_raw}' \
                             (expected NEVER EXPIRES | EXPIRES '<iso8601>' | EXPIRES IN <N> DAYS)"
                        )),
                    });
                }
            }
        }
        other => {
            return Err(SqlError::Parse {
                detail: alter_user_syntax_msg(&format!("unknown ALTER USER sub-command '{other}'")),
            });
        }
    };

    Ok(NodedbStatement::Auth(AuthStmt::AlterUser { username, op }))
}

/// Build the canonical ALTER USER syntax message. The `reason` describes the
/// specific input that failed; the syntax block lists what's accepted.
fn alter_user_syntax_msg(reason: &str) -> String {
    format!(
        "{reason}. ALTER USER syntax: \
         ALTER USER <name> SET PASSWORD '<password>' | \
         ALTER USER <name> SET ROLE <role> | \
         ALTER USER <name> ROLE <role> | \
         ALTER USER <name> MUST CHANGE PASSWORD | \
         ALTER USER <name> PASSWORD NEVER EXPIRES | \
         ALTER USER <name> PASSWORD EXPIRES '<iso8601>' | \
         ALTER USER <name> PASSWORD EXPIRES IN <N> DAYS"
    )
}

/// Parse `ALTER ROLE <name> GRANT/REVOKE/SET ...`.
///
/// Supported forms (object type is `FUNCTION`, `PROCEDURE`, `COLLECTION`,
/// or `TABLE`; a bare name with no keyword is a collection):
/// - `ALTER ROLE <name> GRANT <perm> ON [<object-type>] <target>`
/// - `ALTER ROLE <name> REVOKE <perm> ON [<object-type>] <target>`
/// - `ALTER ROLE <name> SET INHERIT <parent>`
fn parse_alter_role(parts: &[&str], _trimmed: &str) -> Result<NodedbStatement, SqlError> {
    // parts[0] = ALTER, parts[1] = ROLE, parts[2] = <name>, parts[3] = sub-command
    let name = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
    if name.is_empty() {
        return Err(SqlError::Parse {
            detail: alter_role_syntax_msg("missing role name"),
        });
    }
    let Some(sub_cmd) = parts.get(3).map(|s| s.to_uppercase()) else {
        return Err(SqlError::Parse {
            detail: alter_role_syntax_msg("ALTER ROLE requires a sub-command"),
        });
    };

    let sub_op = match sub_cmd.as_str() {
        "GRANT" => {
            // ALTER ROLE <name> GRANT <perm> ON [object-type] <target>
            let permission = parts.get(4).map(|s| s.to_string()).unwrap_or_default();
            // ON is at index 5; the object clause begins at index 6.
            let (target_type, target_name) = super::grant::classify_object_clause(
                parts.get(6).copied().unwrap_or_default(),
                parts.get(7).copied(),
            );
            AlterRoleOp::Grant {
                permission,
                target_type,
                target_name,
            }
        }
        "REVOKE" => {
            // ALTER ROLE <name> REVOKE <perm> ON [object-type] <target>
            let permission = parts.get(4).map(|s| s.to_string()).unwrap_or_default();
            let (target_type, target_name) = super::grant::classify_object_clause(
                parts.get(6).copied().unwrap_or_default(),
                parts.get(7).copied(),
            );
            AlterRoleOp::Revoke {
                permission,
                target_type,
                target_name,
            }
        }
        "SET" => {
            // ALTER ROLE <name> SET INHERIT <parent>
            let next = parts.get(4).map(|s| s.to_uppercase()).unwrap_or_default();
            if next != "INHERIT" {
                return Err(SqlError::Parse {
                    detail: alter_role_syntax_msg(&format!(
                        "unknown ALTER ROLE ... SET clause '{}' (expected SET INHERIT <parent>)",
                        parts.get(4).copied().unwrap_or("")
                    )),
                });
            }
            let parent = parts.get(5).map(|s| s.to_string()).unwrap_or_default();
            if parent.is_empty() {
                return Err(SqlError::Parse {
                    detail: alter_role_syntax_msg(
                        "ALTER ROLE ... SET INHERIT requires a parent role name",
                    ),
                });
            }
            AlterRoleOp::SetInherit { parent }
        }
        other => {
            return Err(SqlError::Parse {
                detail: alter_role_syntax_msg(&format!("unknown ALTER ROLE sub-command '{other}'")),
            });
        }
    };

    Ok(NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }))
}

fn alter_role_syntax_msg(reason: &str) -> String {
    format!(
        "{reason}. ALTER ROLE syntax: \
         ALTER ROLE <name> GRANT <perm> ON [<object-type>] <target> | \
         ALTER ROLE <name> REVOKE <perm> ON [<object-type>] <target> | \
         ALTER ROLE <name> SET INHERIT <parent>"
    )
}

/// Extract a single-quoted string from parts starting at `start`.
/// Handles multi-token quoted strings like `'hello world'`.
fn extract_quoted_string_from_parts(parts: &[&str], start: usize) -> Option<String> {
    if start >= parts.len() {
        return None;
    }
    let first = parts[start];
    if !first.starts_with('\'') {
        return None;
    }
    if first.ends_with('\'') && first.len() > 1 {
        return Some(first[1..first.len() - 1].to_string());
    }
    // Multi-token: accumulate until closing quote.
    let mut result = first[1..].to_string();
    for &part in &parts[start + 1..] {
        result.push(' ');
        if let Some(stripped) = part.strip_suffix('\'') {
            result.push_str(stripped);
            return Some(result);
        }
        result.push_str(part);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(sql: &str) -> Option<NodedbStatement> {
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        try_parse(&upper, &parts, sql).map(|r| r.unwrap())
    }

    #[test]
    fn create_user_basic() {
        let stmt = parse("CREATE USER alice WITH PASSWORD 'secret' ROLE read_write").unwrap();
        if let NodedbStatement::Auth(AuthStmt::CreateUser {
            username,
            password,
            role,
            tenant,
            if_not_exists,
        }) = stmt
        {
            assert_eq!(username, "alice");
            assert_eq!(password, "secret");
            assert_eq!(role.as_deref(), Some("read_write"));
            assert!(tenant.is_none());
            assert!(!if_not_exists);
        } else {
            panic!("expected CreateUser");
        }
    }

    #[test]
    fn create_user_if_not_exists() {
        let stmt = parse("CREATE USER IF NOT EXISTS alice WITH PASSWORD 'secret' ROLE read_write")
            .unwrap();
        if let NodedbStatement::Auth(AuthStmt::CreateUser {
            username,
            if_not_exists,
            ..
        }) = stmt
        {
            // The `IF NOT EXISTS` keywords must not be consumed as the
            // username — `alice` is the real name.
            assert_eq!(username, "alice");
            assert!(if_not_exists);
        } else {
            panic!("expected CreateUser");
        }
    }

    #[test]
    fn create_user_no_role() {
        let stmt = parse("CREATE USER bob WITH PASSWORD 'pw123'").unwrap();
        if let NodedbStatement::Auth(AuthStmt::CreateUser { username, role, .. }) = stmt {
            assert_eq!(username, "bob");
            assert!(role.is_none());
        } else {
            panic!("expected CreateUser");
        }
    }

    #[test]
    fn create_user_with_tenant() {
        let stmt = parse("CREATE USER carol WITH PASSWORD 'pw' TENANT 42").unwrap();
        if let NodedbStatement::Auth(AuthStmt::CreateUser { tenant, .. }) = stmt {
            assert_eq!(tenant, Some(TenantSelector::Id(42)));
        } else {
            panic!("expected CreateUser");
        }
    }

    #[test]
    fn create_user_with_tenant_by_name() {
        let stmt = parse("CREATE USER dave WITH PASSWORD 'pw' TENANT 'acme'").unwrap();
        if let NodedbStatement::Auth(AuthStmt::CreateUser { tenant, .. }) = stmt {
            assert_eq!(tenant, Some(TenantSelector::Name("acme".to_string())));
        } else {
            panic!("expected CreateUser");
        }
    }

    #[test]
    fn alter_user_set_password() {
        let stmt = parse("ALTER USER alice SET PASSWORD 'newpass'").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::AlterUser {
                username: "alice".to_string(),
                op: AlterUserOp::SetPassword {
                    password: "newpass".to_string()
                },
            })
        );
    }

    #[test]
    fn alter_user_set_role() {
        let stmt = parse("ALTER USER alice SET ROLE admin").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::AlterUser {
                username: "alice".to_string(),
                op: AlterUserOp::SetRole {
                    role: "admin".to_string()
                },
            })
        );
    }

    #[test]
    fn alter_user_must_change_password() {
        let stmt = parse("ALTER USER alice MUST CHANGE PASSWORD").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::AlterUser {
                username: "alice".to_string(),
                op: AlterUserOp::MustChangePassword,
            })
        );
    }

    #[test]
    fn alter_user_password_never_expires() {
        let stmt = parse("ALTER USER alice PASSWORD NEVER EXPIRES").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::AlterUser {
                username: "alice".to_string(),
                op: AlterUserOp::PasswordNeverExpires,
            })
        );
    }

    #[test]
    fn alter_user_password_expires_at() {
        let stmt = parse("ALTER USER alice PASSWORD EXPIRES '2026-12-31T00:00:00Z'").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::AlterUser {
                username: "alice".to_string(),
                op: AlterUserOp::PasswordExpiresAt {
                    iso8601: "2026-12-31T00:00:00Z".to_string()
                },
            })
        );
    }

    #[test]
    fn alter_user_password_expires_in_days() {
        let stmt = parse("ALTER USER alice PASSWORD EXPIRES IN 90 DAYS").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::AlterUser {
                username: "alice".to_string(),
                op: AlterUserOp::PasswordExpiresInDays { days: 90 },
            })
        );
    }

    // ── ALTER ROLE tests ─────────────────────────────────────────────

    #[test]
    fn alter_role_set_inherit() {
        let stmt = parse("ALTER ROLE analyst SET INHERIT readonly").unwrap();
        if let NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }) = stmt {
            assert_eq!(name, "analyst");
            assert_eq!(
                sub_op,
                AlterRoleOp::SetInherit {
                    parent: "readonly".to_string()
                }
            );
        } else {
            panic!("expected AlterRole");
        }
    }

    #[test]
    fn alter_role_grant_collection() {
        let stmt = parse("ALTER ROLE analyst GRANT READ ON my_collection").unwrap();
        if let NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }) = stmt {
            assert_eq!(name, "analyst");
            assert_eq!(
                sub_op,
                AlterRoleOp::Grant {
                    permission: "READ".to_string(),
                    target_type: "COLLECTION".to_string(),
                    target_name: "my_collection".to_string(),
                }
            );
        } else {
            panic!("expected AlterRole");
        }
    }

    #[test]
    fn alter_role_grant_on_collection_keyword() {
        let stmt = parse("ALTER ROLE analyst GRANT READ ON COLLECTION my_collection").unwrap();
        if let NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }) = stmt {
            assert_eq!(name, "analyst");
            // The explicit `COLLECTION` object-type keyword must be
            // recognized, not consumed as the collection name itself.
            assert_eq!(
                sub_op,
                AlterRoleOp::Grant {
                    permission: "READ".to_string(),
                    target_type: "COLLECTION".to_string(),
                    target_name: "my_collection".to_string(),
                }
            );
        } else {
            panic!("expected AlterRole");
        }
    }

    #[test]
    fn alter_role_revoke_on_collection_keyword() {
        let stmt = parse("ALTER ROLE analyst REVOKE WRITE ON COLLECTION orders").unwrap();
        if let NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }) = stmt {
            assert_eq!(name, "analyst");
            assert_eq!(
                sub_op,
                AlterRoleOp::Revoke {
                    permission: "WRITE".to_string(),
                    target_type: "COLLECTION".to_string(),
                    target_name: "orders".to_string(),
                }
            );
        } else {
            panic!("expected AlterRole");
        }
    }

    #[test]
    fn alter_role_grant_function() {
        let stmt = parse("ALTER ROLE analyst GRANT EXECUTE ON FUNCTION my_func").unwrap();
        if let NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }) = stmt {
            assert_eq!(name, "analyst");
            assert_eq!(
                sub_op,
                AlterRoleOp::Grant {
                    permission: "EXECUTE".to_string(),
                    target_type: "FUNCTION".to_string(),
                    target_name: "my_func".to_string(),
                }
            );
        } else {
            panic!("expected AlterRole");
        }
    }

    #[test]
    fn alter_role_revoke_collection() {
        let stmt = parse("ALTER ROLE analyst REVOKE WRITE ON orders").unwrap();
        if let NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }) = stmt {
            assert_eq!(name, "analyst");
            assert_eq!(
                sub_op,
                AlterRoleOp::Revoke {
                    permission: "WRITE".to_string(),
                    target_type: "COLLECTION".to_string(),
                    target_name: "orders".to_string(),
                }
            );
        } else {
            panic!("expected AlterRole");
        }
    }

    #[test]
    fn alter_role_revoke_function() {
        let stmt = parse("ALTER ROLE analyst REVOKE EXECUTE ON FUNCTION calc").unwrap();
        if let NodedbStatement::Auth(AuthStmt::AlterRole { name, sub_op }) = stmt {
            assert_eq!(name, "analyst");
            assert_eq!(
                sub_op,
                AlterRoleOp::Revoke {
                    permission: "EXECUTE".to_string(),
                    target_type: "FUNCTION".to_string(),
                    target_name: "calc".to_string(),
                }
            );
        } else {
            panic!("expected AlterRole");
        }
    }

    // ── SHOW PERMISSIONS tests ────────────────────────────────────────

    #[test]
    fn show_permissions_no_filter() {
        let stmt = parse("SHOW PERMISSIONS").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::ShowPermissions {
                on_collection: None,
                for_grantee: None,
            })
        );
    }

    #[test]
    fn show_permissions_on_collection() {
        let stmt = parse("SHOW PERMISSIONS ON orders").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::ShowPermissions {
                on_collection: Some("orders".to_string()),
                for_grantee: None,
            })
        );
    }

    #[test]
    fn show_permissions_for_user() {
        let stmt = parse("SHOW PERMISSIONS FOR alice").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::ShowPermissions {
                on_collection: None,
                for_grantee: Some("alice".to_string()),
            })
        );
    }

    #[test]
    fn show_permissions_on_and_for() {
        let stmt = parse("SHOW PERMISSIONS ON orders FOR alice").unwrap();
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::ShowPermissions {
                on_collection: Some("orders".to_string()),
                for_grantee: Some("alice".to_string()),
            })
        );
    }
}
