//! In-memory custom type registry (Control Plane, `Send + Sync`).
//!
//! Loaded from the system catalog on startup. Updated by DDL handlers.
//! The registry is the single source of truth for duplicate detection,
//! SHOW TYPES queries, OID assignment, and drop-protection checks.

use std::collections::HashMap;
use std::sync::{
    RwLock,
    atomic::{AtomicU32, Ordering},
};

use crate::control::security::catalog::{CustomTypeDef, StoredCustomType};

/// Base OID for user-defined types. PG built-in OIDs end well below 10000;
/// extension OIDs typically start at 16384. We use 70000+ to leave room.
const USER_TYPE_OID_BASE: u32 = 70_000;

/// In-memory custom type registry.
pub struct CustomTypeRegistry {
    /// `(tenant_id, type_name)` → `StoredCustomType`.
    by_name: RwLock<HashMap<(u64, String), StoredCustomType>>,
    /// Next OID to assign. Starts at `USER_TYPE_OID_BASE + 1` and increments.
    next_oid: AtomicU32,
}

impl CustomTypeRegistry {
    pub fn new() -> Self {
        Self {
            by_name: RwLock::new(HashMap::new()),
            next_oid: AtomicU32::new(USER_TYPE_OID_BASE + 1),
        }
    }

    /// Allocate the next available OID. The value is stable for the lifetime
    /// of this process but is NOT persisted here — the DDL handler persists
    /// the chosen OID inside `StoredCustomType` before writing to the catalog.
    pub fn alloc_oid(&self) -> u32 {
        self.next_oid.fetch_add(1, Ordering::Relaxed)
    }

    /// Insert or replace a type in the registry. Also advances `next_oid`
    /// past the stored OID to avoid collisions after restart-reload.
    pub fn register(&self, def: StoredCustomType) {
        let next = def.oid.saturating_add(1);
        self.next_oid.fetch_max(next, Ordering::Relaxed);
        let key = (def.tenant_id, def.name.clone());
        let mut map = self.by_name.write().unwrap_or_else(|p| p.into_inner());
        map.insert(key, def);
    }

    /// Remove a custom type. Returns `true` if it existed.
    pub fn unregister(&self, tenant_id: u64, name: &str) -> bool {
        let key = (tenant_id, name.to_string());
        let mut map = self.by_name.write().unwrap_or_else(|p| p.into_inner());
        map.remove(&key).is_some()
    }

    /// Check whether a type exists.
    pub fn exists(&self, tenant_id: u64, name: &str) -> bool {
        let key = (tenant_id, name.to_string());
        let map = self.by_name.read().unwrap_or_else(|p| p.into_inner());
        map.contains_key(&key)
    }

    /// Get a type by name.
    pub fn get(&self, tenant_id: u64, name: &str) -> Option<StoredCustomType> {
        let key = (tenant_id, name.to_string());
        let map = self.by_name.read().unwrap_or_else(|p| p.into_inner());
        map.get(&key).cloned()
    }

    /// List all types for a tenant.
    pub fn list_for_tenant(&self, tenant_id: u64) -> Vec<StoredCustomType> {
        let map = self.by_name.read().unwrap_or_else(|p| p.into_inner());
        map.values()
            .filter(|t| t.tenant_id == tenant_id)
            .cloned()
            .collect()
    }

    /// Return the pg OID for a named type, or `None` if unknown.
    pub fn oid_for(&self, tenant_id: u64, name: &str) -> Option<u32> {
        self.get(tenant_id, name).map(|t| t.oid)
    }

    /// Validate that `value` is a legal label for the enum type `name`.
    /// Returns `Ok(())` if valid or if the type is not an enum.
    /// Returns `Err(invalid_label)` if the type exists but the label is not in it.
    pub fn validate_enum_label(
        &self,
        tenant_id: u64,
        type_name: &str,
        value: &str,
    ) -> Result<(), String> {
        match self.get(tenant_id, type_name) {
            Some(StoredCustomType {
                def: CustomTypeDef::Enum { labels },
                ..
            }) => {
                if labels.iter().any(|l| l == value) {
                    Ok(())
                } else {
                    Err(format!(
                        "invalid input value for enum \"{type_name}\": \"{value}\""
                    ))
                }
            }
            _ => Ok(()),
        }
    }

    /// Reload from catalog. Used at startup and by recovery verifier.
    pub fn reload_from_catalog(
        &self,
        catalog: &crate::control::security::catalog::SystemCatalog,
    ) -> crate::Result<()> {
        let fresh = catalog.load_all_custom_types()?;
        let mut map = self.by_name.write().unwrap_or_else(|p| p.into_inner());
        map.clear();
        for t in fresh {
            let next = t.oid.saturating_add(1);
            self.next_oid.fetch_max(next, Ordering::Relaxed);
            let key = (t.tenant_id, t.name.clone());
            map.insert(key, t);
        }
        Ok(())
    }
}

impl Default for CustomTypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}
