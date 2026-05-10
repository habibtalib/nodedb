// SPDX-License-Identifier: BUSL-1.1

#![deny(clippy::wildcard_enum_match_arm)]

use std::str::FromStr;

use nodedb_types::id::DatabaseId;

/// Built-in and custom roles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Role {
    /// Full access to everything, all tenants, system catalog.
    Superuser,
    /// Cluster-wide admin operations (CREATE DATABASE, SET QUOTA, …).
    /// Permission-driven; cannot bypass RLS, cannot read other databases' data.
    ClusterAdmin,
    /// Full access within own tenant. Can manage users/roles.
    TenantAdmin,
    /// Read + write on granted collections.
    ReadWrite,
    /// Read-only on granted collections.
    ReadOnly,
    /// Read metrics, health, audit. No data access.
    Monitor,
    /// Full DDL + DML ownership of a specific database.
    DatabaseOwner(DatabaseId),
    /// Read + write + CREATE COLLECTION within a specific database.
    DatabaseEditor(DatabaseId),
    /// SELECT access within a specific database.
    DatabaseReader(DatabaseId),
    /// Custom role defined by user.
    Custom(String),
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::Superuser => write!(f, "superuser"),
            Role::ClusterAdmin => write!(f, "cluster_admin"),
            Role::TenantAdmin => write!(f, "tenant_admin"),
            Role::ReadWrite => write!(f, "readwrite"),
            Role::ReadOnly => write!(f, "readonly"),
            Role::Monitor => write!(f, "monitor"),
            Role::DatabaseOwner(db) => write!(f, "database_owner:{}", db.as_u64()),
            Role::DatabaseEditor(db) => write!(f, "database_editor:{}", db.as_u64()),
            Role::DatabaseReader(db) => write!(f, "database_reader:{}", db.as_u64()),
            Role::Custom(name) => write!(f, "{name}"),
        }
    }
}

impl FromStr for Role {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "superuser" => Role::Superuser,
            "cluster_admin" => Role::ClusterAdmin,
            "tenant_admin" => Role::TenantAdmin,
            "readwrite" => Role::ReadWrite,
            "readonly" => Role::ReadOnly,
            "monitor" => Role::Monitor,
            other => {
                // Parse database-scoped role tokens: "database_owner:{id}" etc.
                if let Some(rest) = other.strip_prefix("database_owner:") {
                    if let Ok(id) = rest.parse::<u64>() {
                        return Ok(Role::DatabaseOwner(DatabaseId::new(id)));
                    }
                } else if let Some(rest) = other.strip_prefix("database_editor:") {
                    if let Ok(id) = rest.parse::<u64>() {
                        return Ok(Role::DatabaseEditor(DatabaseId::new(id)));
                    }
                } else if let Some(rest) = other.strip_prefix("database_reader:")
                    && let Ok(id) = rest.parse::<u64>()
                {
                    return Ok(Role::DatabaseReader(DatabaseId::new(id)));
                }
                Role::Custom(other.to_string())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_display_roundtrip() {
        let roles = [
            Role::Superuser,
            Role::ClusterAdmin,
            Role::TenantAdmin,
            Role::ReadWrite,
            Role::ReadOnly,
            Role::Monitor,
        ];
        for role in &roles {
            let s = role.to_string();
            let parsed: Role = s.parse().unwrap();
            assert_eq!(*role, parsed);
        }
    }

    #[test]
    fn database_role_display_roundtrip() {
        let db = DatabaseId::new(42);
        let roles = [
            Role::DatabaseOwner(db),
            Role::DatabaseEditor(db),
            Role::DatabaseReader(db),
        ];
        for role in &roles {
            let s = role.to_string();
            let parsed: Role = s.parse().unwrap();
            assert_eq!(*role, parsed, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn role_permission_mapping_cluster_admin_no_data_perms() {
        use super::super::permission::{Permission, role_grants_permission};
        for perm in [Permission::Read, Permission::Write, Permission::Admin] {
            assert!(
                !role_grants_permission(&Role::ClusterAdmin, perm),
                "ClusterAdmin must not implicitly grant {perm:?}"
            );
        }
    }

    #[test]
    fn custom_role_falls_through() {
        let role: Role = "my_custom_role".parse().unwrap();
        assert_eq!(role, Role::Custom("my_custom_role".into()));
    }

    #[test]
    fn database_role_with_invalid_id_falls_through_to_custom() {
        // Malformed database role tokens should not silently map to a default
        // DatabaseId — they fall through to Custom so the caller can detect
        // them with explicit comparison.
        let role: Role = "database_owner:not_a_number".parse().unwrap();
        assert_eq!(role, Role::Custom("database_owner:not_a_number".into()));
    }
}
