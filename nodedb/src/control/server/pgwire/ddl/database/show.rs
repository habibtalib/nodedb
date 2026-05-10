// SPDX-License-Identifier: BUSL-1.1

//! Handler for `SHOW DATABASES`.
//!
//! Returns one row per database: name, status, created_at (WAL LSN),
//! quota_id, collection_count, tenant_count, parent_clone.
//!
//! `tenant_count` reports `0` until per-database tenant scoping is wired;
//! the column is part of the stable output schema and will populate when
//! the tenant-by-database index lands. `quota_id` reflects the descriptor's
//! `quota_ref` field and is updated by `ALTER DATABASE ... SET QUOTA`.

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;
use pgwire::messages::data::DataRow;

use crate::control::security::catalog::database_types::DatabaseStatus;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_tenant_admin, sqlstate_error, text_field};

/// Handle `SHOW DATABASES`.
pub fn handle_show_databases(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "show databases")?;

    let catalog = match state.credentials.catalog() {
        Some(c) => c,
        None => {
            return Err(sqlstate_error("XX000", "system catalog unavailable"));
        }
    };

    let databases = catalog
        .list_databases()
        .map_err(|e| sqlstate_error("XX000", &format!("catalog list failed: {e}")))?;

    let schema = Arc::new(vec![
        text_field("name"),
        text_field("status"),
        text_field("created_at_lsn"),
        text_field("quota_id"),
        text_field("collection_count"),
        text_field("tenant_count"),
        text_field("parent_clone"),
    ]);

    let mut rows: Vec<DataRow> = Vec::with_capacity(databases.len());
    for db in &databases {
        let status_str = match db.status {
            DatabaseStatus::Active => "active",
            DatabaseStatus::Deactivated => "deactivated",
            DatabaseStatus::Cloning => "cloning",
            DatabaseStatus::Mirroring => "mirroring",
        };

        let collection_count = catalog
            .load_all_collections(db.id)
            .map(|c| c.len())
            .unwrap_or(0);

        let parent_clone = db
            .parent_clone
            .as_ref()
            .map(|p| format!("db:{}", p.source_db_id.as_u64()))
            .unwrap_or_default();

        let mut encoder = DataRowEncoder::new(Arc::clone(&schema));
        encoder.encode_field(&db.name)?;
        encoder.encode_field(&status_str.to_string())?;
        encoder.encode_field(&db.created_at_lsn.to_string())?;
        encoder.encode_field(&db.quota_ref.to_string())?;
        encoder.encode_field(&collection_count.to_string())?;
        encoder.encode_field(&"0".to_string())?; // tenant_count: per-database tenant index not yet wired
        encoder.encode_field(&parent_clone)?;
        rows.push(encoder.take_row());
    }

    let row_stream = stream::iter(rows.into_iter().map(Ok::<_, pgwire::error::PgWireError>));
    Ok(vec![Response::Query(QueryResponse::new(
        Arc::clone(&schema),
        row_stream,
    ))])
}
