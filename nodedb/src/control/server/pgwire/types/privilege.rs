// SPDX-License-Identifier: BUSL-1.1

//! Privilege-gate helpers for pgwire DDL handlers.
//!
//! Five gates correspond to the privilege hierarchy. Each gate emits
//! `AuditEvent::PermissionDenied` on failure and returns SQLSTATE 42501
//! (`INSUFFICIENT_PRIVILEGE`):
//!
//! | Helper                              | Allowed roles                                          |
//! |-------------------------------------|--------------------------------------------------------|
//! | `require_superuser`                 | Superuser                                              |
//! | `require_cluster_admin`             | Superuser, ClusterAdmin                                |
//! | `require_tenant_admin`              | Superuser, TenantAdmin                                 |
//! | `require_database_owner`            | Superuser, DatabaseOwner(db)                           |
//! | `require_database_owner_or_higher`  | Superuser, ClusterAdmin, DatabaseOwner(db)             |
//!
//! `require_tenant_admin` is the legacy gate covering tenant- and system-scoped
//! DDL (synonym groups, procedures, change streams, triggers, consumer groups,
//! retention policies, alerts, topics, roles, service accounts, functions,
//! streaming MVs, schedules, grants, users, custom types, apikeys, and tenant
//! quota DDL). It is intentionally NOT migrated to the database-scoped gates;
//! those targets are scoped above the database axis.

use nodedb_types::error::sqlstate;
use nodedb_types::id::DatabaseId;
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::{AuthenticatedIdentity, Role};
use crate::control::state::SharedState;

use super::error_map::sqlstate_error;

/// Require that the identity is a superuser.
///
/// Emits `AuditEvent::PermissionDenied` and returns SQLSTATE 42501 on failure.
pub fn require_superuser(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    db_id: Option<DatabaseId>,
    action: &str,
) -> PgWireResult<()> {
    if identity.is_superuser {
        Ok(())
    } else {
        audit_permission_denied(state, identity, db_id, action);
        Err(sqlstate_error(
            sqlstate::INSUFFICIENT_PRIVILEGE,
            &format!("permission denied: {action} requires superuser"),
        ))
    }
}

/// Require that the identity is superuser or tenant_admin.
///
/// For tenant- and system-scoped DDL that is not database-DDL. Callers in
/// synonym_group, procedure, change_stream, consumer_group, retention_policy,
/// trigger, apikey, user, tenant/alter_quota, tenant/show_in_database, and
/// grant/* are all tenant- or system-scoped and are intentionally not migrated
/// to the finer-grained database-DDL helpers.
///
/// Note: this gate does NOT emit an audit record on denial. Callers needing
/// auditable denials at the database scope should use one of the
/// `require_*_admin` / `require_database_owner*` helpers instead.
pub fn require_tenant_admin(identity: &AuthenticatedIdentity, action: &str) -> PgWireResult<()> {
    if identity.is_superuser || identity.has_role(&Role::TenantAdmin) {
        Ok(())
    } else {
        Err(sqlstate_error(
            sqlstate::INSUFFICIENT_PRIVILEGE,
            &format!("permission denied: only superuser or tenant_admin can {action}"),
        ))
    }
}

/// Required role: `ClusterAdmin` or `Superuser`.
///
/// On failure, emits `AuditEvent::PermissionDenied` and returns SQLSTATE 42501.
pub fn require_cluster_admin(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    db_id: Option<DatabaseId>,
    action: &str,
) -> PgWireResult<()> {
    if identity.has_cluster_admin() {
        Ok(())
    } else {
        audit_permission_denied(state, identity, db_id, action);
        Err(sqlstate_error(
            sqlstate::INSUFFICIENT_PRIVILEGE,
            &format!("permission denied: {action} requires cluster_admin or superuser"),
        ))
    }
}

/// Required role: `DatabaseOwner(db)` or `Superuser`.
///
/// On failure, emits `AuditEvent::PermissionDenied` and returns SQLSTATE 42501.
pub fn require_database_owner(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    db_id: DatabaseId,
    action: &str,
) -> PgWireResult<()> {
    if identity.is_database_owner(db_id) {
        Ok(())
    } else {
        audit_permission_denied(state, identity, Some(db_id), action);
        Err(sqlstate_error(
            sqlstate::INSUFFICIENT_PRIVILEGE,
            &format!(
                "permission denied: {action} requires database_owner of this database or superuser"
            ),
        ))
    }
}

/// Required role: `DatabaseOwner(db)`, `ClusterAdmin`, or `Superuser`.
///
/// On failure, emits `AuditEvent::PermissionDenied` and returns SQLSTATE 42501.
pub fn require_database_owner_or_higher(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    db_id: DatabaseId,
    action: &str,
) -> PgWireResult<()> {
    if identity.is_superuser || identity.has_cluster_admin() || identity.is_database_owner(db_id) {
        Ok(())
    } else {
        audit_permission_denied(state, identity, Some(db_id), action);
        Err(sqlstate_error(
            sqlstate::INSUFFICIENT_PRIVILEGE,
            &format!(
                "permission denied: {action} requires database_owner of this database, \
                 cluster_admin, or superuser"
            ),
        ))
    }
}

/// Centralized denial-audit emitter used by all `require_*` helpers that
/// surface a database scope.
fn audit_permission_denied(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    db_id: Option<DatabaseId>,
    action: &str,
) {
    state.audit_record_with_db(
        AuditEvent::PermissionDenied,
        Some(identity.tenant_id),
        db_id,
        &identity.username,
        action,
    );
}
