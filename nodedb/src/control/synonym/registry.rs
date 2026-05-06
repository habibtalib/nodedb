//! In-memory registry of synonym groups.
//!
//! Loaded from the system catalog on startup. Updated by DDL handlers.
//! The registry is the single source of truth for duplicate detection and
//! SHOW SYNONYM GROUPS queries in the Control Plane.
//!
//! Query-time synonym expansion happens in the Data Plane via the FTS backend.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::control::security::catalog::StoredSynonymGroup;

/// In-memory synonym group registry (Control Plane, `Send + Sync`).
pub struct SynonymRegistry {
    /// `(tenant_id, group_name)` → `StoredSynonymGroup`.
    by_name: RwLock<HashMap<(u64, String), StoredSynonymGroup>>,
}

impl SynonymRegistry {
    pub fn new() -> Self {
        Self {
            by_name: RwLock::new(HashMap::new()),
        }
    }

    /// Insert or replace a synonym group in the registry.
    pub fn register(&self, def: StoredSynonymGroup) {
        let key = (def.tenant_id, def.name.clone());
        let mut map = self.by_name.write().unwrap_or_else(|p| p.into_inner());
        map.insert(key, def);
    }

    /// Remove a synonym group. Returns `true` if it existed.
    pub fn unregister(&self, tenant_id: u64, name: &str) -> bool {
        let key = (tenant_id, name.to_string());
        let mut map = self.by_name.write().unwrap_or_else(|p| p.into_inner());
        map.remove(&key).is_some()
    }

    /// Check whether a synonym group exists.
    pub fn exists(&self, tenant_id: u64, name: &str) -> bool {
        let key = (tenant_id, name.to_string());
        let map = self.by_name.read().unwrap_or_else(|p| p.into_inner());
        map.contains_key(&key)
    }

    /// Get a synonym group by name.
    pub fn get(&self, tenant_id: u64, name: &str) -> Option<StoredSynonymGroup> {
        let key = (tenant_id, name.to_string());
        let map = self.by_name.read().unwrap_or_else(|p| p.into_inner());
        map.get(&key).cloned()
    }

    /// List all synonym groups for a tenant.
    pub fn list_for_tenant(&self, tenant_id: u64) -> Vec<StoredSynonymGroup> {
        let map = self.by_name.read().unwrap_or_else(|p| p.into_inner());
        map.values()
            .filter(|g| g.tenant_id == tenant_id)
            .cloned()
            .collect()
    }

    /// Reload from catalog. Used at startup and by recovery verifier.
    pub fn reload_from_catalog(
        &self,
        catalog: &crate::control::security::catalog::SystemCatalog,
    ) -> crate::Result<()> {
        let fresh = catalog.load_all_synonym_groups()?;
        let mut map = self.by_name.write().unwrap_or_else(|p| p.into_inner());
        map.clear();
        for g in fresh {
            let key = (g.tenant_id, g.name.clone());
            map.insert(key, g);
        }
        Ok(())
    }
}

impl Default for SynonymRegistry {
    fn default() -> Self {
        Self::new()
    }
}
