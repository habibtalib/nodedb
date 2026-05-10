// SPDX-License-Identifier: BUSL-1.1

//! Per-database DML audit mode cache.
//!
//! Stores the `AuditDmlMode` for each database in memory so the Event Plane
//! consumer can check it without a catalog round-trip on every write event.
//! Updated synchronously in the `ALTER DATABASE SET AUDIT_DML` handler.

use std::collections::HashMap;
use std::sync::RwLock;

use nodedb_types::{AuditDmlMode, DatabaseId};

use crate::control::security::catalog::SystemCatalog;

/// In-memory cache of per-database DML audit modes.
///
/// Control Plane only (`Send + Sync`). Backed by a `RwLock<HashMap>` for
/// concurrent reads with infrequent writes.
pub struct AuditDmlCache {
    inner: RwLock<HashMap<DatabaseId, AuditDmlMode>>,
}

impl AuditDmlCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Return the current DML audit mode for `db_id`.
    ///
    /// Returns `AuditDmlMode::None` on cache miss (safe default: no extra auditing).
    pub fn get(&self, db_id: DatabaseId) -> AuditDmlMode {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&db_id)
            .copied()
            .unwrap_or(AuditDmlMode::None)
    }

    /// Update the DML audit mode for `db_id`.
    pub fn set(&self, db_id: DatabaseId, mode: AuditDmlMode) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(db_id, mode);
    }

    /// Populate the cache from all descriptors in the catalog.
    ///
    /// Called at startup. Only databases with non-None `audit_dml` are inserted;
    /// the `get` method returns `None` for unknown databases (same semantics).
    pub fn load_from_catalog(&self, catalog: &SystemCatalog) -> crate::Result<()> {
        let databases = catalog.list_databases()?;
        let mut map = self.inner.write().unwrap_or_else(|p| p.into_inner());
        for descriptor in databases {
            if descriptor.audit_dml != AuditDmlMode::None {
                map.insert(descriptor.id, descriptor.audit_dml);
            }
        }
        Ok(())
    }
}

impl Default for AuditDmlCache {
    fn default() -> Self {
        Self::new()
    }
}
