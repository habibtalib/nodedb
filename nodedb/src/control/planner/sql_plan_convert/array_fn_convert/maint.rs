// SPDX-License-Identifier: BUSL-1.1

//! ARRAY_FLUSH / ARRAY_COMPACT → PhysicalPlan::Array(ArrayOp::*).

use nodedb_array::types::ArrayId;

use crate::bridge::envelope::PhysicalPlan;
use crate::bridge::physical_plan::ArrayOp;
use crate::types::{DatabaseId, TenantId, VShardId};

use super::super::super::physical::{PhysicalTask, PostSetOp};
use super::super::convert::ConvertContext;

pub(crate) fn convert_flush(
    name: &str,
    tenant_id: TenantId,
    ctx: &ConvertContext,
) -> crate::Result<Vec<PhysicalTask>> {
    let _ = super::helpers::load_entry(name, ctx)?;
    let wal = ctx.wal.as_ref().ok_or_else(|| crate::Error::PlanError {
        detail: "ARRAY_FLUSH: no WAL wired into convert context".into(),
    })?;
    let aid = ArrayId::new(tenant_id, name);
    let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, name);
    let wal_lsn = wal.next_lsn().as_u64();
    Ok(vec![PhysicalTask {
        tenant_id,
        vshard_id: vshard,
        database_id: crate::types::DatabaseId::DEFAULT,
        plan: PhysicalPlan::Array(ArrayOp::Flush {
            array_id: aid,
            wal_lsn,
        }),
        post_set_op: PostSetOp::None,
    }])
}

pub(crate) fn convert_compact(
    name: &str,
    tenant_id: TenantId,
    ctx: &ConvertContext,
) -> crate::Result<Vec<PhysicalTask>> {
    let entry = super::helpers::load_entry(name, ctx)?;
    let aid = ArrayId::new(tenant_id, name);
    let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, name);
    Ok(vec![PhysicalTask {
        tenant_id,
        vshard_id: vshard,
        database_id: crate::types::DatabaseId::DEFAULT,
        plan: PhysicalPlan::Array(ArrayOp::Compact {
            array_id: aid,
            audit_retain_ms: entry.audit_retain_ms,
        }),
        post_set_op: PostSetOp::None,
    }])
}
