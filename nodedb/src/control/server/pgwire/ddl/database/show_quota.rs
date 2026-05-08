// SPDX-License-Identifier: BUSL-1.1

//! Handler for `SHOW DATABASE QUOTA FOR <name>`.
//!
//! Returns one row per quota dimension for the named database, showing the
//! configured limit from `_system.database_quotas`. Falls back to
//! `QuotaRecord::DEFAULT` when no explicit quota has been set.

use std::sync::Arc;

use futures::stream;
use nodedb_types::QuotaRecord;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_admin, sqlstate_error, text_field};

/// Handle `SHOW DATABASE QUOTA FOR <name>`.
pub fn handle_show_database_quota(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "show database quota")?;

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

    let schema = Arc::new(vec![
        text_field("database"),
        text_field("quota_name"),
        text_field("limit"),
        text_field("priority_class"),
        text_field("cache_weight"),
        text_field("maintenance_cpu_pct"),
    ]);

    let dims: &[(&str, u64)] = &[
        ("max_memory_bytes", record.max_memory_bytes),
        ("max_storage_bytes", record.max_storage_bytes),
        ("max_qps", record.max_qps as u64),
        ("max_connections", record.max_connections as u64),
    ];

    let priority_str = format!("{:?}", record.priority_class).to_lowercase();

    let mut rows = Vec::new();
    for &(quota_name, limit) in dims {
        let mut enc = DataRowEncoder::new(schema.clone());
        enc.encode_field(&name.to_string())?;
        enc.encode_field(&quota_name.to_string())?;
        let limit_str = if limit == 0 {
            "unlimited".to_string()
        } else {
            limit.to_string()
        };
        enc.encode_field(&limit_str)?;
        enc.encode_field(&priority_str)?;
        enc.encode_field(&record.cache_weight.to_string())?;
        enc.encode_field(&record.maintenance_cpu_pct.to_string())?;
        rows.push(Ok(enc.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}
