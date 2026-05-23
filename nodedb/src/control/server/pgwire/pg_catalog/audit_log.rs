// SPDX-License-Identifier: BUSL-1.1

//! `_system.audit_log` virtual view.
//!
//! Materializes the durable audit log as a [`VTable`]. The query evaluator
//! (`vquery::execute`) then applies WHERE / aggregates / projection /
//! ORDER BY / LIMIT to the returned rows. There is no SQL-level pushdown
//! here — the caller's row budget is enforced by an unconditional materialize
//! cap (`MATERIALIZE_LIMIT`) that bounds memory regardless of the client query.
//!
//! Columns:
//! - `seq`          — monotonic sequence number (int8).
//! - `timestamp_us` — UTC microseconds since epoch (int8).
//! - `event`        — event discriminant name (text, e.g. "AuthSuccess").
//! - `tenant_id`    — tenant identifier, 0 if not applicable (int8).
//! - `source`       — source IP or node identifier (text).
//! - `detail`       — human-readable event detail (text).
//! - `prev_hash`    — SHA-256 hex of the previous chain entry (text).
//!
//! Permission required: `audit_log:read`, granted to `superuser` and
//! `monitor` roles. Access is enforced here before any data is read.

use pgwire::error::PgWireResult;

use crate::control::security::identity::{AuthenticatedIdentity, Role};
use crate::control::server::pgwire::pg_catalog::vquery::VTable;
use crate::control::server::pgwire::pg_catalog::vquery::value::{VColumn, VType, VValue};
use crate::control::state::SharedState;

/// Upper bound on rows materialized for the evaluator. Independent of any
/// client-supplied LIMIT — the LIMIT is applied by the evaluator after
/// WHERE / aggregate / ORDER BY. This cap exists only to bound memory.
const MATERIALIZE_LIMIT: usize = 100_000;

pub fn audit_log(state: &SharedState, identity: &AuthenticatedIdentity) -> PgWireResult<VTable> {
    if !identity.is_superuser && !identity.has_role(&Role::Monitor) {
        return Err(pgwire::error::PgWireError::UserError(Box::new(
            pgwire::error::ErrorInfo::new(
                "ERROR".to_string(),
                "42501".to_string(),
                "permission denied: audit_log:read requires superuser or monitor role".to_string(),
            ),
        )));
    }

    let mut table = VTable::new(vec![
        VColumn::new("seq", VType::Int8),
        VColumn::new("timestamp_us", VType::Int8),
        VColumn::new("event", VType::Text),
        VColumn::new("tenant_id", VType::Int8),
        VColumn::new("source", VType::Text),
        VColumn::new("detail", VType::Text),
        VColumn::new("prev_hash", VType::Text),
    ]);

    // Merge catalog-persisted entries with the in-memory tail. The audit log
    // is flushed from memory to the catalog by a periodic background timer
    // (`SharedState::flush_audit_log`), so the in-memory log always holds
    // the most recent entries that have not yet been persisted. Reading
    // only the catalog would hide those entries from operators querying
    // `_system.audit_log` between flush ticks. Dedupe by `seq`.
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();

    if let Some(catalog) = state.credentials.catalog() {
        let entries = catalog
            .load_audit_entries_ranged(1, u64::MAX, 0, u64::MAX, MATERIALIZE_LIMIT)
            .map_err(|e| pgwire::error::PgWireError::ApiError(Box::new(e)))?;
        for e in entries {
            if !seen.insert(e.seq) {
                continue;
            }
            table.push(vec![
                VValue::Int8(e.seq as i64),
                VValue::Int8(e.timestamp_us as i64),
                VValue::Text(e.event),
                VValue::Int8(e.tenant_id.unwrap_or(0) as i64),
                VValue::Text(e.source),
                VValue::Text(e.detail),
                if e.prev_hash.is_empty() {
                    VValue::Null
                } else {
                    VValue::Text(e.prev_hash)
                },
            ]);
        }
    }

    let log = match state.audit.lock() {
        Ok(l) => l,
        Err(p) => p.into_inner(),
    };
    let all = log.all();
    let skip = all.len().saturating_sub(MATERIALIZE_LIMIT);
    for entry in all.iter().skip(skip) {
        if !seen.insert(entry.seq) {
            continue;
        }
        if table.rows.len() >= MATERIALIZE_LIMIT {
            break;
        }
        table.push(vec![
            VValue::Int8(entry.seq as i64),
            VValue::Int8(entry.timestamp_us as i64),
            VValue::Text(format!("{:?}", entry.event)),
            VValue::Int8(entry.tenant_id.map_or(0i64, |t| t.as_u64() as i64)),
            VValue::Text(entry.source.clone()),
            VValue::Text(entry.detail.clone()),
            if entry.prev_hash.is_empty() {
                VValue::Null
            } else {
                VValue::Text(entry.prev_hash.clone())
            },
        ]);
    }
    Ok(table)
}
