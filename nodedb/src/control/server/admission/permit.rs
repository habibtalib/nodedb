// SPDX-License-Identifier: BUSL-1.1

//! Three-level RAII connection permit.
//!
//! A [`ConnectionPermit`] holds:
//! 1. A global permit (from the cluster-wide `max_connections` semaphore).
//! 2. An optional per-database permit (from a per-`DatabaseId` semaphore).
//! 3. An optional per-tenant permit (from a per-`(DatabaseId, TenantId)` semaphore).
//!
//! All three levels are released atomically when the permit is dropped.
//! The permit is `Send` — it is held inside a Tokio task for the connection
//! lifetime and dropped when the task exits.

use tokio::sync::OwnedSemaphorePermit;

use nodedb_types::{DatabaseId, TenantId};

/// A three-level RAII connection permit.
///
/// Holds the connection's slot at the global, database, and (optionally)
/// tenant level. Dropping this struct releases all three slots simultaneously.
#[must_use = "ConnectionPermit must be kept alive for the connection's lifetime"]
pub struct ConnectionPermit {
    /// Global connection slot (always held). Held purely for RAII — its
    /// `Drop` releases the cluster-wide `max_connections` semaphore permit.
    /// Never read after construction, but the field is load-bearing:
    /// removing it would release the global slot at end-of-auth instead of
    /// at connection close. The `dead_code` allow is therefore intentional
    /// and documented, not a lint suppression.
    #[allow(dead_code)]
    pub(crate) global: OwnedSemaphorePermit,
    /// Per-database connection slot. `None` if the database has no
    /// `max_connections` quota configured.
    pub(crate) database: Option<OwnedSemaphorePermit>,
    /// Per-tenant connection slot. `None` if the tenant has no
    /// `max_connections` quota configured.
    pub(crate) tenant: Option<OwnedSemaphorePermit>,
    /// The database this permit is scoped to (for metrics / tracing).
    pub db_id: DatabaseId,
    /// The tenant this permit is scoped to (for metrics / tracing).
    pub tenant_id: TenantId,
}

impl std::fmt::Debug for ConnectionPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionPermit")
            .field("db_id", &self.db_id)
            .field("tenant_id", &self.tenant_id)
            .field("global_permit_held", &true)
            .field("has_database_permit", &self.database.is_some())
            .field("has_tenant_permit", &self.tenant.is_some())
            .finish()
    }
}
