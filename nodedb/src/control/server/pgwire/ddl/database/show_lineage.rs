// SPDX-License-Identifier: BUSL-1.1

//! Handler for `SHOW DATABASE LINEAGE FOR <name>`.
//!
//! Walks the `parent_clone` chain from the named database up to the root,
//! emitting one row per ancestor:
//!
//!   database_id | name | as_of_lsn | clone_created_at_lsn
//!
//! The named database itself is included as the first row; ancestor rows
//! follow in order from most recent to oldest.  A non-cloned database
//! returns exactly one row (itself, with `as_of_lsn = 0`).

use std::sync::Arc;

use futures::stream;
use nodedb_types::DatabaseId;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_tenant_admin, sqlstate_error, text_field};

/// One row in the lineage result set.
struct LineageRow {
    database_id: DatabaseId,
    name: String,
    /// `as_of_lsn` for this clone (the LSN boundary inherited from its parent).
    /// Zero for the root database (which has no parent clone reference).
    as_of_lsn: u64,
    /// LSN at which this clone was created.  Zero for the root database.
    clone_created_at_lsn: u64,
}

/// Handle `SHOW DATABASE LINEAGE FOR <name>`.
pub fn handle_show_database_lineage(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "show database lineage")?;

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    let start_id = catalog
        .get_database_id_by_name(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
        .ok_or_else(|| sqlstate_error("3D000", &format!("database '{name}' does not exist")))?;

    // Walk the parent_clone chain, bounded by MAX_CLONE_DEPTH to prevent
    // infinite loops from corrupt catalog state.
    let mut lineage: Vec<LineageRow> = Vec::new();
    let mut current_id = start_id;
    let max_hops = nodedb_types::MAX_CLONE_DEPTH + 2; // +2 for safety headroom

    for _ in 0..max_hops {
        let desc = catalog
            .get_database(current_id)
            .map_err(|e| sqlstate_error("XX000", &format!("catalog read failed: {e}")))?
            .ok_or_else(|| {
                sqlstate_error(
                    "XX000",
                    &format!("database id {} descriptor missing", current_id.as_u64()),
                )
            })?;

        let (as_of_lsn, clone_created_at_lsn) = match &desc.parent_clone {
            Some(p) => (p.as_of_lsn, desc.created_at_lsn),
            None => (0u64, 0u64),
        };

        lineage.push(LineageRow {
            database_id: current_id,
            name: desc.name.clone(),
            as_of_lsn,
            clone_created_at_lsn,
        });

        match desc.parent_clone {
            Some(p) => {
                current_id = p.source_db_id;
            }
            None => break,
        }
    }

    let schema = Arc::new(vec![
        text_field("database_id"),
        text_field("name"),
        text_field("as_of_lsn"),
        text_field("clone_created_at_lsn"),
    ]);

    let mut rows = Vec::new();
    for row in lineage {
        let mut enc = DataRowEncoder::new(schema.clone());
        enc.encode_field(&row.database_id.as_u64().to_string())?;
        enc.encode_field(&row.name)?;
        enc.encode_field(&row.as_of_lsn.to_string())?;
        enc.encode_field(&row.clone_created_at_lsn.to_string())?;
        rows.push(Ok(enc.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}
