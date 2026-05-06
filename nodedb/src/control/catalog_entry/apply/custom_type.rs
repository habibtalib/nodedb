//! Apply custom type catalog entries to `SystemCatalog` redb.

use tracing::warn;

use crate::control::security::catalog::{StoredCustomType, SystemCatalog};

pub fn put(stored: &StoredCustomType, catalog: &SystemCatalog) {
    if let Err(e) = catalog.put_custom_type(stored) {
        warn!(
            type_name = %stored.name,
            tenant = stored.tenant_id,
            error = %e,
            "catalog_entry: put_custom_type failed"
        );
    }
}

pub fn delete(tenant_id: u64, name: &str, catalog: &SystemCatalog) {
    if let Err(e) = catalog.delete_custom_type(tenant_id, name) {
        warn!(
            type_name = %name,
            tenant = tenant_id,
            error = %e,
            "catalog_entry: delete_custom_type failed"
        );
    }
}
