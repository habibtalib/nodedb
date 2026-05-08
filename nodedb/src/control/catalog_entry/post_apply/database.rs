// SPDX-License-Identifier: BUSL-1.1

//! Synchronous post-apply side effects for database catalog entries.
//!
//! Database descriptors and grants are read directly from redb on the hot
//! path (no separate in-memory registry). Post-apply is therefore a no-op
//! today — the redb write in the apply step is sufficient for consistency.
//! This module exists to keep the per-family structure uniform with all
//! other catalog families.

use std::sync::Arc;

use crate::control::security::catalog::database_types::DatabaseDescriptor;
use crate::control::state::SharedState;

/// Post-apply for `PutDatabase` — no in-memory cache to update.
pub fn put(_descriptor: DatabaseDescriptor, _shared: Arc<SharedState>) {}

/// Post-apply for `DeleteDatabase` — no in-memory cache to update.
pub fn delete(_db_id: u64, _shared: Arc<SharedState>) {}

/// Post-apply for `PutDatabaseGrant`.
pub fn put_grant(_db_id: u64, _user_id: u64, _privilege: String, _shared: Arc<SharedState>) {}

/// Post-apply for `DeleteDatabaseGrant`.
pub fn delete_grant(_db_id: u64, _user_id: u64, _privilege: String, _shared: Arc<SharedState>) {}
