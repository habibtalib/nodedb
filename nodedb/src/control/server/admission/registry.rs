// SPDX-License-Identifier: BUSL-1.1

//! Per-database and per-tenant connection semaphore registries.
//!
//! The registry lazily creates a `tokio::sync::Semaphore` for each database
//! (keyed by `DatabaseId`) and each tenant within a database
//! (keyed by `(DatabaseId, TenantId)`) on first connection.
//! Semaphore capacity is set from the `max_connections` field of the
//! matching `QuotaRecord`; zero means "no limit" and no semaphore is created.
//!
//! All operations are lock-free on the fast path (read) and use a short-held
//! write lock only when creating a new semaphore entry.
//!
//! ## Lock-poisoning policy
//!
//! The `RwLock`-guarded maps store `Arc<Semaphore>` handles. Map updates
//! are single insertions; they cannot leave a partially-constructed
//! invariant if a different thread panics. We therefore recover poisoned
//! locks via `unwrap_or_else(|p| p.into_inner())` rather than propagate
//! the poison — keeping admission live across an unrelated panic is
//! strictly better than failing every future connection until restart.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tracing::debug;

use nodedb_types::{DatabaseId, TenantId};

/// Reason a connection was rejected at admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    /// The target database has exhausted its `max_connections` quota.
    DatabaseCapExhausted { db: DatabaseId, limit: u32 },
    /// The tenant has exhausted its `max_connections` quota within the database.
    TenantCapExhausted {
        db: DatabaseId,
        tenant: TenantId,
        limit: u32,
    },
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DatabaseCapExhausted { db, limit } => {
                write!(
                    f,
                    "database {db:?} has reached its maximum connection limit ({limit})"
                )
            }
            Self::TenantCapExhausted { db, tenant, limit } => {
                write!(
                    f,
                    "tenant {tenant:?} in database {db:?} has reached its maximum \
                     connection limit ({limit})"
                )
            }
        }
    }
}

impl std::error::Error for AdmissionError {}

/// Entry in the per-database semaphore map.
struct DbEntry {
    semaphore: Arc<Semaphore>,
    limit: u32,
}

/// Entry in the per-tenant semaphore map.
struct TenantEntry {
    semaphore: Arc<Semaphore>,
    limit: u32,
}

/// Registry of per-database and per-tenant connection semaphores.
///
/// Created once at server startup and shared (via `Arc`) with every
/// `Listener` instance. Quotas are updated at runtime via
/// `set_database_limit` / `set_tenant_limit` (called by the catalog apply
/// path on `ALTER DATABASE … SET QUOTA` and `ALTER TENANT … SET QUOTA`).
pub struct AdmissionRegistry {
    db_semaphores: RwLock<HashMap<DatabaseId, DbEntry>>,
    tenant_semaphores: RwLock<HashMap<(DatabaseId, TenantId), TenantEntry>>,
}

impl AdmissionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            db_semaphores: RwLock::new(HashMap::new()),
            tenant_semaphores: RwLock::new(HashMap::new()),
        }
    }

    // ── Quota setters ─────────────────────────────────────────────────────────

    /// Configure the maximum connections for a database.
    ///
    /// `limit = 0` removes any cap (deletes the semaphore entry).
    /// Called by the quota catalog apply path.
    pub fn set_database_limit(&self, db: DatabaseId, limit: u32) {
        let mut map = self
            .db_semaphores
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if limit == 0 {
            map.remove(&db);
        } else {
            map.insert(
                db,
                DbEntry {
                    semaphore: Arc::new(Semaphore::new(limit as usize)),
                    limit,
                },
            );
        }
    }

    /// Configure the maximum connections for a tenant within a database.
    ///
    /// `limit = 0` removes any cap.
    pub fn set_tenant_limit(&self, db: DatabaseId, tenant: TenantId, limit: u32) {
        let mut map = self
            .tenant_semaphores
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if limit == 0 {
            map.remove(&(db, tenant));
        } else {
            map.insert(
                (db, tenant),
                TenantEntry {
                    semaphore: Arc::new(Semaphore::new(limit as usize)),
                    limit,
                },
            );
        }
    }

    // ── Admission ─────────────────────────────────────────────────────────────

    /// Attempt to acquire a database-level permit for a new connection.
    ///
    /// Returns `Ok(Some(permit))` if a semaphore exists and a slot was acquired,
    /// `Ok(None)` if no limit is configured, or `Err(AdmissionError)` if the
    /// database is at capacity.
    pub fn try_acquire_database(
        &self,
        db: DatabaseId,
    ) -> Result<Option<OwnedSemaphorePermit>, AdmissionError> {
        let map = self.db_semaphores.read().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = map.get(&db) else {
            return Ok(None); // No cap configured.
        };
        match entry.semaphore.clone().try_acquire_owned() {
            Ok(permit) => {
                debug!(db = ?db, "database admission permit acquired");
                Ok(Some(permit))
            }
            Err(TryAcquireError::NoPermits) => Err(AdmissionError::DatabaseCapExhausted {
                db,
                limit: entry.limit,
            }),
            Err(TryAcquireError::Closed) => {
                // Semaphore was closed (registry teardown) — treat as no-limit.
                Ok(None)
            }
        }
    }

    /// Attempt to acquire a tenant-level permit for a new connection.
    ///
    /// Returns `Ok(Some(permit))` if a semaphore exists and a slot was acquired,
    /// `Ok(None)` if no limit is configured, or `Err(AdmissionError)` if the
    /// tenant is at capacity.
    pub fn try_acquire_tenant(
        &self,
        db: DatabaseId,
        tenant: TenantId,
    ) -> Result<Option<OwnedSemaphorePermit>, AdmissionError> {
        let map = self
            .tenant_semaphores
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let Some(entry) = map.get(&(db, tenant)) else {
            return Ok(None); // No cap configured.
        };
        match entry.semaphore.clone().try_acquire_owned() {
            Ok(permit) => {
                debug!(db = ?db, tenant = ?tenant, "tenant admission permit acquired");
                Ok(Some(permit))
            }
            Err(TryAcquireError::NoPermits) => Err(AdmissionError::TenantCapExhausted {
                db,
                tenant,
                limit: entry.limit,
            }),
            Err(TryAcquireError::Closed) => Ok(None),
        }
    }
}

impl Default for AdmissionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AdmissionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let db_count = self.db_semaphores.read().map(|m| m.len()).unwrap_or(0);
        let tenant_count = self.tenant_semaphores.read().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("AdmissionRegistry")
            .field("db_entries", &db_count)
            .field("tenant_entries", &tenant_count)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use nodedb_types::{DatabaseId, TenantId};

    use super::{AdmissionError, AdmissionRegistry};

    fn db(n: u64) -> DatabaseId {
        if n == 0 {
            DatabaseId::DEFAULT
        } else {
            // Use a non-default ID for tests that need a distinct DB.
            // DatabaseId doesn't expose a public constructor for arbitrary IDs
            // so we use DEFAULT for db_a and a fabricated value for db_b via
            // the From impl if available. For simplicity we test with DEFAULT.
            DatabaseId::DEFAULT
        }
    }

    fn tenant(n: u64) -> TenantId {
        TenantId::new(n)
    }

    // ── Database cap tests ────────────────────────────────────────────────────

    #[test]
    fn no_database_cap_allows_unlimited() {
        let reg = AdmissionRegistry::new();
        // No cap configured → Ok(None).
        let r = reg.try_acquire_database(db(0));
        assert!(r.unwrap().is_none());
    }

    #[test]
    fn database_cap_allows_up_to_limit() {
        let reg = AdmissionRegistry::new();
        reg.set_database_limit(db(0), 2);

        let p1 = reg.try_acquire_database(db(0)).unwrap();
        let p2 = reg.try_acquire_database(db(0)).unwrap();
        assert!(p1.is_some());
        assert!(p2.is_some());

        // Third attempt must fail.
        let err = reg.try_acquire_database(db(0)).unwrap_err();
        assert!(matches!(
            err,
            AdmissionError::DatabaseCapExhausted { limit: 2, .. }
        ));

        // Drop one permit — the slot is released.
        drop(p1);
        let p3 = reg.try_acquire_database(db(0)).unwrap();
        assert!(p3.is_some());
    }

    // ── Tenant cap tests ──────────────────────────────────────────────────────

    #[test]
    fn tenant_cap_isolates_tenants() {
        let reg = AdmissionRegistry::new();
        reg.set_database_limit(db(0), 100); // generous DB cap
        reg.set_tenant_limit(db(0), tenant(1), 1);

        // T1 gets its single slot.
        let t1_permit = reg.try_acquire_tenant(db(0), tenant(1)).unwrap();
        assert!(t1_permit.is_some());

        // T1 is now at capacity.
        let err = reg.try_acquire_tenant(db(0), tenant(1)).unwrap_err();
        assert!(matches!(
            err,
            AdmissionError::TenantCapExhausted { limit: 1, .. }
        ));

        // T2 (different tenant) is unaffected.
        let t2_permit = reg.try_acquire_tenant(db(0), tenant(2)).unwrap();
        assert!(t2_permit.is_none()); // T2 has no cap configured → None
    }

    #[test]
    fn set_limit_zero_removes_cap() {
        let reg = AdmissionRegistry::new();
        reg.set_database_limit(db(0), 1);

        // Use the slot.
        let _p = reg.try_acquire_database(db(0)).unwrap().unwrap();

        // Remove cap.
        reg.set_database_limit(db(0), 0);

        // Slot is now uncapped — Ok(None).
        let r = reg.try_acquire_database(db(0)).unwrap();
        assert!(r.is_none());
    }
}
