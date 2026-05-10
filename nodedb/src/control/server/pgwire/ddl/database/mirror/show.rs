// SPDX-License-Identifier: BUSL-1.1

//! Handler for `SHOW DATABASE MIRROR STATUS [FOR <name>]`.
//!
//! Returns one row per mirror database (or one if `FOR <name>` is specified):
//! - `name`              — local database name
//! - `source_cluster`    — source cluster identifier
//! - `source_database`   — source database numeric id
//! - `mode`              — "sync" or "async"
//! - `status`            — mirror lifecycle status string
//! - `bytes_done`        — bytes received during Bootstrapping (0 otherwise)
//! - `bytes_total`       — total snapshot bytes (0 when unknown)
//! - `lag_ms`            — replication lag in ms (0 when not Degraded)
//! - `last_applied_lsn`  — WAL LSN of last applied entry (from mirror_lag table)
//! - `last_apply_ms`     — wall-clock ms when last_applied_lsn was applied

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;
use pgwire::messages::data::DataRow;

use nodedb_types::{MirrorMode, MirrorStatus};

use crate::control::security::catalog::database_types::DatabaseStatus;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::super::types::{require_tenant_admin, sqlstate_error, text_field};

/// Handle `SHOW DATABASE MIRROR STATUS [FOR <name>]`.
pub fn handle_show_database_mirror_status(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: Option<&str>,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "show database mirror status")?;

    let catalog = state.credentials.catalog();
    let catalog = catalog
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    let all_databases = catalog
        .list_databases()
        .map_err(|e| sqlstate_error("XX000", &format!("catalog list failed: {e}")))?;

    let schema = Arc::new(vec![
        text_field("name"),
        text_field("source_cluster"),
        text_field("source_database"),
        text_field("mode"),
        text_field("status"),
        text_field("bytes_done"),
        text_field("bytes_total"),
        text_field("lag_ms"),
        text_field("last_applied_lsn"),
        text_field("last_apply_ms"),
    ]);

    let mut rows: Vec<DataRow> = Vec::new();

    for db in &all_databases {
        // Filter: only mirror databases (Mirroring status or Active with Promoted origin).
        let origin = match &db.mirror_origin {
            Some(o) => o,
            None => continue,
        };

        // Apply FOR <name> filter if specified.
        if let Some(filter) = name
            && !db.name.eq_ignore_ascii_case(filter)
        {
            continue;
        }

        // Check that the database status is consistent with a mirror lifecycle.
        match db.status {
            DatabaseStatus::Mirroring | DatabaseStatus::Active => {}
            DatabaseStatus::Deactivated | DatabaseStatus::Cloning => continue,
        }

        let mode_str = match origin.mode {
            MirrorMode::Sync => "sync",
            MirrorMode::Async => "async",
        };

        let (status_str, bytes_done, bytes_total, lag_ms) = match &origin.status {
            MirrorStatus::Bootstrapping {
                bytes_done,
                bytes_total,
            } => ("bootstrapping", *bytes_done, *bytes_total, 0u64),
            MirrorStatus::Following => ("following", 0u64, 0u64, 0u64),
            MirrorStatus::Degraded { lag_ms } => ("degraded", 0, 0, *lag_ms),
            MirrorStatus::Disconnected => ("disconnected", 0, 0, 0),
            MirrorStatus::Promoted => ("promoted", 0, 0, 0),
        };

        // Load lag record from _system.mirror_lag for precise LSN / ms values.
        let (last_applied_lsn, last_apply_ms) = match catalog.get_mirror_lag(db.id) {
            Ok(Some(lag)) => (lag.last_applied_lsn.as_u64(), lag.last_apply_ms),
            Ok(None) => (origin.last_applied.as_u64(), 0u64),
            Err(_) => (origin.last_applied.as_u64(), 0u64),
        };

        let mut encoder = DataRowEncoder::new(Arc::clone(&schema));
        encoder.encode_field(&db.name)?;
        encoder.encode_field(&origin.source_cluster)?;
        encoder.encode_field(&origin.source_database.as_u64().to_string())?;
        encoder.encode_field(&mode_str.to_string())?;
        encoder.encode_field(&status_str.to_string())?;
        encoder.encode_field(&bytes_done.to_string())?;
        encoder.encode_field(&bytes_total.to_string())?;
        encoder.encode_field(&lag_ms.to_string())?;
        encoder.encode_field(&last_applied_lsn.to_string())?;
        encoder.encode_field(&last_apply_ms.to_string())?;
        rows.push(encoder.take_row());
    }

    // When a specific name was requested and no rows were found, return an error.
    if let Some(filter) = name
        && rows.is_empty()
    {
        return Err(sqlstate_error(
            "42P01",
            &format!("mirror database '{filter}' not found"),
        ));
    }

    let row_stream = stream::iter(rows.into_iter().map(Ok::<_, pgwire::error::PgWireError>));
    Ok(vec![Response::Query(QueryResponse::new(
        Arc::clone(&schema),
        row_stream,
    ))])
}
