// SPDX-License-Identifier: BUSL-1.1

//! Apply User catalog entries to `SystemCatalog` redb.

use tracing::warn;

use crate::control::security::catalog::{StoredUser, SystemCatalog};

pub fn put(stored: &StoredUser, catalog: &SystemCatalog) {
    if let Err(e) = catalog.put_user(stored) {
        warn!(
            username = %stored.username,
            error = %e,
            "catalog_entry: put_user failed"
        );
    }
}

pub fn delete(username: &str, catalog: &SystemCatalog) {
    // Fully remove the user record from redb. `delete_user` is
    // idempotent — a missing record on a fresh follower is a
    // harmless no-op (redb `remove` on an absent key succeeds).
    if let Err(e) = catalog.delete_user(username) {
        warn!(
            username = %username,
            error = %e,
            "catalog_entry: delete_user failed"
        );
    }
}
