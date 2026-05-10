// SPDX-License-Identifier: BUSL-1.1

//! Per-virtual-queue depth and backpressure counters.
//!
//! These counters are incremented by the dispatch layer as databases cross the
//! 85% (throttle) and 95% (suspend) thresholds of their fair-share WFQ slot
//! allocation. The `/metrics` endpoint exposure is wired in a later pass; the
//! counters themselves are authoritative from this point.
//!
//! # Concurrency
//!
//! Writers are Data Plane cores recording threshold transitions on the
//! dispatch hot path; readers are the Tokio metrics exporter. To keep the
//! hot path lock-free after the first observation of a given `database_id`,
//! the map is wrapped in an `RwLock<HashMap<u64, Arc<DbCounters>>>`:
//!
//! - **Steady state**: a read lock is acquired, the `Arc<DbCounters>` is
//!   cloned out, and the lock is dropped before the atomic `fetch_add`. No
//!   writer ever blocks a reader once the entry exists.
//! - **First-time entry**: the read lock is dropped, a write lock is taken,
//!   the entry is double-checked, then inserted. This happens at most once
//!   per `database_id` over the lifetime of the process.
//!
//! The counters themselves are `AtomicU64` so the increment after
//! `Arc::clone` is genuinely lock-free.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Per-database throttle/suspend event counters for one Data Plane core.
pub struct VirtualQueueMetrics {
    /// Keyed by database_id. Lazily created.
    inner: RwLock<HashMap<u64, Arc<DbCounters>>>,
}

struct DbCounters {
    /// Number of times this DB's virtual queue crossed the 85% throttle threshold.
    throttle_events: AtomicU64,
    /// Number of times this DB's virtual queue crossed the 95% suspend threshold.
    suspend_events: AtomicU64,
}

impl DbCounters {
    fn new() -> Self {
        Self {
            throttle_events: AtomicU64::new(0),
            suspend_events: AtomicU64::new(0),
        }
    }
}

impl VirtualQueueMetrics {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Record a throttle event (virtual queue crossed 85% of fair share).
    pub fn record_throttle(&self, database_id: u64) {
        self.counters(database_id)
            .throttle_events
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a suspend event (virtual queue crossed 95% of fair share).
    pub fn record_suspend(&self, database_id: u64) {
        self.counters(database_id)
            .suspend_events
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Total throttle events recorded for a database.
    pub fn throttle_events(&self, database_id: u64) -> u64 {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard
            .get(&database_id)
            .map(|c| c.throttle_events.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Total suspend events recorded for a database.
    pub fn suspend_events(&self, database_id: u64) -> u64 {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard
            .get(&database_id)
            .map(|c| c.suspend_events.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Return the `Arc<DbCounters>` for `database_id`, creating it on first
    /// observation. Steady-state path takes only a read lock.
    fn counters(&self, database_id: u64) -> Arc<DbCounters> {
        // Fast path: entry exists, only a read lock is needed.
        {
            let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
            if let Some(c) = guard.get(&database_id) {
                return Arc::clone(c);
            }
        }
        // Slow path: first-ever observation for this database. Acquire a
        // write lock, double-check (another writer may have raced us), then
        // insert. After this completes, all subsequent calls take the fast
        // path forever.
        let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let entry = guard
            .entry(database_id)
            .or_insert_with(|| Arc::new(DbCounters::new()));
        Arc::clone(entry)
    }
}

impl Default for VirtualQueueMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn record_then_read() {
        let m = VirtualQueueMetrics::new();
        m.record_throttle(7);
        m.record_throttle(7);
        m.record_suspend(7);
        assert_eq!(m.throttle_events(7), 2);
        assert_eq!(m.suspend_events(7), 1);
        assert_eq!(m.throttle_events(99), 0);
    }

    #[test]
    fn concurrent_writers_share_atomic_counter() {
        let m = Arc::new(VirtualQueueMetrics::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = Arc::clone(&m);
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    m.record_throttle(42);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.throttle_events(42), 8 * 1000);
    }

    #[test]
    fn concurrent_first_observation_does_not_lose_counts() {
        // All 8 threads race to be the first observer of the same DB ID.
        let m = Arc::new(VirtualQueueMetrics::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = Arc::clone(&m);
            handles.push(thread::spawn(move || {
                m.record_suspend(123);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.suspend_events(123), 8);
    }
}
