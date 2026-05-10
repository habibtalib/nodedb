// SPDX-License-Identifier: BUSL-1.1

#![deny(clippy::wildcard_enum_match_arm)]

use nodedb_types::id::DatabaseId;

use crate::types::TenantId;

use super::database_set::DatabaseSet;
use super::role::Role;

/// A verified identity bound to a session after authentication.
///
/// This is the single source of truth for "who is this connection?"
/// Created during auth handshake, immutable for the session lifetime.
/// Tenant ID comes from here — never from client payload.
#[derive(Debug, Clone)]
pub struct AuthenticatedIdentity {
    /// Unique user identifier.
    pub user_id: u64,
    /// Username (for display, logging, audit).
    pub username: String,
    /// Tenant this user belongs to.
    ///
    /// Single-tenant per user; the database is the multi-axis.
    /// Cross-tenant access requires separate user accounts per tenant, or superuser.
    /// No code path branches on "user belongs to multiple tenants" —
    /// the single-tenant invariant holds throughout the codebase.
    pub tenant_id: TenantId,
    /// How the user authenticated.
    pub auth_method: AuthMethod,
    /// Assigned roles.
    pub roles: Vec<Role>,
    /// Whether this user is a superuser (bypasses all permission checks).
    pub is_superuser: bool,
    /// Per-user default database. `None` means fall through to tenant default,
    /// then `DatabaseId::DEFAULT`.
    ///
    /// Set via `ALTER USER <name> SET DEFAULT DATABASE <db>` and stored in
    /// the credential store alongside the user record.
    pub default_database: Option<DatabaseId>,
    /// Which databases this identity may access.
    ///
    /// Superusers carry `DatabaseSet::All`. Regular users start with
    /// `DatabaseSet::Some([DatabaseId::DEFAULT])` and gain additional entries
    /// via `GRANT … ON DATABASE …`. Session bind rejects `current_database`
    /// values not in this set with `ACCESS_DENIED`.
    pub accessible_databases: DatabaseSet,
}

impl AuthenticatedIdentity {
    /// Check if this identity has a specific role.
    pub fn has_role(&self, role: &Role) -> bool {
        self.is_superuser || self.roles.contains(role)
    }

    /// Check if this identity has any of the specified roles.
    pub fn has_any_role(&self, roles: &[Role]) -> bool {
        self.is_superuser || roles.iter().any(|r| self.roles.contains(r))
    }

    /// Returns `true` if this identity is Superuser or carries `Role::ClusterAdmin`.
    pub fn has_cluster_admin(&self) -> bool {
        self.is_superuser || self.roles.iter().any(|r| matches!(r, Role::ClusterAdmin))
    }

    /// Returns `true` if this identity is the owner of `db` (or is Superuser).
    pub fn is_database_owner(&self, db: DatabaseId) -> bool {
        self.is_superuser
            || self
                .roles
                .iter()
                .any(|r| matches!(r, Role::DatabaseOwner(d) if *d == db))
    }

    /// Returns `true` if this identity may access the given database.
    ///
    /// Superusers always return `true`. Regular users return `true` only if
    /// the database is in `accessible_databases`. This is enforced at session
    /// bind — the session is rejected with `ACCESS_DENIED` if the resolved
    /// `current_database` fails this check.
    pub fn can_access_database(&self, db: DatabaseId) -> bool {
        self.is_superuser || self.accessible_databases.contains(db)
    }

    /// Derive the appropriate `DatabaseSet` for a superuser identity.
    ///
    /// Superusers receive `DatabaseSet::All`; regular users start with
    /// `DatabaseSet::Some([DatabaseId::DEFAULT])`.
    pub fn default_database_set(is_superuser: bool) -> DatabaseSet {
        if is_superuser {
            DatabaseSet::All
        } else {
            DatabaseSet::Some(smallvec::smallvec![DatabaseId::DEFAULT])
        }
    }
}

/// How the client proved their identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// SCRAM-SHA-256 via pgwire.
    ScramSha256,
    /// Cleartext password (dev/testing only).
    CleartextPassword,
    /// API key (bearer token).
    ApiKey,
    /// mTLS client certificate.
    Certificate,
    /// Trust mode (no authentication — dev only).
    Trust,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity(roles: Vec<Role>, superuser: bool) -> AuthenticatedIdentity {
        AuthenticatedIdentity {
            user_id: 1,
            username: "test".into(),
            tenant_id: TenantId::new(1),
            auth_method: AuthMethod::Trust,
            roles,
            is_superuser: superuser,
            default_database: None,
            accessible_databases: AuthenticatedIdentity::default_database_set(superuser),
        }
    }

    #[test]
    fn superuser_has_all_roles() {
        let id = test_identity(vec![], true);
        assert!(id.has_role(&Role::ReadOnly));
        assert!(id.has_role(&Role::TenantAdmin));
        assert!(id.has_role(&Role::Custom("anything".into())));
    }

    #[test]
    fn readonly_only_has_readonly() {
        let id = test_identity(vec![Role::ReadOnly], false);
        assert!(id.has_role(&Role::ReadOnly));
        assert!(!id.has_role(&Role::ReadWrite));
        assert!(!id.has_role(&Role::TenantAdmin));
    }

    #[test]
    fn superuser_can_access_any_database() {
        let id = test_identity(vec![], true);
        assert!(id.can_access_database(DatabaseId::new(99)));
    }

    #[test]
    fn regular_user_only_default_database() {
        let id = test_identity(vec![Role::ReadOnly], false);
        assert!(id.can_access_database(DatabaseId::DEFAULT));
        assert!(!id.can_access_database(DatabaseId::new(99)));
    }
}
