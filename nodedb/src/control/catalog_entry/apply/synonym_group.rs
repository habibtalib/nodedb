//! Apply synonym group catalog entries to `SystemCatalog` redb.

use tracing::warn;

use crate::control::security::catalog::{StoredSynonymGroup, SystemCatalog};

pub fn put(stored: &StoredSynonymGroup, catalog: &SystemCatalog) {
    if let Err(e) = catalog.put_synonym_group(stored) {
        warn!(
            group = %stored.name,
            tenant = stored.tenant_id,
            error = %e,
            "catalog_entry: put_synonym_group failed"
        );
    }
}

pub fn delete(tenant_id: u64, name: &str, catalog: &SystemCatalog) {
    if let Err(e) = catalog.delete_synonym_group(tenant_id, name) {
        warn!(
            group = %name,
            tenant = tenant_id,
            error = %e,
            "catalog_entry: delete_synonym_group failed"
        );
    }
}
