// SPDX-License-Identifier: BUSL-1.1

//! Snapshot phase for `MOVE TENANT`.
//!
//! Dispatches `PhysicalPlan::Meta(MetaOp::CreateTenantSnapshot)` to the
//! local Data Plane and returns the raw snapshot bytes.  In the offline v1
//! implementation the snapshot is performed on the local node only — the
//! offline drain window ensures no cross-node writes are in-flight.  The
//! cluster fan-out orchestrator (`backup::orchestrator::backup_tenant`) is
//! intentionally bypassed here because its caller path requires
//! `Arc<SharedState>`, which the DDL dispatch pipeline does not carry.

use std::time::Duration;

use bytes::Bytes;

use crate::bridge::physical_plan::{MetaOp, PhysicalPlan};

use crate::control::server::pgwire::ddl::sync_dispatch;
use crate::control::state::SharedState;
use crate::types::TenantId;
use nodedb_types::NodeDbError;

/// Run the snapshot phase: produce a backup snapshot for `tenant_id` via
/// a local Data Plane dispatch.
///
/// Returns the raw snapshot bytes on success.
pub async fn run(
    state: &SharedState,
    tenant_id: TenantId,
    timeout: Duration,
) -> Result<Bytes, NodeDbError> {
    let plan = PhysicalPlan::Meta(MetaOp::CreateTenantSnapshot {
        tenant_id: tenant_id.as_u64(),
    });
    let raw = sync_dispatch::dispatch_async(state, tenant_id, "__system", plan, timeout)
        .await
        .map_err(|e| {
            NodeDbError::move_tenant_snapshot_failed(tenant_id.as_u64().to_string(), format!("{e}"))
        })?;
    Ok(Bytes::from(raw))
}

/// Return the temporary in-cluster storage key for the tenant's snapshot.
///
/// This key is recorded in the journal so crash recovery can clean up any
/// partial snapshot artifact.
pub fn temp_key(tenant_id: TenantId) -> String {
    format!("_move_tenant_snapshot_{}", tenant_id.as_u64())
}

/// Delete the temporary snapshot (best-effort; called on cutover success or
/// failure compensation).
///
/// In this implementation the snapshot lives in memory and is not persisted
/// to a separate store — the `temp_key` recorded in the journal is used for
/// identification only.  This function is a no-op but is kept as an extension
/// point for future durable snapshot storage.
pub async fn delete_temp(_state: &SharedState, _key: &str) -> Result<(), NodeDbError> {
    Ok(())
}
