// SPDX-License-Identifier: BUSL-1.1

use nodedb_sql::types::SqlPlan;

use nodedb_physical::physical_task::PhysicalTask;
use crate::types::TenantId;

use super::ConvertContext;

/// Convert array-related `SqlPlan` variants to `PhysicalTask`s.
/// Called from `convert_one` in `convert.rs`.
pub(super) fn convert_array_plans(
    plan: &SqlPlan,
    tenant_id: TenantId,
    ctx: &ConvertContext,
) -> Option<crate::Result<Vec<PhysicalTask>>> {
    match plan {
        SqlPlan::CreateArray {
            name,
            dims,
            attrs,
            tile_extents,
            cell_order,
            tile_order,
            prefix_bits,
            audit_retain_ms,
            minimum_audit_retain_ms,
        } => Some(super::super::array_convert::convert_create_array(
            super::super::array_convert::CreateArrayArgs {
                name,
                dims,
                attrs,
                tile_extents,
                cell_order: *cell_order,
                tile_order: *tile_order,
                prefix_bits: *prefix_bits,
                audit_retain_ms: *audit_retain_ms,
                minimum_audit_retain_ms: *minimum_audit_retain_ms,
                tenant_id,
                ctx,
            },
        )),

        SqlPlan::DropArray { name, if_exists } => Some(
            super::super::array_convert::convert_drop_array(name, *if_exists, tenant_id, ctx),
        ),

        SqlPlan::AlterArray {
            name,
            audit_retain_ms,
            minimum_audit_retain_ms,
        } => Some(super::super::array_alter_convert::convert_alter_array(
            name,
            *audit_retain_ms,
            *minimum_audit_retain_ms,
            tenant_id,
            ctx,
        )),

        SqlPlan::InsertArray { name, rows } => Some(
            super::super::array_convert::convert_insert_array(name, rows, tenant_id, ctx),
        ),

        SqlPlan::DeleteArray { name, coords } => Some(
            super::super::array_convert::convert_delete_array(name, coords, tenant_id, ctx),
        ),

        SqlPlan::ArraySlice {
            name,
            slice,
            attr_projection,
            limit,
            temporal,
        } => Some(super::super::array_fn_convert::convert_slice(
            name,
            slice,
            attr_projection,
            *limit,
            *temporal,
            tenant_id,
            ctx,
        )),

        SqlPlan::ArrayProject {
            name,
            attr_projection,
        } => Some(super::super::array_fn_convert::convert_project(
            name,
            attr_projection,
            tenant_id,
            ctx,
        )),

        SqlPlan::ArrayAgg {
            name,
            attr,
            reducer,
            group_by_dim,
            temporal,
        } => Some(super::super::array_fn_convert::convert_agg(
            name,
            attr,
            *reducer,
            group_by_dim.as_deref(),
            *temporal,
            tenant_id,
            ctx,
        )),

        SqlPlan::ArrayElementwise {
            left,
            right,
            op,
            attr,
        } => Some(super::super::array_fn_convert::convert_elementwise(
            left, right, *op, attr, tenant_id, ctx,
        )),

        SqlPlan::ArrayFlush { name } => Some(super::super::array_fn_convert::convert_flush(
            name, tenant_id, ctx,
        )),

        SqlPlan::ArrayCompact { name } => Some(super::super::array_fn_convert::convert_compact(
            name, tenant_id, ctx,
        )),

        _ => None,
    }
}
