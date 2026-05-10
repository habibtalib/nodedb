// SPDX-License-Identifier: BUSL-1.1

//! `DatabaseRegistry` — thread-safe monotonic database-id allocator.
//!
//! ## Counter semantics
//!
//! The internal `AtomicU64` counter stores the **next** database id to
//! be handed out. `alloc_one()` does `fetch_add(1, AcqRel)`; the returned
//! value is the previous counter (i.e. the id the caller now owns). After
//! every successful allocation, `current_hwm()` returns the highest id
//! ever issued — equivalently, `counter - 1`.
//!
//! ## Reserved range
//!
//! `DatabaseId(0)` is permanently reserved for the built-in `default`
//! database. `DatabaseId(1..=1023)` is reserved for future system
//! databases; none are assigned in v1. User-created databases start
//! at `DatabaseId(1024)`. `from_persisted_hwm` enforces this floor:
//! if the persisted hwm is less than 1023, the counter is initialized
//! to 1024 so the first allocation cannot invade the reserved range.
//!
//! ## Restart semantics
//!
//! `from_persisted_hwm(hwm)` initializes `counter = max(hwm + 1, 1024)`.
//!
//! ## Raft routing
//!
//! The atomic counter is a local cache only. The authoritative
//! allocation goes through Raft metadata group 0 via
//! `crate::control::metadata_proposer::propose_database_hwm`. The
//! `install_shared` hook (mirroring `SurrogateAssigner`) wires the
//! weak `SharedState` handle so the flush path can propose.
//!
//! ## Width
//!
//! Database IDs are `u64`. With user databases starting at 1024 and
//! u64::MAX ≈ 1.8 × 10^19, overflow is not a practical concern; the
//! registry does not implement an `Exhausted` error path.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nodedb_types::DatabaseId;

use super::persist::DatabaseHwmPersist;

/// Periodic flush trigger: every N allocations, regardless of elapsed time.
pub const FLUSH_OPS_THRESHOLD: u64 = 64;

/// Periodic flush trigger: every T elapsed since the last flush.
pub const FLUSH_ELAPSED_THRESHOLD: Duration = Duration::from_millis(200);

/// First user-assignable database id. `0..=1023` reserved.
pub const USER_DB_START: u64 = 1024;

/// Allocation errors. Surfaced to the caller; `From` impl wires this into
/// the crate's central `Error` enum.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseAllocError {
    #[error("database hwm flush failed: {detail}")]
    FlushFailed { detail: String },
}

/// Thread-safe database-id allocator.
///
/// The `Mutex<Instant>` for `last_flush_at` is uncontended on the hot
/// path (`alloc_one` only touches atomics); only `should_flush` and
/// `flush` take the lock, which run at most once per ~200 ms or per 64
/// allocations.
pub struct DatabaseRegistry {
    /// Next id to hand out. Always >= `USER_DB_START`.
    counter: AtomicU64,
    /// Allocations since the last flush. Reset by `flush()`.
    allocs_since_flush: AtomicU64,
    /// Wall-clock anchor for the elapsed-time flush trigger.
    last_flush_at: Mutex<Instant>,
}

impl DatabaseRegistry {
    /// Create an empty registry — first allocation returns `DatabaseId(1024)`.
    pub fn new() -> Self {
        Self::from_persisted_hwm(0)
    }

    /// Restore from a persisted high-watermark. Next allocation returns
    /// `max(hwm + 1, USER_DB_START)`.
    pub fn from_persisted_hwm(hwm: u64) -> Self {
        let next = (hwm + 1).max(USER_DB_START);
        Self {
            counter: AtomicU64::new(next),
            allocs_since_flush: AtomicU64::new(0),
            last_flush_at: Mutex::new(Instant::now()),
        }
    }

    /// Allocate a single database id.
    pub fn alloc_one(&self) -> DatabaseId {
        let prev = self.counter.fetch_add(1, Ordering::AcqRel);
        self.allocs_since_flush.fetch_add(1, Ordering::AcqRel);
        DatabaseId::new(prev)
    }

    /// Highest database id ever issued — `USER_DB_START - 1` if no user
    /// allocations yet (meaning no user database has been created).
    pub fn current_hwm(&self) -> u64 {
        let next = self.counter.load(Ordering::Acquire);
        next.saturating_sub(1)
    }

    /// Idempotently raise the high-watermark to at least `new_hwm`.
    /// Used by WAL replay or Raft follower catch-up. Never lowers.
    pub fn restore_hwm(&self, new_hwm: u64) {
        let target = new_hwm + 1;
        let mut current = self.counter.load(Ordering::Acquire);
        loop {
            if target <= current {
                return;
            }
            match self.counter.compare_exchange_weak(
                current,
                target,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// True if the periodic-flush thresholds (ops or elapsed) are tripped.
    pub fn should_flush(&self) -> bool {
        if self.allocs_since_flush.load(Ordering::Acquire) >= FLUSH_OPS_THRESHOLD {
            return true;
        }
        if let Ok(last) = self.last_flush_at.lock() {
            return last.elapsed() >= FLUSH_ELAPSED_THRESHOLD;
        }
        false
    }

    /// Persist the current high-watermark and reset flush counters.
    /// Idempotent: calling on an unmodified registry just rewrites the
    /// same hwm.
    pub fn flush(&self, persist: &dyn DatabaseHwmPersist) -> Result<(), DatabaseAllocError> {
        let hwm = self.current_hwm();
        persist
            .checkpoint(hwm)
            .map_err(|e| DatabaseAllocError::FlushFailed {
                detail: e.to_string(),
            })?;
        self.allocs_since_flush.store(0, Ordering::Release);
        if let Ok(mut guard) = self.last_flush_at.lock() {
            *guard = Instant::now();
        }
        Ok(())
    }

    /// Test-only: force the elapsed-flush trigger by rewinding the
    /// wall-clock anchor.
    #[cfg(test)]
    fn rewind_flush_clock(&self, by: Duration) {
        if let Ok(mut guard) = self.last_flush_at.lock()
            && let Some(earlier) = guard.checked_sub(by)
        {
            *guard = earlier;
        }
    }
}

impl Default for DatabaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl From<DatabaseAllocError> for crate::Error {
    fn from(e: DatabaseAllocError) -> Self {
        match e {
            DatabaseAllocError::FlushFailed { detail } => crate::Error::Storage {
                engine: "database_registry".into(),
                detail,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    use super::*;

    struct MemPersist {
        last: std::sync::Mutex<Option<u64>>,
        calls: AtomicU32,
    }

    impl MemPersist {
        fn new() -> Self {
            Self {
                last: std::sync::Mutex::new(None),
                calls: AtomicU32::new(0),
            }
        }

        fn last(&self) -> Option<u64> {
            *self.last.lock().unwrap()
        }

        fn calls(&self) -> u32 {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl DatabaseHwmPersist for MemPersist {
        fn checkpoint(&self, hwm: u64) -> crate::Result<()> {
            *self.last.lock().unwrap() = Some(hwm);
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn load(&self) -> crate::Result<u64> {
            Ok(self.last().unwrap_or(0))
        }
    }

    #[test]
    fn first_alloc_returns_user_db_start() {
        let reg = DatabaseRegistry::new();
        let d = reg.alloc_one();
        assert_eq!(d.as_u64(), USER_DB_START);
    }

    #[test]
    fn monotonic_100() {
        let reg = DatabaseRegistry::new();
        let mut prev = 0u64;
        for _ in 0..100 {
            let d = reg.alloc_one();
            assert!(d.as_u64() > prev);
            prev = d.as_u64();
        }
    }

    #[test]
    fn restart_respects_hwm() {
        let reg = DatabaseRegistry::from_persisted_hwm(5000);
        let d = reg.alloc_one();
        assert_eq!(d.as_u64(), 5001);
        assert_eq!(reg.current_hwm(), 5001);
    }

    #[test]
    fn restart_below_user_start_floored() {
        // hwm=0 → counter starts at USER_DB_START
        let reg = DatabaseRegistry::from_persisted_hwm(0);
        let d = reg.alloc_one();
        assert_eq!(d.as_u64(), USER_DB_START);
        // hwm=500 → still floored to USER_DB_START
        let reg2 = DatabaseRegistry::from_persisted_hwm(500);
        let d2 = reg2.alloc_one();
        assert_eq!(d2.as_u64(), USER_DB_START);
    }

    #[test]
    fn restore_hwm_monotonic() {
        let reg = DatabaseRegistry::new();
        reg.restore_hwm(9000);
        let d = reg.alloc_one();
        assert_eq!(d.as_u64(), 9001);
        // Lowering is a no-op
        reg.restore_hwm(100);
        let d2 = reg.alloc_one();
        assert_eq!(d2.as_u64(), 9002);
    }

    #[test]
    fn concurrent_32x50_unique() {
        let reg = Arc::new(DatabaseRegistry::new());
        let mut handles = Vec::with_capacity(32);
        for _ in 0..32 {
            let r = reg.clone();
            handles.push(std::thread::spawn(move || {
                (0..50).map(|_| r.alloc_one().as_u64()).collect::<Vec<_>>()
            }));
        }
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 1600);
    }

    #[test]
    fn flush_ops_threshold() {
        let reg = DatabaseRegistry::new();
        for _ in 0..(FLUSH_OPS_THRESHOLD - 1) {
            reg.alloc_one();
        }
        assert!(!reg.should_flush());
        reg.alloc_one();
        assert!(reg.should_flush());

        let persist = MemPersist::new();
        reg.flush(&persist).unwrap();
        assert_eq!(persist.calls(), 1);
        assert!(!reg.should_flush());
    }

    #[test]
    fn flush_elapsed_threshold() {
        let reg = DatabaseRegistry::new();
        reg.alloc_one();
        assert!(!reg.should_flush());
        reg.rewind_flush_clock(FLUSH_ELAPSED_THRESHOLD * 2);
        assert!(reg.should_flush());
        let persist = MemPersist::new();
        reg.flush(&persist).unwrap();
        assert!(!reg.should_flush());
    }

    #[test]
    fn flush_idempotent() {
        let reg = DatabaseRegistry::new();
        let persist = MemPersist::new();
        reg.flush(&persist).unwrap();
        reg.flush(&persist).unwrap();
        assert_eq!(persist.calls(), 2);
    }
}
