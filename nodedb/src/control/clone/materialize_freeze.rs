// SPDX-License-Identifier: BUSL-1.1

//! Source-database freeze registry for clone materialization.
//!
//! While a clone materializer is copying rows from a source database, that
//! source must not accept new user writes.  For MVCC-capable engines
//! (Document, Columnar) the `system_as_of_ms` filter at the scan layer
//! enforces the as-of semantic, but for the KV engine — which has no MVCC —
//! a concurrent write to the source would slip into the target copy.
//!
//! `MaterializeFreezeRegistry` solves this with a reference-counted freeze:
//!
//! * `freeze(db_id)` — increments the refcount for `db_id` and returns an
//!   RAII [`FreezeGuard`] whose [`Drop`] decrements it.
//! * `is_frozen(db_id)` — fast read-only check used by the dispatch gate.
//!
//! Two concurrent materializers sweeping different clones of the same source
//! nest correctly: the first `freeze()` inserts with count 1; the second
//! increments to 2.  The source is unfrozen only when the last guard drops
//! and the count returns to 0.
//!
//! All operations are wait-free under the read path: `is_frozen` holds the
//! read-lock only long enough to look up one entry.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use nodedb_types::id::DatabaseId;

/// Database-level freeze registry.
///
/// All methods are `Send + Sync` and safe for concurrent use from multiple
/// Tokio tasks.
pub struct MaterializeFreezeRegistry {
    /// Maps `database_id → active freeze count`.
    inner: RwLock<HashMap<DatabaseId, u32>>,
}

impl MaterializeFreezeRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(HashMap::new()),
        })
    }

    /// Freeze `db_id` for the duration of the returned guard.
    ///
    /// Multiple concurrent calls with the same `db_id` are allowed; the
    /// database stays frozen until every guard is dropped.
    pub fn freeze(self: &Arc<Self>, db_id: DatabaseId) -> FreezeGuard {
        {
            let mut map = self.inner.write().unwrap_or_else(|p| p.into_inner());
            let count = map.entry(db_id).or_insert(0);
            *count += 1;
        }
        FreezeGuard {
            registry: Arc::clone(self),
            db_id,
        }
    }

    /// Returns `true` if `db_id` currently has at least one active freeze.
    ///
    /// Called on the hot write path — read lock is held only for the duration
    /// of the `contains_key` check.
    pub fn is_frozen(&self, db_id: DatabaseId) -> bool {
        let map = self.inner.read().unwrap_or_else(|p| p.into_inner());
        map.get(&db_id).copied().unwrap_or(0) > 0
    }

    /// Decrement the refcount for `db_id`, removing the entry when it hits 0.
    ///
    /// Called exclusively by [`FreezeGuard::drop`].
    fn release(&self, db_id: DatabaseId) {
        let mut map = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(count) = map.get_mut(&db_id) {
            if *count <= 1 {
                map.remove(&db_id);
            } else {
                *count -= 1;
            }
        }
    }
}

impl Default for MaterializeFreezeRegistry {
    fn default() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }
}

/// RAII guard that holds a freeze for one database.
///
/// Dropping this guard releases the freeze (decrements the refcount). A panic
/// between `freeze()` and the natural drop point is safe: Rust's stack
/// unwinding calls `Drop`, so the registry is always cleaned up.
pub struct FreezeGuard {
    registry: Arc<MaterializeFreezeRegistry>,
    db_id: DatabaseId,
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        self.registry.release(self.db_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(id: u64) -> DatabaseId {
        DatabaseId::new(id)
    }

    #[test]
    fn freeze_and_release_single() {
        let reg = MaterializeFreezeRegistry::new();
        assert!(!reg.is_frozen(db(1)));
        let guard = reg.freeze(db(1));
        assert!(reg.is_frozen(db(1)));
        drop(guard);
        assert!(!reg.is_frozen(db(1)));
    }

    #[test]
    fn nested_freeze_releases_on_last_drop() {
        let reg = MaterializeFreezeRegistry::new();
        let g1 = reg.freeze(db(2));
        let g2 = reg.freeze(db(2));
        assert!(reg.is_frozen(db(2)));
        drop(g1);
        assert!(reg.is_frozen(db(2)), "still frozen after first drop");
        drop(g2);
        assert!(!reg.is_frozen(db(2)), "unfrozen after last drop");
    }

    #[test]
    fn different_databases_independent() {
        let reg = MaterializeFreezeRegistry::new();
        let _g1 = reg.freeze(db(3));
        assert!(reg.is_frozen(db(3)));
        assert!(!reg.is_frozen(db(4)));
    }
}
