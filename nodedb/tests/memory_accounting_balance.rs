// SPDX-License-Identifier: BUSL-1.1

//! Engine memory accounting must remain balanced across a full
//! lifecycle of `CREATE → INSERT → DROP`.
//!
//! `Budget::release()` saturates to zero on over-release and emits a
//! `tracing::warn!` — silent in test output, but the real symptom on
//! origin deployments is a continuous "memory release exceeds
//! allocation" log every ~40 s. The dual call-site bug — releasing the
//! wrong size or skipping `acquire` — surfaces as drift in the global
//! `total_allocated()` counter once a workload's resources are fully
//! released.
//!
//! This test pins the invariant: after a known ingest is fully torn
//! down, the governor's accumulated allocation must return to its
//! pre-workload baseline (within a small tolerance for background
//! metadata).

use std::time::Duration;

mod common;

use common::pgwire_harness::TestServer;

/// Drain pending Data Plane releases by repeatedly sleeping until the
/// allocated counter is stable for two consecutive observations, or
/// the deadline expires. Returns the final settled value.
async fn settle_allocated(srv: &TestServer, max_wait: Duration) -> usize {
    let gov = srv
        .shared
        .governor
        .as_ref()
        .expect("test harness must wire the memory governor")
        .clone();
    let start = std::time::Instant::now();
    let mut prev = gov.total_allocated();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let next = gov.total_allocated();
        if next == prev || start.elapsed() >= max_wait {
            return next;
        }
        prev = next;
    }
}

/// After a full `CREATE → INSERT (n=200) → DROP COLLECTION` cycle the
/// governor's total allocated bytes must return to within a small
/// tolerance of the pre-workload baseline.
///
/// Drift in either direction is a bug:
/// - Drift **up** = a call site acquired without a matching release.
/// - Drift **down** would saturate `Budget` to zero on a per-engine
///   basis (the over-release case), so the global counter never goes
///   negative — but residual upward drift from one engine compensating
///   for another's over-release shows up here too.
///
/// The tolerance is generous (16 KiB) because background metadata
/// caches may still hold a handful of bytes after teardown — the
/// originally-reported bench drift was multiple megabytes (~6× over
/// the engine budget), so the tolerance has plenty of headroom.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_memory_returns_to_baseline_after_create_insert_drop() {
    let srv = TestServer::start().await;

    // Baseline: governor at rest, before any user workload.
    let baseline = settle_allocated(&srv, Duration::from_secs(2)).await;

    srv.exec(
        "CREATE COLLECTION mem_balance_check \
         COLUMNS (id TEXT PRIMARY KEY, payload TEXT) \
         WITH (engine='document_strict')",
    )
    .await
    .unwrap();

    // Two hundred small rows — representative of a steady ingest burst
    // and large enough that a per-row leak would dominate the tolerance.
    for i in 0..200u32 {
        srv.exec(&format!(
            "INSERT INTO mem_balance_check (id, payload) \
             VALUES ('r{i}', 'payload-{i}')"
        ))
        .await
        .unwrap();
    }

    srv.exec("DROP COLLECTION mem_balance_check PURGE")
        .await
        .unwrap();

    let settled = settle_allocated(&srv, Duration::from_secs(5)).await;

    // Tolerance: 16 KiB above baseline. The reported drift was
    // multiple-megabyte, so a per-row leak would blow well past this.
    const TOLERANCE_BYTES: usize = 16 * 1024;
    let drift = settled.saturating_sub(baseline);
    assert!(
        drift <= TOLERANCE_BYTES,
        "post-teardown memory drift = {drift} B (settled = {settled}, \
         baseline = {baseline}); tolerance = {TOLERANCE_BYTES} B. \
         Persistent drift after a complete CREATE/INSERT/DROP cycle is \
         a call-site accounting bug — `acquire` without a matching \
         `release`, or the wrong size on either side."
    );

    // The drift check above only catches *under-release* (acquire
    // without matching release). The bench-reported symptom is the
    // dual case: `release(size)` where `size > current`, which
    // saturates the per-engine counter to zero and so is invisible
    // in `allocated()`. Assert directly on the over-release event
    // counter exposed by `Budget` so any call-site that crosses the
    // wrong direction is caught here.
    let gov = srv
        .shared
        .governor
        .as_ref()
        .expect("governor wired")
        .clone();
    let over_releases = gov.total_over_release_count();
    assert_eq!(
        over_releases, 0,
        "memory accounting reported {over_releases} over-release event(s) \
         during the CREATE/INSERT/DROP cycle. Over-release saturates the \
         per-engine counter to zero so it doesn't show up in drift, but \
         each event is the production-reported \"memory release exceeds \
         allocation\" warning — a call-site is releasing more bytes than \
         it reserved."
    );
}
