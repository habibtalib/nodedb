// SPDX-License-Identifier: BUSL-1.1

//! Database hwm persistence trait + concrete `SystemCatalog`-backed impl.
//!
//! Mirrors `nodedb::control::surrogate::persist` for the database
//! allocator. The trait separates the registry's allocation logic from
//! the storage layer so tests can substitute an in-memory impl.

use std::sync::Arc;

use crate::control::security::catalog::SystemCatalog;

/// Pluggable persistence boundary for `DatabaseRegistry`.
/// Tests substitute an in-memory store; production wires
/// [`SystemCatalogDatabaseHwm`].
pub trait DatabaseHwmPersist: Send + Sync {
    /// Persist the current high-watermark. Called by
    /// `DatabaseRegistry::flush` whenever periodic-flush thresholds
    /// (64 ops or 200 ms) are tripped.
    fn checkpoint(&self, hwm: u64) -> crate::Result<()>;

    /// Load the persisted high-watermark, or `0` if none recorded yet
    /// (fresh database).
    fn load(&self) -> crate::Result<u64>;
}

/// `SystemCatalog`-backed persistence — delegates to
/// `put_database_hwm` / `get_database_hwm`.
pub struct SystemCatalogDatabaseHwm {
    catalog: Arc<SystemCatalog>,
}

impl SystemCatalogDatabaseHwm {
    pub fn new(catalog: Arc<SystemCatalog>) -> Self {
        Self { catalog }
    }
}

impl DatabaseHwmPersist for SystemCatalogDatabaseHwm {
    fn checkpoint(&self, hwm: u64) -> crate::Result<()> {
        self.catalog.put_database_hwm(hwm)
    }

    fn load(&self) -> crate::Result<u64> {
        self.catalog.get_database_hwm()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_roundtrip_via_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Arc::new(SystemCatalog::open(&dir.path().join("system.redb")).unwrap());
        let p = SystemCatalogDatabaseHwm::new(catalog);
        assert_eq!(p.load().unwrap(), 0);
        p.checkpoint(1024).unwrap();
        assert_eq!(p.load().unwrap(), 1024);
    }
}
