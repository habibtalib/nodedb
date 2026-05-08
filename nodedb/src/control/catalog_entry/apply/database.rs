// SPDX-License-Identifier: BUSL-1.1

//! Apply database catalog entries to `SystemCatalog` redb.

use tracing::warn;

use crate::control::security::catalog::SystemCatalog;
use crate::control::security::catalog::database_types::DatabaseDescriptor;
use nodedb_types::DatabaseId;

/// Apply a `PutDatabase` entry — upsert the descriptor into
/// `_system.databases` and `_system.databases_by_name`.
pub fn put(descriptor: &DatabaseDescriptor, catalog: &SystemCatalog) {
    if let Err(e) = catalog.put_database(descriptor) {
        warn!(
            db_id = descriptor.id.as_u64(),
            name = %descriptor.name,
            error = %e,
            "catalog_entry: put_database failed"
        );
    }
}

/// Apply a `DeleteDatabase` entry — remove the descriptor and its
/// reverse-lookup row from the catalog.
pub fn delete(db_id: u64, catalog: &SystemCatalog) {
    if let Err(e) = catalog.delete_database(DatabaseId::new(db_id)) {
        warn!(
            db_id,
            error = %e,
            "catalog_entry: delete_database failed"
        );
    }
}

/// Apply a `PutDatabaseGrant` entry.
pub fn put_grant(db_id: u64, user_id: u64, privilege: &str, catalog: &SystemCatalog) {
    let db = DatabaseId::new(db_id);
    if let Err(e) = catalog.put_database_grant(db, user_id, privilege) {
        warn!(
            db_id,
            user_id,
            privilege,
            error = %e,
            "catalog_entry: put_database_grant failed"
        );
    }
}

/// Apply a `DeleteDatabaseGrant` entry.
pub fn delete_grant(db_id: u64, user_id: u64, privilege: &str, catalog: &SystemCatalog) {
    let db = DatabaseId::new(db_id);
    if let Err(e) = catalog.delete_database_grant(db, user_id, privilege) {
        warn!(
            db_id,
            user_id,
            privilege,
            error = %e,
            "catalog_entry: delete_database_grant failed"
        );
    }
}
