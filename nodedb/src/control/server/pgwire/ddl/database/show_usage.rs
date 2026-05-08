// SPDX-License-Identifier: BUSL-1.1

//! Handler for `SHOW DATABASE USAGE FOR <name>`.
//!
//! Reports the configured quota dimensions for a database alongside live
//! values pulled from `SystemMetrics`. The metrics gauges are wired by the
//! memory governor (memory), compaction (storage), and the query path
//! (queries). Dimensions that have no per-database accounting source yet
//! (currently `max_connections`) report `0` — the value the gauge actually
//! holds — rather than a fabricated placeholder. The `current` column is
//! always the live gauge value; `percent_used` is computed from it.

use std::sync::Arc;

use futures::stream;
use nodedb_types::QuotaRecord;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_admin, sqlstate_error, text_field};

/// Handle `SHOW DATABASE USAGE FOR <name>`.
pub fn handle_show_database_usage(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "show database usage")?;

    let catalog = match state.credentials.catalog() {
        Some(c) => c,
        None => return Err(sqlstate_error("XX000", "system catalog unavailable")),
    };

    let db_id = catalog
        .get_database_id_by_name(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
        .ok_or_else(|| sqlstate_error("3D000", &format!("database '{name}' does not exist")))?;

    let record = catalog
        .get_database_quota(db_id)
        .map_err(|e| sqlstate_error("XX000", &format!("quota read failed: {e}")))?
        .unwrap_or(QuotaRecord::DEFAULT);

    // Pull live gauges from the system metrics registry. Dimensions without a
    // per-database accounting source land as `0`, which is the gauge's actual
    // value, not a fabricated placeholder.
    let (cur_memory, cur_storage, cur_queries) = match &state.system_metrics {
        Some(m) => (
            m.database_memory_bytes(name),
            m.database_storage_bytes(name),
            m.database_queries_total(name),
        ),
        None => (0, 0, 0),
    };
    // No per-database connection gauge yet — `connections_accepted` /
    // `connections_rejected` on SharedState are process-global and would
    // mislead under a per-database column. Report `0` until per-database
    // connection accounting lands.
    let cur_connections: u64 = 0;

    let schema = Arc::new(vec![
        text_field("database"),
        text_field("quota_name"),
        text_field("limit"),
        text_field("current"),
        text_field("percent_used"),
    ]);

    let dims: &[(&str, u64, u64)] = &[
        ("max_memory_bytes", record.max_memory_bytes, cur_memory),
        ("max_storage_bytes", record.max_storage_bytes, cur_storage),
        ("max_qps", record.max_qps as u64, cur_queries),
        (
            "max_connections",
            record.max_connections as u64,
            cur_connections,
        ),
    ];

    let mut rows = Vec::new();
    for &(quota_name, limit, current) in dims {
        let limit_str = if limit == 0 {
            "unlimited".to_string()
        } else {
            limit.to_string()
        };
        let pct_str = format_percent(limit, current);
        let mut enc = DataRowEncoder::new(schema.clone());
        enc.encode_field(&name.to_string())?;
        enc.encode_field(&quota_name.to_string())?;
        enc.encode_field(&limit_str)?;
        enc.encode_field(&current.to_string())?;
        enc.encode_field(&pct_str)?;
        rows.push(Ok(enc.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// Render `current / limit` as a `"<n>%"` string.
///
/// `limit == 0` means "unlimited" and renders as `"n/a"` (percentage of
/// infinity is undefined). Otherwise the result is `(current * 100 / limit)`
/// floored, with the divisor pre-promoted to `u128` so the multiply can never
/// overflow even when both inputs are near `u64::MAX`.
pub(crate) fn format_percent(limit: u64, current: u64) -> String {
    if limit == 0 {
        return "n/a".to_string();
    }
    let pct = (u128::from(current) * 100) / u128::from(limit);
    format!("{pct}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_unlimited_renders_na() {
        assert_eq!(format_percent(0, 100), "n/a");
        assert_eq!(format_percent(0, 0), "n/a");
    }

    #[test]
    fn percent_basic_arithmetic() {
        assert_eq!(format_percent(100, 25), "25%");
        assert_eq!(format_percent(100, 100), "100%");
        assert_eq!(format_percent(100, 200), "200%"); // over-budget surfaces, never silently clamped
        assert_eq!(format_percent(1000, 0), "0%");
    }

    #[test]
    fn percent_does_not_overflow_on_max() {
        // Both at u64::MAX: 100 * MAX / MAX = 100. Pre-u64 arithmetic would overflow.
        assert_eq!(format_percent(u64::MAX, u64::MAX), "100%");
    }
}
