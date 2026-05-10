// SPDX-License-Identifier: BUSL-1.1

//! Unit tests for the compaction handler. Integration coverage of the
//! per-database budget gate lives in `nodedb/tests/fts_compaction_budget.rs`
//! and exercises the FTS path end-to-end.

use crate::engine::vector::hnsw::graph::HnswParams;
use nodedb_types::DatabaseId;

#[test]
fn compaction_removes_tombstones() {
    // Test HNSW compaction directly (sealed segment tombstone removal).
    let mut idx = crate::engine::vector::hnsw::graph::HnswIndex::new(4, HnswParams::default());
    for i in 0..20u32 {
        let _ = idx.insert(vec![i as f32; 4]);
    }
    for i in 0..10u32 {
        idx.delete(i);
    }
    assert_eq!(idx.tombstone_count(), 10);
    assert_eq!(idx.live_count(), 10);

    let removed = idx.compact();
    assert_eq!(removed, 10);
    assert_eq!(idx.live_count(), 10);
    assert_eq!(idx.tombstone_count(), 0);
}

#[test]
fn maintenance_respects_interval() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _req_tx, _resp_rx) =
        crate::data::executor::core_loop::tests::make_core_with_dir(dir.path());

    // First call should run.
    assert!(core.maybe_run_maintenance());

    // Immediate second call should skip.
    assert!(!core.maybe_run_maintenance());
}

#[test]
fn forced_compaction_ignores_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _req_tx, _resp_rx) =
        crate::data::executor::core_loop::tests::make_core_with_dir(dir.path());

    // Force compaction with no data — should succeed without error.
    let stats = core.run_compaction(true);
    assert_eq!(stats.vectors_compacted, 0);
    assert!(stats.csr_compacted);
}

/// Regression test for the lease-lifetime bug: the maintenance lease
/// MUST live across the actual compaction work, not be dropped before
/// it. The previous implementation called `try_acquire(...).is_none()`
/// — the lease was constructed and dropped on the same line, so its
/// `Drop` impl recorded ~0 elapsed time and the per-database budget
/// was effectively unbounded. This test pre-saturates the budget by
/// repeatedly running the gated path; if the lease is held correctly,
/// the next non-forced call must return `csr_deferred == true`.
#[test]
fn lease_is_held_across_work() {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::control::maintenance::MaintenanceBudgetTracker;

    let dir = tempfile::tempdir().unwrap();
    let (mut core, _req_tx, _resp_rx) =
        crate::data::executor::core_loop::tests::make_core_with_dir(dir.path());

    // 1% of 60s = 0.6s cap per minute for the DEFAULT db (CSR + sweep
    // budget scope). Saturating this requires <1 wall-clock second.
    let tracker = Arc::new(MaintenanceBudgetTracker::new());
    tracker.set_cap(DatabaseId::DEFAULT, 1);
    core.set_maintenance_budget(Arc::clone(&tracker));

    // Burn the budget by acquiring leases tied to ~1 ms of real work.
    // If the lease is held correctly, each iteration records ~1 ms; we
    // expect deferral after ~600 iterations, so 5000 is a safe upper
    // bound for slow CI machines while still catching the regression
    // (the bug allowed unbounded acquires).
    let mut acquired = 0usize;
    for _ in 0..5000 {
        match tracker.try_acquire(DatabaseId::DEFAULT, 0.0) {
            Some(lease) => {
                std::thread::sleep(Duration::from_millis(1));
                drop(lease);
                acquired += 1;
            }
            None => break,
        }
    }
    assert!(
        acquired < 5000,
        "budget never exhausted after {acquired} acquires — lease drop is not recording elapsed time"
    );

    // Non-forced compaction must now report deferral on the gated phases.
    let stats = core.run_compaction(false);
    assert!(
        stats.csr_deferred,
        "CSR compaction must defer when the DEFAULT db is over budget"
    );
    assert!(
        stats.edges_deferred,
        "edge sweep must defer when the DEFAULT db is over budget"
    );
    assert!(!stats.csr_compacted, "CSR must not have run while deferred");

    // Forced compaction bypasses the budget unconditionally.
    let forced = core.run_compaction(true);
    assert!(forced.csr_compacted, "force=true must bypass the budget");
    assert!(!forced.csr_deferred);
}
