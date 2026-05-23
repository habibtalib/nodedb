// SPDX-License-Identifier: BUSL-1.1

//! `_system.dropped_collections` virtual view — materializer.

use pgwire::error::PgWireResult;

use crate::control::security::identity::{AuthenticatedIdentity, Role};
use crate::control::server::pgwire::pg_catalog::vquery::VTable;
use crate::control::server::pgwire::pg_catalog::vquery::value::{VColumn, VType, VValue};
use crate::control::state::SharedState;
use crate::types::{DatabaseId, TraceId};

pub async fn dropped_collections(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<VTable> {
    let mut table = VTable::new(vec![
        VColumn::new("tenant_id", VType::Int8),
        VColumn::new("name", VType::Text),
        VColumn::new("owner", VType::Text),
        VColumn::new("engine_type", VType::Text),
        VColumn::new("deactivated_at_ns", VType::Int8),
        VColumn::new("retention_expires_at_ns", VType::Int8),
        VColumn::new("size_bytes_estimate", VType::Int8),
    ]);

    let Some(catalog) = state.credentials.catalog() else {
        return Ok(table);
    };

    let dropped = catalog
        .load_dropped_collections(DatabaseId::DEFAULT)
        .map_err(|e| pgwire::error::PgWireError::ApiError(Box::new(e)))?;

    let retention = state
        .retention_settings
        .read()
        .map(|r| r.retention_window())
        .unwrap_or_else(|_| crate::config::server::RetentionSettings::default().retention_window());
    let retention_ns = retention.as_nanos() as u64;

    let is_admin = identity.is_superuser || identity.has_role(&Role::TenantAdmin);
    let caller_tenant = identity.tenant_id.as_u64();

    for coll in &dropped {
        if !is_admin && coll.tenant_id != caller_tenant {
            continue;
        }
        let deactivated_ns = coll.modification_hlc.wall_ns;
        let expires_ns = deactivated_ns.saturating_add(retention_ns);
        let engine_type = coll.collection_type.as_str().to_string();

        let size_estimate = if coll.size_bytes_estimate > 0 {
            coll.size_bytes_estimate
        } else {
            query_collection_size(state, coll.tenant_id, &coll.name)
                .await
                .unwrap_or(0)
        };

        table.push(vec![
            VValue::Int8(coll.tenant_id as i64),
            VValue::Text(coll.name.clone()),
            VValue::Text(coll.owner.clone()),
            VValue::Text(engine_type),
            VValue::Int8(deactivated_ns as i64),
            VValue::Int8(expires_ns as i64),
            VValue::Int8(size_estimate as i64),
        ]);
    }
    Ok(table)
}

async fn query_collection_size(
    state: &SharedState,
    tenant_id: u64,
    collection: &str,
) -> Option<u64> {
    use crate::bridge::envelope::{PhysicalPlan, Priority, Request, Status};
    use crate::types::{DatabaseId, ReadConsistency, TenantId, VShardId};
    use nodedb_physical::physical_plan::MetaOp;

    let request_id = state.next_request_id();
    let timeout = std::time::Duration::from_millis(500);

    let request = Request {
        request_id,
        tenant_id: TenantId::new(tenant_id),
        database_id: DatabaseId::DEFAULT,
        vshard_id: VShardId::new(0),
        plan: PhysicalPlan::Meta(MetaOp::QueryCollectionSize {
            tenant_id,
            name: collection.to_string(),
        }),
        deadline: std::time::Instant::now() + timeout,
        priority: Priority::Background,
        trace_id: TraceId::generate(),
        consistency: ReadConsistency::Eventual,
        idempotency_key: None,
        event_source: crate::event::EventSource::User,
        user_roles: Vec::new(),
        user_id: None,
        statement_digest: None,
    };
    let mut rx = state.tracker.register(request_id);
    {
        let mut d = state.dispatcher.lock().unwrap_or_else(|p| p.into_inner());
        if d.dispatch_to_core(0, request).is_err() {
            state.tracker.cancel(&request_id);
            return None;
        }
    }
    let resp = tokio::time::timeout(timeout, async { rx.recv().await.ok_or(()) })
        .await
        .ok()?
        .ok()?;
    if resp.status != Status::Ok {
        return None;
    }
    let bytes = resp.payload.as_ref();
    if bytes.len() < 8 {
        return None;
    }
    Some(u64::from_le_bytes(bytes[..8].try_into().ok()?))
}
