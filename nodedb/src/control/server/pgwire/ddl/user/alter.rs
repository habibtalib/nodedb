// SPDX-License-Identifier: BUSL-1.1

//! `ALTER USER` DDL handler — typed dispatch for every `AlterUserOp`.

use nodedb_sql::ddl_ast::AlterUserOp;
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::{AuthenticatedIdentity, Role};
use crate::control::server::pgwire::types::{parse_role, require_tenant_admin, sqlstate_error};
use crate::control::state::SharedState;

use super::iso8601::parse_iso8601_to_unix;

/// ALTER USER <name> ... — typed dispatch for all AlterUserOp forms.
pub fn alter_user(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    username: &str,
    op: &AlterUserOp,
) -> PgWireResult<Vec<Response>> {
    if username.is_empty() {
        return Err(sqlstate_error(
            "42601",
            "syntax: ALTER USER <name> SET PASSWORD '<password>' | SET ROLE <role> | MUST CHANGE PASSWORD | PASSWORD NEVER EXPIRES | PASSWORD EXPIRES ...",
        ));
    }

    // Users can change their own password; admin required for anything else.
    let is_self = username == identity.username;
    let can_alter = is_self || identity.is_superuser || identity.has_role(&Role::TenantAdmin);

    match op {
        AlterUserOp::SetPassword { password } => {
            if !can_alter {
                return Err(sqlstate_error(
                    "42501",
                    "permission denied: can only alter your own user, or be superuser/tenant_admin",
                ));
            }
            if password.is_empty() {
                return Err(sqlstate_error(
                    "42601",
                    "password must be a non-empty single-quoted string",
                ));
            }
            let stored = state
                .credentials
                .prepare_user_update(username, Some(password.as_str()), None)
                .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
            // Password change — no role/access change; no invalidation.
            propose_and_install(state, stored, None)?;

            state.audit_record(
                AuditEvent::PrivilegeChange,
                Some(identity.tenant_id),
                &identity.username,
                &format!("changed password for user '{username}'"),
            );
            Ok(vec![Response::Execution(Tag::new("ALTER USER"))])
        }

        AlterUserOp::SetRole { role } => {
            if is_self && !identity.is_superuser {
                return Err(sqlstate_error("42501", "cannot change your own role"));
            }
            require_tenant_admin(identity, "change roles")?;
            if role.is_empty() {
                return Err(sqlstate_error("42601", "expected role name after SET ROLE"));
            }
            let parsed_role: Role = parse_role(role);
            let stored = state
                .credentials
                .prepare_user_update(username, None, Some(vec![parsed_role.clone()]))
                .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
            propose_and_install(
                state,
                stored,
                Some(crate::control::security::buses::SessionInvalidationReason::RoleAltered),
            )?;

            state.audit_record(
                AuditEvent::PrivilegeChange,
                Some(identity.tenant_id),
                &identity.username,
                &format!("set role '{parsed_role}' for user '{username}'"),
            );
            Ok(vec![Response::Execution(Tag::new("ALTER USER"))])
        }

        AlterUserOp::MustChangePassword => {
            require_tenant_admin(identity, "set must_change_password")?;
            let stored = state
                .credentials
                .prepare_set_must_change_password(username, true)
                .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
            propose_and_install(state, stored, None)?;

            state.audit_record(
                AuditEvent::PrivilegeChange,
                Some(identity.tenant_id),
                &identity.username,
                &format!("set must_change_password for user '{username}'"),
            );
            Ok(vec![Response::Execution(Tag::new("ALTER USER"))])
        }

        AlterUserOp::PasswordNeverExpires => {
            require_tenant_admin(identity, "set password expiry")?;
            let stored = state
                .credentials
                .prepare_set_password_expires_at(username, 0)
                .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
            propose_and_install(state, stored, None)?;

            state.audit_record(
                AuditEvent::PrivilegeChange,
                Some(identity.tenant_id),
                &identity.username,
                &format!("set PASSWORD NEVER EXPIRES for user '{username}'"),
            );
            Ok(vec![Response::Execution(Tag::new("ALTER USER"))])
        }

        AlterUserOp::PasswordExpiresAt { iso8601 } => {
            require_tenant_admin(identity, "set password expiry")?;
            let expires_at = parse_iso8601_to_unix(iso8601).map_err(|e| {
                sqlstate_error(
                    "22007",
                    &format!("invalid ISO-8601 datetime '{iso8601}': {e}"),
                )
            })?;
            let stored = state
                .credentials
                .prepare_set_password_expires_at(username, expires_at)
                .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
            propose_and_install(state, stored, None)?;

            state.audit_record(
                AuditEvent::PrivilegeChange,
                Some(identity.tenant_id),
                &identity.username,
                &format!("set PASSWORD EXPIRES '{iso8601}' for user '{username}'"),
            );
            Ok(vec![Response::Execution(Tag::new("ALTER USER"))])
        }

        AlterUserOp::PasswordExpiresInDays { days } => {
            require_tenant_admin(identity, "set password expiry")?;
            if *days == 0 {
                return Err(sqlstate_error(
                    "22003",
                    "PASSWORD EXPIRES IN requires a positive day count",
                ));
            }
            let expires_at = crate::control::security::time::now_secs() + (*days as u64) * 86400;
            let stored = state
                .credentials
                .prepare_set_password_expires_at(username, expires_at)
                .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
            propose_and_install(state, stored, None)?;

            state.audit_record(
                AuditEvent::PrivilegeChange,
                Some(identity.tenant_id),
                &identity.username,
                &format!("set PASSWORD EXPIRES IN {days} DAYS for user '{username}'"),
            );
            Ok(vec![Response::Execution(Tag::new("ALTER USER"))])
        }

        AlterUserOp::SetDefaultDatabase { db_name } => {
            // Users can set their own default database; admin may set for others.
            if !can_alter {
                return Err(sqlstate_error(
                    "42501",
                    "permission denied: can only alter your own user, or be superuser/tenant_admin",
                ));
            }
            if db_name.is_empty() {
                return Err(sqlstate_error(
                    "42601",
                    "syntax: ALTER USER <name> SET DEFAULT DATABASE <db_name>",
                ));
            }
            // Resolve the database name to an ID via the system catalog.
            let catalog = state
                .credentials
                .catalog()
                .as_ref()
                .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;
            let db_id = catalog
                .get_database_id_by_name(db_name)
                .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup: {e}")))?
                .ok_or_else(|| {
                    sqlstate_error("42704", &format!("database '{db_name}' does not exist"))
                })?;
            let stored = state
                .credentials
                .prepare_set_default_database(username, db_id.as_u64())
                .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
            propose_and_install(state, stored, None)?;

            state.audit_record(
                AuditEvent::PrivilegeChange,
                Some(identity.tenant_id),
                &identity.username,
                &format!("set default database '{db_name}' for user '{username}'"),
            );
            Ok(vec![Response::Execution(Tag::new("ALTER USER"))])
        }
    }
}

/// Propose a `StoredUser` via Raft and install it locally on single-node.
///
/// `invalidation` is passed to `install_replicated_user` for in-process
/// session notification in single-node mode.  Cluster-mode notifications
/// arrive via `post_apply::user::put` after Raft commit.
fn propose_and_install(
    state: &SharedState,
    stored: crate::control::security::catalog::StoredUser,
    invalidation: Option<crate::control::security::buses::SessionInvalidationReason>,
) -> PgWireResult<()> {
    let entry = crate::control::catalog_entry::CatalogEntry::PutUser(Box::new(stored.clone()));
    let log_index = crate::control::metadata_proposer::propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    if log_index == 0 {
        if let Some(catalog) = state.credentials.catalog() {
            catalog
                .put_user(&stored)
                .map_err(|e| sqlstate_error("XX000", &format!("catalog write: {e}")))?;
        }
        state
            .credentials
            .install_replicated_user(&stored, invalidation);
    }
    Ok(())
}
