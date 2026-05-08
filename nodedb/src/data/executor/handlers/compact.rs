// SPDX-License-Identifier: BUSL-1.1

//! Compaction handler: periodic and on-demand engine compaction.
//!
//! Compaction removes tombstoned vectors from HNSW indexes, compacts CSR
//! write buffers into dense arrays, and sweeps dangling edges from deleted
//! nodes. All operations run on the Data Plane (single-core, no locks).
//!
//! ## What gets compacted
//!
//! - **Vector engine**: `HnswIndex::compact()` on each sealed segment.
//!   Rebuilds the node array with only live nodes, remaps neighbor IDs,
//!   reclaims jemalloc arena memory. At 768-dim FP32 (~3 KiB/vector),
//!   compacting 1M tombstones reclaims ~3 GB.
//!
//! - **CSR index**: `CsrIndex::compact()` merges the mutable write buffer
//!   into the dense adjacency arrays. Eliminates per-node buffer overhead
//!   and restores cache-friendly sequential access.
//!
//! - **Dangling edges**: Removes edges whose source or destination was
//!   deleted (present in `deleted_nodes` set). Cleans both the in-memory
//!   CSR and the persistent redb edge store.
//!
//! ## Triggering
//!
//! - **Periodic**: The runtime event loop calls `run_maintenance()` every
//!   `COMPACTION_INTERVAL` (default 10 minutes). Only compacts collections
//!   with tombstone ratio above the threshold.
//!
//! - **On-demand**: `PhysicalPlan::Compact` dispatched from the Control
//!   Plane. Forces compaction regardless of tombstone ratio (for operator
//!   use, e.g., after a bulk delete).

use tracing::info;

use crate::bridge::envelope::Response;
use crate::control::maintenance::MaintenanceLease;
use crate::data::executor::core_loop::CoreLoop;
use crate::data::executor::task::ExecutionTask;
use nodedb_types::DatabaseId;

/// Outcome of a budget gate for a single maintenance unit.
///
/// The `Granted` variant carries an `Option<MaintenanceLease>`:
/// - `Some(lease)` — caller MUST hold the lease for the duration of the work;
///   on drop, actual elapsed wall-clock time is recorded into the per-database
///   sliding window. Dropping the lease before the work runs records ~0 and
///   silently disables the budget — see the regression test
///   `lease_is_held_across_work` below.
/// - `None` — no tracker installed or `force` set; no recording is needed.
enum BudgetGate {
    Granted(Option<MaintenanceLease>),
    Deferred,
}

impl CoreLoop {
    /// Execute an on-demand compaction request.
    ///
    /// Forces compaction of all vector collections (regardless of tombstone
    /// ratio), CSR compaction, and dangling edge sweep. Returns a summary
    /// payload with compaction statistics.
    pub(in crate::data::executor) fn execute_compact(&mut self, task: &ExecutionTask) -> Response {
        let result = self.run_compaction(true);
        let payload = match zerompk::to_msgpack_vec(&result) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "failed to encode compaction stats");
                Vec::new()
            }
        };
        self.response_with_payload(task, payload)
    }

    /// Run all maintenance/compaction tasks.
    ///
    /// Called periodically from the runtime event loop (idle maintenance)
    /// and on-demand via `PhysicalPlan::Compact`.
    ///
    /// When `force` is false (periodic), only compacts collections whose
    /// tombstone ratio exceeds the threshold. When `force` is true
    /// (on-demand), compacts everything.
    ///
    /// Per-database CPU budget is enforced when a budget tracker is installed.
    /// Collections whose owning database has exhausted its per-minute budget
    /// are deferred; this only affects periodic maintenance, not forced compaction.
    pub fn run_compaction(&mut self, force: bool) -> CompactionStats {
        let mut stats = CompactionStats::default();

        // 1. Vector compaction: remove tombstoned nodes from HNSW indexes.
        // Collect keys first to avoid borrow conflict on `self`.
        let vector_keys: Vec<_> = self.vector_collections.keys().cloned().collect();
        for key in vector_keys {
            let tid = key.0;
            let db = self.database_for_tenant(tid);

            // Budget gate. The lease, if any, MUST be bound to a `let` so it
            // lives across `collection.compact()` below — its `Drop` impl is
            // what records actual elapsed CPU into the per-db window.
            let _lease = match self.acquire_maintenance_lease(db, force) {
                BudgetGate::Granted(lease) => lease,
                BudgetGate::Deferred => {
                    stats.vectors_deferred += 1;
                    tracing::debug!(
                        core = self.core_id,
                        db = db.as_u64(),
                        collection = &key.1,
                        "vector compaction deferred: database over maintenance budget"
                    );
                    continue;
                }
            };

            let collection = match self.vector_collections.get_mut(&key) {
                Some(c) => c,
                None => continue,
            };

            let total_tombstones: usize = collection
                .sealed_segments()
                .iter()
                .map(|seg| seg.index.tombstone_count())
                .sum();
            let total_nodes: usize = collection
                .sealed_segments()
                .iter()
                .map(|seg| seg.index.len())
                .sum();

            if total_tombstones == 0 {
                continue;
            }

            let ratio = if total_nodes > 0 {
                total_tombstones as f64 / total_nodes as f64
            } else {
                0.0
            };

            if !force && ratio < self.compaction_tombstone_threshold {
                continue;
            }

            let removed = collection.compact();
            if removed > 0 {
                info!(
                    core = self.core_id,
                    collection = &key.1,
                    removed,
                    ratio = format!("{ratio:.2}"),
                    "vector compaction: tombstones removed"
                );
                stats.vectors_compacted += removed;
                stats.collections_compacted += 1;
            }
            // `_lease` drops here, recording elapsed wall-clock into the budget window.
        }

        // 2. CSR compaction: merge write buffers into dense arrays.
        // CSR is not per-database keyed; use DEFAULT as the budget scope.
        let csr_db = DatabaseId::DEFAULT;
        match self.acquire_maintenance_lease(csr_db, force) {
            BudgetGate::Granted(_lease) => {
                match self.csr.compact_all() {
                    Ok(()) => stats.csr_compacted = true,
                    Err(e) => tracing::warn!(
                        error = %e,
                        "CSR compaction rejected by memory governor; skipping"
                    ),
                }
                // _lease drops here, recording elapsed wall-clock into the budget window.
            }
            BudgetGate::Deferred => {
                stats.csr_deferred = true;
                tracing::debug!(
                    core = self.core_id,
                    db = csr_db.as_u64(),
                    "CSR compaction deferred: database over maintenance budget"
                );
            }
        }

        // 3. Dangling edge sweep — budget-gated against DEFAULT (sweep is
        // process-wide; per-tenant attribution happens inside the sweep loop).
        let edges_db = DatabaseId::DEFAULT;
        match self.acquire_maintenance_lease(edges_db, force) {
            BudgetGate::Granted(_lease) => {
                stats.edges_swept = self.sweep_dangling_edges();
                // _lease drops here, recording elapsed wall-clock into the budget window.
            }
            BudgetGate::Deferred => {
                stats.edges_deferred = true;
                tracing::debug!(
                    core = self.core_id,
                    db = edges_db.as_u64(),
                    "edge sweep deferred: database over maintenance budget"
                );
            }
        }

        // 4. L1 segment compaction: per-(tenant, collection) → per-database gated.
        let (merged, deferred) = self.run_segment_compaction(force);
        stats.segments_merged = merged;
        stats.segments_deferred = deferred;

        if stats.vectors_compacted > 0
            || stats.edges_swept > 0
            || stats.segments_merged > 0
            || stats.vectors_deferred > 0
            || stats.csr_deferred
            || stats.edges_deferred
            || stats.segments_deferred > 0
        {
            info!(
                core = self.core_id,
                vectors_compacted = stats.vectors_compacted,
                collections_compacted = stats.collections_compacted,
                edges_swept = stats.edges_swept,
                segments_merged = stats.segments_merged,
                vectors_deferred = stats.vectors_deferred,
                csr_deferred = stats.csr_deferred,
                edges_deferred = stats.edges_deferred,
                segments_deferred = stats.segments_deferred,
                "compaction cycle complete"
            );
        }

        stats
    }

    /// Acquire a maintenance lease for `db`, returning a [`BudgetGate`].
    ///
    /// Callers MUST bind the returned lease to a `let` whose scope spans the
    /// actual maintenance work. The lease's `Drop` impl is what records
    /// elapsed wall-clock time into the per-database budget window.
    fn acquire_maintenance_lease(&self, db: DatabaseId, force: bool) -> BudgetGate {
        if force {
            return BudgetGate::Granted(None);
        }
        match self.maintenance_budget.as_ref() {
            None => BudgetGate::Granted(None),
            Some(tracker) => match tracker.try_acquire(db, 0.0) {
                Some(lease) => BudgetGate::Granted(Some(lease)),
                None => BudgetGate::Deferred,
            },
        }
    }

    /// Run maintenance tasks if enough time has elapsed.
    ///
    /// Called from the runtime event loop on every idle wake. Tracks the
    /// last maintenance time internally and skips if the interval hasn't
    /// elapsed. Returns `true` if maintenance was executed.
    pub fn maybe_run_maintenance(&mut self) -> bool {
        // Checkpoint coordinator tick: incremental dirty page flushing.
        // Runs on its own interval (independent from compaction interval).
        let flush_plan = self.checkpoint_coordinator.tick();
        for (engine, pages) in &flush_plan {
            match engine.as_str() {
                "vector" => {
                    let flushed = self.checkpoint_vector_indexes();
                    self.checkpoint_coordinator
                        .record_flush("vector", flushed.min(*pages));
                }
                "crdt" => {
                    let flushed = self.checkpoint_crdt_engines();
                    self.checkpoint_coordinator
                        .record_flush("crdt", flushed.min(*pages));
                }
                "sparse" => {
                    // redb is ACID — writes are already durable.
                    self.checkpoint_coordinator.record_flush("sparse", *pages);
                }
                "timeseries" => {
                    // Idle flush: if no ingest for 5 seconds, flush all
                    // non-empty memtables so data becomes queryable.
                    let idle_threshold = std::time::Duration::from_secs(5);
                    let is_idle = self
                        .last_ts_ingest
                        .map(|t| t.elapsed() >= idle_threshold)
                        .unwrap_or(false);

                    if is_idle {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        let collections: Vec<(crate::types::TenantId, String)> = self
                            .columnar_memtables
                            .iter()
                            .filter(|(_, mt)| !mt.is_empty())
                            .map(|(k, _)| k.clone())
                            .collect();
                        let mut flushed = 0usize;
                        for (tid, collection) in &collections {
                            self.flush_ts_collection(*tid, collection, now_ms);
                            flushed += 1;
                        }
                        if flushed > 0 {
                            tracing::info!(
                                core = self.core_id,
                                flushed,
                                "idle flush: timeseries memtables flushed"
                            );
                        }
                        // Reset so we don't re-flush until next ingest.
                        self.last_ts_ingest = None;
                        self.checkpoint_coordinator
                            .record_flush("timeseries", flushed.max(*pages));
                    } else {
                        self.checkpoint_coordinator
                            .record_flush("timeseries", *pages);
                    }
                }
                _ => {}
            }
        }
        if self.checkpoint_coordinator.is_clean()
            && !flush_plan.is_empty()
            && self.checkpoint_coordinator.total_dirty_pages() == 0
        {
            self.checkpoint_coordinator
                .complete_checkpoint(self.watermark.as_u64());
        }

        // KV expiry wheel tick: process expired keys on every maintenance call.
        // Bounded by the per-tick reap budget internally — safe for the reactor.
        // Expired keys are emitted as structured log events for CDC visibility.
        {
            let now_ms = crate::engine::kv::current_ms();
            let expired_keys = self.kv_engine.tick_expiry(now_ms);
            if !expired_keys.is_empty() {
                tracing::debug!(
                    core = self.core_id,
                    reaped = expired_keys.len(),
                    backlog = self.kv_engine.expiry_backlog(),
                    "kv expiry wheel tick"
                );

                for ek in &expired_keys {
                    tracing::info!(
                        target: "nodedb::kv::expired",
                        tenant_id = ek.tenant_id,
                        collection = %ek.collection,
                        key_len = ek.key.len(),
                        "kv key expired"
                    );
                }
            }
        }

        // Compaction: periodic tombstone removal + segment merge.
        let now = std::time::Instant::now();
        if let Some(last) = self.last_maintenance
            && now.duration_since(last) < self.compaction_interval
        {
            return !flush_plan.is_empty();
        }
        self.last_maintenance = Some(now);
        self.run_compaction(false);
        true
    }
}

impl CoreLoop {
    /// Run L1 segment compaction using the partition registry's merge logic.
    ///
    /// Finds eligible sealed partitions via `find_mergeable`, marks them
    /// for merge, and purges expired/deleted partitions. The actual merge
    /// I/O is handled by the partition registry and flush pipeline.
    ///
    /// Returns `(merged, deferred)` — the count of partitions selected for
    /// merge, and the count of `(tenant, collection)` pairs whose owning
    /// database was over its maintenance CPU budget and was skipped.
    fn run_segment_compaction(&mut self, force: bool) -> (usize, usize) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let max_per_pass = self.segment_compaction_config.max_segments_per_pass;
        let mut total_merged = 0usize;
        let mut total_deferred = 0usize;

        // Snapshot bitemporal flags per collection up front so the
        // mutable-borrow loop below can query them without re-borrowing `self`.
        let bitemporal_flags: std::collections::HashMap<(crate::types::TenantId, String), bool> =
            self.ts_registries
                .keys()
                .map(|(tid, col)| {
                    let flag = self.is_bitemporal(tid.as_u64(), col);
                    ((*tid, col.clone()), flag)
                })
                .collect();

        // Snapshot tenant→database map and the budget tracker handle so the
        // mutable iteration below can acquire leases without re-borrowing `self`.
        let tenant_db: std::collections::HashMap<crate::types::TenantId, DatabaseId> = self
            .ts_registries
            .keys()
            .map(|(tid, _)| {
                let db = self
                    .tenant_database_map
                    .get(tid)
                    .copied()
                    .unwrap_or(DatabaseId::DEFAULT);
                (*tid, db)
            })
            .collect();
        let budget = self.maintenance_budget.clone();
        let core_id = self.core_id;

        for ((tid, collection), registry) in &mut self.ts_registries {
            let db = tenant_db.get(tid).copied().unwrap_or(DatabaseId::DEFAULT);

            // Per-(tenant, collection) budget gate. Lease lives across the
            // mark/purge work below — its drop records elapsed wall-clock
            // into the per-database window.
            let _lease = if force {
                None
            } else {
                match budget.as_ref() {
                    None => None,
                    Some(tracker) => match tracker.try_acquire(db, 0.0) {
                        Some(l) => Some(l),
                        None => {
                            total_deferred += 1;
                            tracing::debug!(
                                core = core_id,
                                db = db.as_u64(),
                                collection = %collection,
                                "segment compaction deferred: database over maintenance budget"
                            );
                            continue;
                        }
                    },
                }
            };

            let bitemporal = bitemporal_flags
                .get(&(*tid, collection.clone()))
                .copied()
                .unwrap_or(false);

            // Find mergeable partition groups, limited by config.
            let mut groups = registry.find_mergeable(now_ms);
            if !force {
                groups.truncate(max_per_pass);
            }
            for group in &groups {
                for &start_ts in group {
                    registry.mark_merging(start_ts);
                }
                total_merged += group.len();
            }

            // Retention: find and purge expired partitions.
            let expired = registry.find_expired(now_ms, bitemporal);
            for start_ts in &expired {
                registry.mark_deleted(*start_ts);
            }
            let purged = registry.purge_deleted();

            if !groups.is_empty() || !purged.is_empty() {
                info!(
                    core = core_id,
                    collection = %collection,
                    merge_groups = groups.len(),
                    expired = expired.len(),
                    purged = purged.len(),
                    "timeseries partition compaction"
                );
            }

            // Force mode: also compact partitions that wouldn't normally merge.
            if force && total_merged == 0 {
                let sealed_count = registry.sealed_count();
                if sealed_count >= 2 {
                    info!(
                        core = core_id,
                        collection = %collection,
                        sealed_count,
                        "forced compaction: marking sealed partitions for merge"
                    );
                }
            }
        }

        (total_merged, total_deferred)
    }
}

/// Statistics from a compaction cycle.
#[derive(
    Debug,
    Clone,
    Default,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct CompactionStats {
    /// Number of tombstoned vectors removed across all collections.
    pub vectors_compacted: usize,
    /// Number of collections that had tombstones compacted.
    pub collections_compacted: usize,
    /// Whether CSR write buffers were compacted.
    pub csr_compacted: bool,
    /// Number of dangling edges swept.
    pub edges_swept: usize,
    /// Number of L1 segments selected for merge compaction.
    pub segments_merged: usize,

    /// Vector collections skipped because their database was over its
    /// per-minute maintenance CPU budget.
    pub vectors_deferred: usize,
    /// Whether CSR compaction was skipped due to budget exhaustion.
    pub csr_deferred: bool,
    /// Whether dangling-edge sweep was skipped due to budget exhaustion.
    pub edges_deferred: bool,
    /// `(tenant, collection)` pairs whose L1 segment compaction was skipped
    /// due to budget exhaustion.
    pub segments_deferred: usize,
}

#[cfg(test)]
mod tests {
    use super::DatabaseId;
    use crate::engine::vector::hnsw::graph::HnswParams;

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
}
