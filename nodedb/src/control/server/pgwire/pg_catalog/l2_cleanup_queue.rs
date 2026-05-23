// SPDX-License-Identifier: BUSL-1.1

//! `_system.l2_cleanup_queue` virtual view — materializer.

use pgwire::error::PgWireResult;

use crate::control::security::identity::{AuthenticatedIdentity, Role};
use crate::control::server::pgwire::pg_catalog::vquery::VTable;
use crate::control::server::pgwire::pg_catalog::vquery::value::{VColumn, VType, VValue};
use crate::control::state::SharedState;

pub fn l2_cleanup_queue(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<VTable> {
    let mut table = VTable::new(vec![
        VColumn::new("tenant_id", VType::Int8),
        VColumn::new("name", VType::Text),
        VColumn::new("purge_lsn", VType::Int8),
        VColumn::new("enqueued_at_ns", VType::Int8),
        VColumn::new("bytes_pending", VType::Int8),
        VColumn::new("last_error", VType::Text),
        VColumn::new("attempts", VType::Int4),
    ]);

    let Some(catalog) = state.credentials.catalog() else {
        return Ok(table);
    };
    let queue = catalog
        .load_l2_cleanup_queue()
        .map_err(|e| pgwire::error::PgWireError::ApiError(Box::new(e)))?;

    let is_admin = identity.is_superuser || identity.has_role(&Role::TenantAdmin);
    let caller_tenant = identity.tenant_id.as_u64();

    for e in &queue {
        if !is_admin && e.tenant_id != caller_tenant {
            continue;
        }
        table.push(vec![
            VValue::Int8(e.tenant_id as i64),
            VValue::Text(e.name.clone()),
            VValue::Int8(e.purge_lsn as i64),
            VValue::Int8(e.enqueued_at_ns as i64),
            VValue::Int8(e.bytes_pending as i64),
            VValue::Text(e.last_error.clone()),
            VValue::Int4(e.attempts as i32),
        ]);
    }
    Ok(table)
}
