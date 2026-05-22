// SPDX-License-Identifier: BUSL-1.1

//! Permission evaluation: `check`, `check_function`, `is_owner`.
//!
//! Multi-layer order: superuser → owner → built-in role → explicit
//! user grant → role grants (with custom-role inheritance).

use crate::control::security::audit::{AuditEmitContext, AuditEmitter, AuditEvent};
use crate::control::security::identity::{self, AuthenticatedIdentity, Permission};
use crate::control::security::role::RoleStore;

use crate::types::TenantId;

use super::store::PermissionStore;
use super::types::{Grant, collection_target, function_target, tenant_target};

impl PermissionStore {
    /// Does any grant on `target` confer `permission` to this identity —
    /// either through an explicit `user:<name>` grant or through any role
    /// in the identity's inheritance chain?
    ///
    /// Acquires the grants read lock for the duration of the lookup.
    fn target_grants_permission(
        &self,
        target: &str,
        permission: Permission,
        identity: &AuthenticatedIdentity,
        role_store: &RoleStore,
    ) -> bool {
        let grants = match self.grants.read() {
            Ok(g) => g,
            Err(p) => {
                tracing::error!("permission grants lock poisoned — recovering data");
                p.into_inner()
            }
        };

        let user_grantee = format!("user:{}", identity.username);
        if grants.contains(&Grant {
            target: target.to_string(),
            grantee: user_grantee,
            permission,
        }) {
            return true;
        }

        for role in &identity.roles {
            let chain = match role_store.resolve_inheritance(role) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "failed to resolve role inheritance — denying");
                    continue;
                }
            };
            for ancestor in &chain {
                if grants.contains(&Grant {
                    target: target.to_string(),
                    grantee: ancestor.to_string(),
                    permission,
                }) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if an identity has a specific permission on a collection.
    ///
    /// Checks in order:
    /// 1. Superuser → always allowed
    /// 2. Ownership → owner has all permissions on their objects
    /// 3. Built-in role grants (from identity.rs role_grants_permission)
    /// 4. Explicit collection-level grants (on user or any of user's roles)
    /// 5. Custom role inheritance chain (via `RoleStore`)
    ///
    /// When access is denied (returns `false`) the decision is emitted to
    /// `emitter` as `AuditEvent::PermissionDenied`.  Pass
    /// `&NoopAuditEmitter` from callers that are not the terminal denial
    /// point (e.g. multi-layer fallback chains that try broader scopes
    /// after this call).
    pub fn check(
        &self,
        identity: &AuthenticatedIdentity,
        permission: Permission,
        collection: &str,
        role_store: &RoleStore,
        emitter: &dyn AuditEmitter,
    ) -> bool {
        if identity.is_superuser {
            return true;
        }

        let target = collection_target(identity.tenant_id, collection);
        if self.is_owner(&target, &identity.username) {
            return true;
        }

        for role in &identity.roles {
            if identity::role_grants_permission(role, permission) {
                return true;
            }
        }

        // Explicit grant on the collection itself.
        if self.target_grants_permission(&target, permission, identity, role_store) {
            return true;
        }

        // Tenant-wide grant — `GRANT <perm> ON TENANT <name>` confers the
        // permission on every collection in the tenant.
        let tenant_tgt = tenant_target(identity.tenant_id);
        if self.target_grants_permission(&tenant_tgt, permission, identity, role_store) {
            return true;
        }

        emitter.emit(
            AuditEvent::PermissionDenied,
            &identity.username,
            &format!(
                "permission {:?} denied on '{}' for user '{}'",
                permission, collection, identity.username
            ),
            AuditEmitContext::new(
                Some(identity.tenant_id),
                &identity.user_id.to_string(),
                &identity.username,
            ),
        );
        false
    }

    /// Check if an identity has EXECUTE permission on a function.
    ///
    /// Same multi-layer check as [`Self::check`] but uses
    /// `function:tenant:name` targets. Function owners implicitly
    /// have EXECUTE.  Emits `AuditEvent::PermissionDenied` via
    /// `emitter` when access is denied.
    pub fn check_function(
        &self,
        identity: &AuthenticatedIdentity,
        function_name: &str,
        role_store: &RoleStore,
        emitter: &dyn AuditEmitter,
    ) -> bool {
        if identity.is_superuser {
            return true;
        }

        let target = function_target(identity.tenant_id, function_name);

        if self.is_owner(&target, &identity.username) {
            return true;
        }

        for role in &identity.roles {
            if identity::role_grants_permission(role, Permission::Execute) {
                return true;
            }
        }

        if self.target_grants_permission(&target, Permission::Execute, identity, role_store) {
            return true;
        }

        emitter.emit(
            AuditEvent::PermissionDenied,
            &identity.username,
            &format!(
                "EXECUTE permission denied on function '{}' for user '{}'",
                function_name, identity.username
            ),
            AuditEmitContext::new(
                Some(identity.tenant_id),
                &identity.user_id.to_string(),
                &identity.username,
            ),
        );
        false
    }

    /// Check if an identity holds `permission` scoped to an entire tenant
    /// (`GRANT <perm> ON TENANT <name>`).
    ///
    /// Used for tenant-wide operations such as `BACKUP TENANT` /
    /// `RESTORE TENANT`. Checks superuser → built-in role grants → explicit
    /// tenant-scoped grants (on the user or any of the user's roles). Emits
    /// `AuditEvent::PermissionDenied` via `emitter` when access is denied.
    pub fn check_tenant(
        &self,
        identity: &AuthenticatedIdentity,
        permission: Permission,
        tenant_id: TenantId,
        role_store: &RoleStore,
        emitter: &dyn AuditEmitter,
    ) -> bool {
        if identity.is_superuser {
            return true;
        }

        for role in &identity.roles {
            if identity::role_grants_permission(role, permission) {
                return true;
            }
        }

        let target = tenant_target(tenant_id);
        if self.target_grants_permission(&target, permission, identity, role_store) {
            return true;
        }

        emitter.emit(
            AuditEvent::PermissionDenied,
            &identity.username,
            &format!(
                "permission {:?} denied on tenant {} for user '{}'",
                permission,
                tenant_id.as_u64(),
                identity.username
            ),
            AuditEmitContext::new(
                Some(identity.tenant_id),
                &identity.user_id.to_string(),
                &identity.username,
            ),
        );
        false
    }

    /// Lookup helper: is `username` recorded as the owner of `target`?
    pub(super) fn is_owner(&self, target: &str, username: &str) -> bool {
        let owners = match self.owners.read() {
            Ok(o) => o,
            Err(p) => {
                tracing::error!("owner store lock poisoned — recovering data");
                p.into_inner()
            }
        };
        owners.get(target).is_some_and(|o| o == username)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::security::audit::NoopAuditEmitter;
    use crate::control::security::identity::{AuthMethod, Role};
    use crate::types::TenantId;

    const NOOP: &NoopAuditEmitter = &NoopAuditEmitter;

    fn identity(username: &str, roles: Vec<Role>, superuser: bool) -> AuthenticatedIdentity {
        use crate::control::security::identity::DatabaseSet;
        AuthenticatedIdentity {
            user_id: 1,
            username: username.into(),
            tenant_id: TenantId::new(1),
            auth_method: AuthMethod::Trust,
            roles,
            is_superuser: superuser,
            default_database: None,
            accessible_databases: if superuser {
                DatabaseSet::All
            } else {
                DatabaseSet::Some(smallvec::smallvec![nodedb_types::id::DatabaseId::DEFAULT])
            },
        }
    }

    #[test]
    fn superuser_always_allowed() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let id = identity("admin", vec![], true);
        assert!(store.check(&id, Permission::Write, "secret", &roles, NOOP));
    }

    #[test]
    fn owner_has_all_permissions() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        store
            .set_owner("collection", TenantId::new(1), "users", "alice", None)
            .unwrap();

        let id = identity("alice", vec![], false);
        assert!(store.check(&id, Permission::Read, "users", &roles, NOOP));
        assert!(store.check(&id, Permission::Write, "users", &roles, NOOP));
        assert!(store.check(&id, Permission::Drop, "users", &roles, NOOP));
    }

    #[test]
    fn non_owner_denied_without_grant() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        store
            .set_owner("collection", TenantId::new(1), "users", "alice", None)
            .unwrap();

        let id = identity("bob", vec![], false);
        assert!(!store.check(&id, Permission::Write, "users", &roles, NOOP));
    }

    #[test]
    fn explicit_user_grant() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let target = collection_target(TenantId::new(1), "orders");
        store
            .grant(&target, "user:bob", Permission::Read, "admin", None)
            .unwrap();

        let id = identity("bob", vec![], false);
        assert!(store.check(&id, Permission::Read, "orders", &roles, NOOP));
        assert!(!store.check(&id, Permission::Write, "orders", &roles, NOOP));
    }

    #[test]
    fn grant_on_role() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let target = collection_target(TenantId::new(1), "reports");
        store
            .grant(&target, "readonly", Permission::Read, "admin", None)
            .unwrap();

        let id = identity("viewer", vec![Role::Custom("readonly".into())], false);
        assert!(store.check(&id, Permission::Read, "reports", &roles, NOOP));
    }

    #[test]
    fn inherited_role_grant() {
        let role_store = RoleStore::new();
        role_store
            .create_role("analyst", TenantId::new(1), Some("readonly"), None)
            .unwrap();

        let perm_store = PermissionStore::new();
        let target = collection_target(TenantId::new(1), "data");
        perm_store
            .grant(&target, "readonly", Permission::Read, "admin", None)
            .unwrap();

        let id = identity("alice", vec![Role::Custom("analyst".into())], false);
        assert!(perm_store.check(&id, Permission::Read, "data", &role_store, NOOP));
    }

    #[test]
    fn revoke_removes_grant() {
        let store = PermissionStore::new();
        let target = collection_target(TenantId::new(1), "users");
        store
            .grant(&target, "user:bob", Permission::Read, "admin", None)
            .unwrap();
        assert!(
            store
                .revoke(&target, "user:bob", Permission::Read, None)
                .unwrap()
        );

        let roles = RoleStore::new();
        let id = identity("bob", vec![], false);
        assert!(!store.check(&id, Permission::Read, "users", &roles, NOOP));
    }

    #[test]
    fn builtin_role_still_works() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let id = identity("writer", vec![Role::ReadWrite], false);
        assert!(store.check(&id, Permission::Read, "anything", &roles, NOOP));
        assert!(store.check(&id, Permission::Write, "anything", &roles, NOOP));
        assert!(!store.check(&id, Permission::Drop, "anything", &roles, NOOP));
    }

    #[test]
    fn denied_check_emits_permission_denied() {
        use crate::control::security::audit::emitter::test_helpers::CapturingEmitter;

        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let emitter = CapturingEmitter::new();
        let id = identity("eve", vec![], false);

        let allowed = store.check(&id, Permission::Write, "secrets", &roles, &emitter);
        assert!(!allowed);

        let recorded = emitter.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, AuditEvent::PermissionDenied);
    }

    #[test]
    fn allowed_check_does_not_emit() {
        use crate::control::security::audit::emitter::test_helpers::CapturingEmitter;

        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let emitter = CapturingEmitter::new();
        let id = identity("admin", vec![], true);

        let allowed = store.check(&id, Permission::Write, "anything", &roles, &emitter);
        assert!(allowed);
        assert!(emitter.recorded().is_empty());
    }

    #[test]
    fn tenant_wide_grant_covers_every_collection() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        // `GRANT READ ON TENANT <name>` lands on the tenant target.
        let target = tenant_target(TenantId::new(1));
        store
            .grant(&target, "user:bob", Permission::Read, "admin", None)
            .unwrap();

        let id = identity("bob", vec![], false);
        // A tenant-wide grant confers the permission on any collection in
        // the tenant, with no per-collection grant.
        assert!(store.check(&id, Permission::Read, "orders", &roles, NOOP));
        assert!(store.check(&id, Permission::Read, "invoices", &roles, NOOP));
        // It does not widen to permissions that were not granted.
        assert!(!store.check(&id, Permission::Write, "orders", &roles, NOOP));
    }

    #[test]
    fn check_tenant_honors_explicit_grant() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let target = tenant_target(TenantId::new(1));
        store
            .grant(&target, "user:ops", Permission::Backup, "admin", None)
            .unwrap();

        let granted = identity("ops", vec![], false);
        assert!(store.check_tenant(&granted, Permission::Backup, TenantId::new(1), &roles, NOOP));

        // A different user without the grant is denied.
        let other = identity("eve", vec![], false);
        assert!(!store.check_tenant(&other, Permission::Backup, TenantId::new(1), &roles, NOOP));
    }

    #[test]
    fn check_tenant_superuser_always_allowed() {
        let store = PermissionStore::new();
        let roles = RoleStore::new();
        let id = identity("admin", vec![], true);
        assert!(store.check_tenant(&id, Permission::Backup, TenantId::new(9), &roles, NOOP));
    }
}
