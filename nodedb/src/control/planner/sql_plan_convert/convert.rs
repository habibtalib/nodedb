//! Convert nodedb-sql SqlPlan IR to NodeDB PhysicalPlan + PhysicalTask.
//!
//! This is the Origin-specific mapping layer. It adds vShard routing,
//! serializes filters to msgpack, and handles broadcast join decisions.

use nodedb_sql::types::SqlPlan;

use std::sync::Arc;

use crate::control::array_catalog::ArrayCatalogHandle;
use crate::control::security::credential::CredentialStore;
use crate::control::surrogate::SurrogateAssigner;
use crate::engine::bitemporal::BitemporalRetentionRegistry;
use crate::engine::timeseries::retention_policy::RetentionPolicyRegistry;
use crate::types::TenantId;
use crate::wal::WalManager;

use super::super::physical::PhysicalTask;
use convert_array_arms::convert_array_plans;

#[path = "convert_array_arms.rs"]
mod convert_array_arms;

/// Conversion context holding optional references needed during plan conversion.
pub struct ConvertContext {
    pub retention_registry: Option<Arc<RetentionPolicyRegistry>>,
    /// Array DDL/DML targets — when `None`, array statements fail with a
    /// deterministic error so converters used by sub-planners (which do
    /// not own array state) cannot accidentally mutate the catalog.
    pub array_catalog: Option<ArrayCatalogHandle>,
    /// Used by `SqlPlan::CreateArray` / `DropArray` to persist or
    /// remove `_system.arrays` rows.
    pub credentials: Option<Arc<CredentialStore>>,
    /// LSN allocator for array Put/Delete dispatches.
    pub wal: Option<Arc<WalManager>>,
    /// CP-side surrogate assigner — bound to the same `Arc` held on
    /// `SharedState`. Threaded into INSERT/UPSERT/KV-INSERT converters
    /// to bind `(collection, pk_bytes)` → `Surrogate` before the op
    /// crosses the SPSC bridge. `None` only for converters used by
    /// sub-planners that never lower to the surrogate-bearing variants
    /// (e.g. CREATE/DROP/ARRAY paths).
    pub surrogate_assigner: Option<Arc<SurrogateAssigner>>,
    /// `true` when the node is running in cluster mode with a live
    /// topology. Array DML/query converters emit `ClusterArray` variants
    /// when this flag is set; single-node mode emits local `Array` variants.
    pub cluster_enabled: bool,
    /// Bitemporal retention registry — required by `ALTER ARRAY` to
    /// update the purge-scheduler's view of the array's retention policy.
    /// `None` for sub-planners that don't own array DDL.
    pub bitemporal_retention_registry: Option<Arc<BitemporalRetentionRegistry>>,
}

/// Convert a list of SqlPlans to PhysicalTasks.
pub fn convert(
    plans: &[SqlPlan],
    tenant_id: TenantId,
    ctx: &ConvertContext,
) -> crate::Result<Vec<PhysicalTask>> {
    let mut tasks = Vec::new();
    for plan in plans {
        tasks.extend(convert_one(plan, tenant_id, ctx)?);
    }
    Ok(tasks)
}

pub(super) fn convert_one(
    plan: &SqlPlan,
    tenant_id: TenantId,
    ctx: &ConvertContext,
) -> crate::Result<Vec<PhysicalTask>> {
    // Delegate array plans first to keep this match manageable.
    if let Some(result) = convert_array_plans(plan, tenant_id, ctx) {
        return result;
    }

    match plan {
        SqlPlan::ConstantResult { columns, values } => {
            super::set_ops::convert_constant_result(columns, values, tenant_id)
        }

        SqlPlan::Scan {
            collection,
            alias: _,
            engine,
            filters,
            projection,
            sort_keys,
            limit,
            offset,
            distinct,
            window_functions,
            temporal,
        } => super::scan::convert_scan(super::scan_params::ScanParams {
            collection,
            engine,
            filters,
            projection,
            sort_keys,
            limit,
            offset,
            distinct,
            window_functions,
            tenant_id,
            temporal,
        }),

        SqlPlan::PointGet {
            collection,
            alias: _,
            engine,
            key_column,
            key_value,
        } => super::scan::convert_point_get(
            collection, engine, key_column, key_value, tenant_id, ctx,
        ),

        SqlPlan::DocumentIndexLookup {
            collection,
            alias: _,
            engine: _,
            field,
            value,
            filters,
            projection,
            sort_keys: _,
            limit,
            offset,
            distinct: _,
            window_functions: _,
            case_insensitive: _,
            temporal: _,
        } => super::scan::convert_document_index_lookup(
            collection, field, value, filters, projection, *limit, *offset, tenant_id,
        ),

        SqlPlan::Insert {
            collection,
            engine,
            rows,
            column_defaults,
            if_absent,
        } => super::dml::convert_insert(
            collection,
            engine,
            rows,
            column_defaults,
            *if_absent,
            tenant_id,
            ctx,
        ),

        SqlPlan::Upsert {
            collection,
            engine,
            rows,
            column_defaults,
            on_conflict_updates,
        } => super::dml::convert_upsert(
            collection,
            engine,
            rows,
            column_defaults,
            on_conflict_updates,
            tenant_id,
            ctx,
        ),

        SqlPlan::KvInsert {
            collection,
            entries,
            ttl_secs,
            intent,
            on_conflict_updates,
        } => super::dml::convert_kv_insert(
            collection,
            entries,
            *ttl_secs,
            *intent,
            on_conflict_updates,
            tenant_id,
            ctx,
        ),

        SqlPlan::Update {
            collection,
            engine,
            assignments,
            filters,
            target_keys,
            returning,
        } => super::dml::convert_update(
            collection,
            engine,
            assignments,
            filters,
            target_keys,
            *returning,
            tenant_id,
            ctx,
        ),

        SqlPlan::UpdateFrom {
            collection,
            engine: _,
            source,
            target_join_col,
            source_join_col,
            assignments,
            target_filters,
            returning,
        } => super::dml::convert_update_from(
            collection,
            source,
            target_join_col,
            source_join_col,
            assignments,
            target_filters,
            *returning,
            tenant_id,
        ),

        SqlPlan::Delete {
            collection,
            engine,
            filters,
            target_keys,
        } => super::dml::convert_delete(collection, engine, filters, target_keys, tenant_id, ctx),

        SqlPlan::Truncate {
            collection,
            restart_identity,
        } => super::set_ops::convert_truncate(collection, *restart_identity, tenant_id),

        SqlPlan::Join {
            left,
            right,
            on,
            join_type,
            condition,
            limit,
            projection,
            filters,
        } => super::scan::convert_join(super::scan_params::JoinPlanParams {
            left,
            right,
            on,
            join_type,
            condition,
            limit,
            projection,
            filters,
            tenant_id,
            ctx,
        }),

        SqlPlan::Aggregate {
            input,
            group_by,
            aggregates,
            having,
            limit,
            grouping_sets,
        } => super::aggregate::convert_aggregate(super::aggregate::ConvertAggregateParams {
            input,
            group_by,
            aggregates,
            having,
            limit: *limit,
            grouping_sets: grouping_sets.as_deref(),
            tenant_id,
            ctx,
        }),

        SqlPlan::TimeseriesScan {
            collection,
            time_range,
            bucket_interval_ms,
            group_by,
            aggregates,
            filters,
            projection,
            gap_fill,
            limit,
            tiered,
            temporal,
        } => super::scan::convert_timeseries_scan(super::scan_params::TimeseriesScanParams {
            collection,
            time_range,
            bucket_interval_ms,
            group_by,
            aggregates,
            filters,
            projection,
            gap_fill,
            limit,
            tiered,
            tenant_id,
            ctx,
            temporal,
        }),

        SqlPlan::TimeseriesIngest { collection, rows } => {
            super::scan::convert_timeseries_ingest(collection, rows, tenant_id, ctx)
        }

        SqlPlan::VectorSearch {
            collection,
            field,
            query_vector,
            top_k,
            ef_search,
            metric,
            filters,
            array_prefilter,
            ann_options,
            skip_payload_fetch,
            payload_filters,
        } => super::scan::convert_vector_search(super::scan_params::VectorSearchParams {
            collection,
            field,
            query_vector,
            top_k,
            ef_search,
            metric,
            filters,
            array_prefilter: array_prefilter.as_ref(),
            ann_options,
            tenant_id,
            ctx,
            skip_payload_fetch: *skip_payload_fetch,
            payload_filters,
        }),

        SqlPlan::TextSearch {
            collection,
            query,
            top_k,
            score_alias,
            ..
        } => super::scan::convert_text_search(
            collection,
            query,
            top_k,
            score_alias.as_deref(),
            tenant_id,
        ),

        SqlPlan::HybridSearch {
            collection,
            query_vector,
            query_text,
            top_k,
            ef_search,
            vector_weight,
            fuzzy,
            score_alias,
        } => super::scan::convert_hybrid_search(super::scan_params::HybridSearchParams {
            collection,
            query_vector,
            query_text,
            top_k,
            ef_search,
            vector_weight,
            fuzzy,
            score_alias: score_alias.as_deref(),
            tenant_id,
        }),

        SqlPlan::HybridSearchTriple {
            collection,
            query_vector,
            query_text,
            graph_seed_id,
            graph_depth,
            graph_edge_label,
            top_k,
            ef_search,
            fuzzy,
            rrf_k,
            score_alias,
        } => super::scan::convert_hybrid_search_triple(
            super::scan_params::HybridSearchTripleParams {
                collection,
                query_vector,
                query_text,
                graph_seed_id,
                graph_depth,
                graph_edge_label,
                top_k,
                ef_search,
                fuzzy,
                rrf_k,
                score_alias: score_alias.as_deref(),
                tenant_id,
            },
        ),

        SqlPlan::SpatialScan {
            collection,
            field,
            predicate,
            query_geometry,
            distance_meters,
            attribute_filters,
            limit,
            projection,
        } => super::scan::convert_spatial_scan(super::scan_params::SpatialScanParams {
            collection,
            field,
            predicate,
            query_geometry,
            distance_meters,
            attribute_filters,
            limit,
            projection,
            tenant_id,
        }),

        SqlPlan::Union { inputs, distinct } => {
            super::set_ops::convert_union(inputs, *distinct, tenant_id, ctx)
        }

        SqlPlan::Intersect { left, right, all } => {
            super::set_ops::convert_intersect(left, right, *all, tenant_id, ctx)
        }

        SqlPlan::Except { left, right, all } => {
            super::set_ops::convert_except(left, right, *all, tenant_id, ctx)
        }

        SqlPlan::InsertSelect { target, source, .. } => {
            super::set_ops::convert_insert_select(target, source, tenant_id, ctx)
        }

        SqlPlan::RecursiveScan {
            collection,
            base_filters,
            recursive_filters,
            join_link,
            max_iterations,
            distinct,
            limit,
        } => super::scan::convert_recursive_scan(super::scan_params::RecursiveScanParams {
            collection,
            base_filters,
            recursive_filters,
            join_link,
            max_iterations,
            distinct,
            limit,
            tenant_id,
        }),

        SqlPlan::Cte { definitions, outer } => {
            super::set_ops::convert_cte(definitions, outer, tenant_id, ctx)
        }

        SqlPlan::VectorPrimaryInsert {
            collection,
            field,
            quantization,
            payload_indexes,
            rows,
        } => super::dml::convert_vector_primary_insert(
            collection,
            field,
            *quantization,
            payload_indexes,
            rows,
            tenant_id,
            ctx,
        ),

        SqlPlan::Merge {
            target,
            engine: _,
            source,
            target_join_col,
            source_join_col,
            source_alias,
            clauses,
            returning,
        } => super::dml::convert_merge(
            target,
            source,
            target_join_col,
            source_join_col,
            source_alias,
            clauses,
            *returning,
            tenant_id,
        ),

        SqlPlan::LateralTopK {
            outer,
            outer_alias,
            inner_collection,
            inner_filters,
            inner_order_by,
            inner_limit,
            correlation_keys,
            lateral_alias,
            projection,
            left_join,
        } => super::lateral::convert_lateral_top_k(
            outer,
            outer_alias.as_deref(),
            inner_collection,
            inner_filters,
            inner_order_by,
            *inner_limit,
            correlation_keys,
            lateral_alias,
            projection,
            *left_join,
            tenant_id,
            ctx,
        ),

        SqlPlan::LateralLoop {
            outer,
            outer_alias,
            inner,
            correlation_predicates,
            lateral_alias,
            projection,
            outer_row_cap,
            left_join,
        } => super::lateral::convert_lateral_loop(
            outer,
            outer_alias.as_deref(),
            inner,
            correlation_predicates,
            lateral_alias,
            projection,
            *outer_row_cap,
            *left_join,
            tenant_id,
            ctx,
        ),

        SqlPlan::MultiVectorSearch { .. } | SqlPlan::RangeScan { .. } => {
            Err(crate::Error::PlanError {
                detail: format!("unsupported SqlPlan variant: {plan:?}"),
            })
        }

        // Array arms are handled above by `convert_array_plans`.
        // This catch-all handles any future array-related variants that
        // haven't been added to `convert_array_arms.rs` yet.
        _ => Err(crate::Error::PlanError {
            detail: format!("unhandled SqlPlan variant: {plan:?}"),
        }),
    }
}
