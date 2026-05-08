// SPDX-License-Identifier: BUSL-1.1

//! RAII reservation token for the four-level memory hierarchy.
//!
//! A [`ReservationToken`] is produced by
//! [`MemoryGovernor::try_reserve`](crate::governor::MemoryGovernor::try_reserve)
//! and holds references to all four budget layers:
//! global counter, optional per-database counter, optional per-tenant counter,
//! and the engine identifier for engine-budget release.
//!
//! Dropping the token releases all four layers atomically.
//!
//! # Panic safety
//!
//! `Drop` uses atomic operations only and never panics.
//!
//! # `mem::forget`
//!
//! Calling `mem::forget` on a token prevents release. This is intentional:
//! the token represents live allocations that must not be double-freed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nodedb_types::{DatabaseId, TenantId};

use crate::engine::EngineId;
use crate::governor::GlobalCounter;

/// Holds a memory reservation across the four budget layers.
///
/// Releasing happens in reverse order (engine → tenant → database → global)
/// on drop.
#[must_use = "dropping a ReservationToken immediately releases the reservation; bind it to a variable"]
pub struct ReservationToken {
    /// Shared global-ceiling atomic. Drop decrements this.
    pub(crate) global_counter: Arc<GlobalCounter>,
    /// Per-database allocated counter. `None` if no database budget.
    pub(crate) database_counter: Option<Arc<AtomicUsize>>,
    /// Per-tenant allocated counter. `None` if no tenant budget.
    pub(crate) tenant_counter: Option<Arc<AtomicUsize>>,
    /// Per-engine allocated counter. `None` if no engine budget (unusual —
    /// `try_reserve` always requires a registered engine).
    pub(crate) engine_counter: Option<Arc<AtomicUsize>>,
    /// Bytes reserved at every layer.
    pub(crate) size: usize,
    /// Identity carried for `Debug` and metrics.
    db: DatabaseId,
    tenant: TenantId,
    engine: EngineId,
}

/// Parameters for constructing a [`ReservationToken`].
///
/// Used by [`MemoryGovernor::try_reserve`] to avoid a too-many-arguments
/// constructor.
pub(crate) struct ReservationParams {
    pub global_counter: Arc<GlobalCounter>,
    pub database_counter: Option<Arc<AtomicUsize>>,
    pub tenant_counter: Option<Arc<AtomicUsize>>,
    pub engine_counter: Option<Arc<AtomicUsize>>,
    pub size: usize,
    pub db: DatabaseId,
    pub tenant: TenantId,
    pub engine: EngineId,
}

impl ReservationToken {
    /// Construct a new token. Called only by [`MemoryGovernor::try_reserve`].
    pub(crate) fn new(params: ReservationParams) -> Self {
        Self {
            global_counter: params.global_counter,
            database_counter: params.database_counter,
            tenant_counter: params.tenant_counter,
            engine_counter: params.engine_counter,
            size: params.size,
            db: params.db,
            tenant: params.tenant,
            engine: params.engine,
        }
    }

    /// Number of bytes reserved by this token.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The database this reservation is scoped to.
    pub fn database_id(&self) -> DatabaseId {
        self.db
    }

    /// The tenant this reservation is scoped to.
    pub fn tenant_id(&self) -> TenantId {
        self.tenant
    }

    /// The engine this reservation is scoped to.
    pub fn engine(&self) -> EngineId {
        self.engine
    }
}

impl Drop for ReservationToken {
    fn drop(&mut self) {
        let size = self.size;
        if size == 0 {
            return;
        }

        // Release in reverse order: engine → tenant → database → global.
        if let Some(ref counter) = self.engine_counter {
            counter.fetch_sub(size, Ordering::Relaxed);
        }
        if let Some(ref counter) = self.tenant_counter {
            counter.fetch_sub(size, Ordering::Relaxed);
        }
        if let Some(ref counter) = self.database_counter {
            counter.fetch_sub(size, Ordering::Relaxed);
        }
        self.global_counter
            .allocated
            .fetch_sub(size, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for ReservationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReservationToken")
            .field("size", &self.size)
            .field("db", &self.db)
            .field("tenant", &self.tenant)
            .field("engine", &self.engine)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use nodedb_types::{DatabaseId, TenantId};

    use super::{ReservationParams, ReservationToken};
    use crate::engine::EngineId;
    use crate::governor::GlobalCounter;

    fn make_counter(val: usize) -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(val))
    }

    fn make_global(val: usize) -> Arc<GlobalCounter> {
        Arc::new(GlobalCounter {
            allocated: AtomicUsize::new(val),
            ceiling: 1024 * 1024,
        })
    }

    #[test]
    fn drop_releases_all_four_levels() {
        let global = make_global(100);
        let db_ctr = make_counter(100);
        let tenant_ctr = make_counter(100);
        let engine_ctr = make_counter(100);

        let token = ReservationToken::new(ReservationParams {
            global_counter: Arc::clone(&global),
            database_counter: Some(Arc::clone(&db_ctr)),
            tenant_counter: Some(Arc::clone(&tenant_ctr)),
            engine_counter: Some(Arc::clone(&engine_ctr)),
            size: 100,
            db: DatabaseId::DEFAULT,
            tenant: TenantId::new(1),
            engine: EngineId::Vector,
        });

        assert_eq!(
            global.allocated.load(std::sync::atomic::Ordering::Relaxed),
            100
        );

        drop(token);

        assert_eq!(
            global.allocated.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(db_ctr.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(tenant_ctr.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(engine_ctr.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn drop_with_no_scoped_counters_releases_global() {
        let global = make_global(200);
        let token = ReservationToken::new(ReservationParams {
            global_counter: Arc::clone(&global),
            database_counter: None,
            tenant_counter: None,
            engine_counter: None,
            size: 200,
            db: DatabaseId::DEFAULT,
            tenant: TenantId::new(1),
            engine: EngineId::Query,
        });
        drop(token);
        assert_eq!(
            global.allocated.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn zero_size_drop_is_noop() {
        let global = make_global(0);
        let token = ReservationToken::new(ReservationParams {
            global_counter: Arc::clone(&global),
            database_counter: None,
            tenant_counter: None,
            engine_counter: None,
            size: 0,
            db: DatabaseId::DEFAULT,
            tenant: TenantId::new(1),
            engine: EngineId::Query,
        });
        drop(token);
        assert_eq!(
            global.allocated.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}
