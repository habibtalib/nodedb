// SPDX-License-Identifier: BUSL-1.1

//! In-memory API key shapes — `KeyScope`, `CreateKeyParams`, `ApiKeyRecord`.
//!
//! `ApiKeyRecord` is the runtime view; `StoredApiKey` (in
//! `super::super::catalog`) is the persisted form. Bidirectional mapping
//! between them lives here so the catalog encoding never leaks into the store.

use nodedb_types::id::DatabaseId;

use crate::control::security::catalog::StoredApiKey;
use crate::types::TenantId;

use super::token::now_unix_secs;

/// A single scoped permission on a key: (permission, collection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyScope {
    pub permission: String,
    pub collection: String,
}

/// Parameters for `create_key` / `prepare_key`.
///
/// Grouped into a struct because the option set has grown beyond the readable
/// positional-argument threshold and to keep new optional knobs (e.g. future
/// per-key rate limits) additive without a parameter cascade.
pub struct CreateKeyParams<'a> {
    pub username: &'a str,
    pub user_id: u64,
    pub tenant_id: TenantId,
    pub expires_secs: u64,
    pub scope: Vec<KeyScope>,
    /// Database IDs this key may access. Empty = inherit from owner's set
    /// at bind time. Non-empty must already be a subset of the owner's set
    /// (validated by the caller before reaching this layer).
    pub accessible_databases: Vec<DatabaseId>,
}

/// In-memory API key record.
#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    pub key_id: String,
    pub secret_hash: Vec<u8>,
    pub username: String,
    pub user_id: u64,
    pub tenant_id: TenantId,
    pub expires_at: u64,
    pub is_revoked: bool,
    pub created_at: u64,
    /// Permission scope restriction. Empty = inherit all user permissions.
    pub scope: Vec<KeyScope>,
    /// Database IDs this key may access. Empty = inherit from owner's set at bind time.
    pub accessible_databases: Vec<DatabaseId>,
}

impl ApiKeyRecord {
    pub(super) fn to_stored(&self) -> StoredApiKey {
        StoredApiKey {
            key_id: self.key_id.clone(),
            secret_hash: self.secret_hash.clone(),
            username: self.username.clone(),
            user_id: self.user_id,
            tenant_id: self.tenant_id.as_u64(),
            expires_at: self.expires_at,
            is_revoked: self.is_revoked,
            created_at: self.created_at,
            scope: self
                .scope
                .iter()
                .map(|s| format!("{}:{}", s.permission, s.collection))
                .collect(),
            accessible_databases: self
                .accessible_databases
                .iter()
                .map(|id| id.as_u64())
                .collect(),
        }
    }

    pub(super) fn from_stored(s: StoredApiKey) -> Self {
        let scope = s
            .scope
            .iter()
            .filter_map(|s| {
                let (perm, coll) = s.split_once(':')?;
                Some(KeyScope {
                    permission: perm.to_string(),
                    collection: coll.to_string(),
                })
            })
            .collect();
        let accessible_databases = s
            .accessible_databases
            .iter()
            .map(|&id| DatabaseId::new(id))
            .collect();
        Self {
            key_id: s.key_id,
            secret_hash: s.secret_hash,
            username: s.username,
            user_id: s.user_id,
            tenant_id: TenantId::new(s.tenant_id),
            expires_at: s.expires_at,
            is_revoked: s.is_revoked,
            created_at: s.created_at,
            scope,
            accessible_databases,
        }
    }

    /// Check if the key is currently valid (not revoked, not expired).
    pub fn is_valid(&self) -> bool {
        if self.is_revoked {
            return false;
        }
        if self.expires_at > 0 {
            let now = now_unix_secs();
            if now >= self.expires_at {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_with_accessible_databases_persists_through_stored_roundtrip() {
        let db1 = DatabaseId::new(1);
        let db2 = DatabaseId::new(2);
        let record = ApiKeyRecord {
            key_id: "testkey1".into(),
            secret_hash: vec![0u8; 32],
            username: "alice".into(),
            user_id: 1,
            tenant_id: TenantId::new(1),
            expires_at: 0,
            is_revoked: false,
            created_at: 100,
            scope: vec![],
            accessible_databases: vec![db1, db2],
        };
        let stored = record.to_stored();
        assert_eq!(stored.accessible_databases, vec![1u64, 2u64]);
        let recovered = ApiKeyRecord::from_stored(stored);
        assert_eq!(recovered.accessible_databases, vec![db1, db2]);
    }

    #[test]
    fn inherit_default_uses_empty_accessible_databases() {
        // Empty accessible_databases signals "inherit from owner at bind time"
        let record = ApiKeyRecord {
            key_id: "testkey2".into(),
            secret_hash: vec![0u8; 32],
            username: "bob".into(),
            user_id: 2,
            tenant_id: TenantId::new(1),
            expires_at: 0,
            is_revoked: false,
            created_at: 100,
            scope: vec![],
            accessible_databases: vec![],
        };
        let stored = record.to_stored();
        assert!(stored.accessible_databases.is_empty());
        let recovered = ApiKeyRecord::from_stored(stored);
        assert!(recovered.accessible_databases.is_empty());
    }
}
