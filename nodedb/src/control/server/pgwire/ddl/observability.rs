// SPDX-License-Identifier: BUSL-1.1

//! Administrative observability `SHOW` commands: server-wide counters,
//! per-engine query stats, and per-engine memory budgets.
//!
//! `SHOW STATS` and `SHOW SERVER STATS` expose the same underlying
//! `SystemMetrics` counters used by the Prometheus `/metrics` endpoint
//! and the OTLP exporter — without forcing administrators to leave the
//! pgwire session for a side-channel HTTP probe.
//!
//! `SHOW METRICS` is a `(key, value)` projection of the same source,
//! suitable for grep-and-pipe inspection from `psql`.
//!
//! `SHOW MEMORY` reports per-engine memory budgets and current
//! utilisation from `nodedb_mem::MemoryGovernor`.

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::types::{int8_field, require_tenant_admin, text_field};

/// Render an `(key, value)` schema and emit one row per `(name, value)`
/// pair. Both columns are TEXT — the consumer interprets numbers.
fn key_value_response(rows_in: Vec<(String, String)>) -> PgWireResult<Vec<Response>> {
    let schema = Arc::new(vec![text_field("name"), text_field("value")]);
    let mut rows = Vec::with_capacity(rows_in.len());
    for (k, v) in rows_in {
        let mut encoder = DataRowEncoder::new(schema.clone());
        encoder.encode_field(&k)?;
        encoder.encode_field(&v)?;
        rows.push(Ok(encoder.take_row()));
    }
    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// Build the canonical `(name, value)` rows for `SHOW STATS` and
/// `SHOW SERVER STATS`. Both commands use the same source; the
/// distinction is purely a UX synonym.
fn server_stats_rows(state: &SharedState) -> Vec<(String, String)> {
    use std::sync::atomic::Ordering;

    let mut rows: Vec<(String, String)> = Vec::new();

    rows.push(("version".into(), crate::version::VERSION.to_string()));

    if let Some(sys) = state.system_metrics.as_ref() {
        rows.push((
            "queries_total".into(),
            sys.queries_total.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "query_errors".into(),
            sys.query_errors.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "slow_queries_total".into(),
            sys.slow_queries_total.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "active_connections".into(),
            sys.active_connections.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "pgwire_connections".into(),
            sys.pgwire_connections.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "http_connections".into(),
            sys.http_connections.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "native_connections".into(),
            sys.native_connections.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "auth_failures".into(),
            sys.auth_failures.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "auth_successes".into(),
            sys.auth_successes.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "wal_fsync_count".into(),
            sys.wal_fsync_count.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "wal_segment_count".into(),
            sys.wal_segment_count.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "wal_segment_bytes".into(),
            sys.wal_segment_bytes.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "raft_apply_lag".into(),
            sys.raft_apply_lag.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "compaction_debt".into(),
            sys.compaction_debt.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "compaction_cycles".into(),
            sys.compaction_cycles.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "queries_vector".into(),
            sys.queries_vector.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "queries_graph".into(),
            sys.queries_graph.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "queries_document".into(),
            sys.queries_document.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "queries_columnar".into(),
            sys.queries_columnar.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "queries_kv".into(),
            sys.queries_kv.load(Ordering::Relaxed).to_string(),
        ));
        rows.push((
            "queries_fts".into(),
            sys.queries_fts.load(Ordering::Relaxed).to_string(),
        ));
    }

    rows
}

/// SHOW STATS / SHOW SERVER STATS — server-wide counters as
/// `(name, value)` rows.
///
/// Restricted to tenant_admin or superuser; the same authorisation
/// envelope as `SHOW USERS`. Numbers are emitted as their decimal
/// string form so the column type stays uniform.
pub fn show_server_stats(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "show stats")?;
    key_value_response(server_stats_rows(state))
}

/// SHOW METRICS — `(name, value)` projection of the same source as
/// SHOW STATS, with histogram percentiles appended so latency-style
/// metrics are visible from the SQL surface.
pub fn show_metrics(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "show metrics")?;
    let mut rows = server_stats_rows(state);

    if let Some(sys) = state.system_metrics.as_ref() {
        rows.push((
            "wal_fsync_p50_us".into(),
            sys.wal_fsync_seconds.percentile(50.0).to_string(),
        ));
        rows.push((
            "wal_fsync_p99_us".into(),
            sys.wal_fsync_seconds.percentile(99.0).to_string(),
        ));
        rows.push((
            "query_latency_p50_us".into(),
            sys.query_latency.percentile(50.0).to_string(),
        ));
        rows.push((
            "query_latency_p99_us".into(),
            sys.query_latency.percentile(99.0).to_string(),
        ));
    }

    key_value_response(rows)
}

/// SHOW MEMORY — per-engine memory budget and utilisation.
///
/// Columns: `engine`, `allocated_bytes`, `limit_bytes`, `peak_bytes`,
/// `rejections`, `utilization_percent`. One row per engine in
/// `nodedb_mem::EngineId`.
pub fn show_memory(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "show memory")?;

    let schema = Arc::new(vec![
        text_field("engine"),
        int8_field("allocated_bytes"),
        int8_field("limit_bytes"),
        int8_field("peak_bytes"),
        int8_field("rejections"),
        int8_field("utilization_percent"),
    ]);

    let mut rows = Vec::new();
    if let Some(gov) = state.governor.as_ref() {
        for snap in gov.snapshot() {
            let mut encoder = DataRowEncoder::new(schema.clone());
            encoder.encode_field(&format!("{:?}", snap.engine))?;
            encoder.encode_field(&(snap.allocated as i64))?;
            encoder.encode_field(&(snap.limit as i64))?;
            encoder.encode_field(&(snap.peak as i64))?;
            encoder.encode_field(&(snap.rejections as i64))?;
            encoder.encode_field(&(snap.utilization_percent as i64))?;
            rows.push(Ok(encoder.take_row()));
        }
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}
