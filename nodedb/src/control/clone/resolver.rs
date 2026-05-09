// SPDX-License-Identifier: BUSL-1.1

//! Copy-on-write read resolution algorithm.
//!
//! For reads targeting a `Shadowed` or `Materializing` clone, this module
//! produces an augmented task list: one task for the target database (post-clone
//! writes) and one task for the source database (source rows at
//! `effective_source_lsn`).  Both tasks are dispatched by the caller using the
//! normal SPSC path; the results are then merged via `merge_clone_responses`.
//!
//! Non-cloned databases and `Materialized` clones return the original task list
//! unchanged — zero overhead.

use std::sync::Arc;

use nodedb_types::{CloneOrigin, CloneStatus, DatabaseId, Lsn, TenantId};

use crate::bridge::physical_plan::{ColumnarOp, DocumentOp, KvOp, PhysicalPlan, TimeseriesOp};
use crate::control::planner::physical::PhysicalTask;
use crate::control::state::SharedState;
use crate::types::VShardId;

use super::metadata::ClonePredicatesNote;

/// Parameters for the clone read resolver.
pub struct CloneReadParams {
    /// The LSN at which the query runs (T_lsn).
    pub query_lsn: Lsn,
    /// Wall-clock milliseconds corresponding to `query_lsn` (for engine
    /// `system_as_of_ms` fields that work in millisecond space).
    pub query_ms: Option<i64>,
}

/// Outcome of attempting to resolve clone reads for a set of tasks.
pub enum ResolveOutcome {
    /// Tasks were not modified — either the collection is not a clone, or
    /// it is fully `Materialized`.
    Passthrough(Vec<PhysicalTask>),
    /// The query time predates the clone's creation — return empty + note.
    PreDatesClone(ClonePredicatesNote),
    /// Augmented tasks: [target_tasks..., source_tasks...].
    ///
    /// The caller dispatches all tasks and then calls `merge_source_into_target`
    /// to filter tombstoned rows before returning results to the client.
    Augmented {
        tasks: Vec<PhysicalTask>,
        /// Index into `tasks` where source tasks begin.
        source_start_idx: usize,
        /// Clone metadata for result filtering.
        origin: CloneOrigin,
        /// Collection key for tombstone lookups, e.g. `"1/users"`.
        target_collection_key: String,
        /// Clone predation note, `None` unless `T_lsn < clone_created_at`.
        note: Option<ClonePredicatesNote>,
    },
}

/// Attempt to resolve `tasks` for a cloned collection.
///
/// Returns `None` when the collection has no clone origin (fast path: zero
/// overhead). Returns `Some(ResolveOutcome)` when resolution is required.
pub fn resolve_read(
    state: &Arc<SharedState>,
    tasks: Vec<PhysicalTask>,
    tenant_id: TenantId,
    params: &CloneReadParams,
) -> crate::Result<Option<ResolveOutcome>> {
    // Quick-check: do any tasks target a database other than the default?
    // All tasks in a statement share the same database_id (single-database
    // statements); use the first task's database_id.
    let Some(first_task) = tasks.first() else {
        return Ok(None);
    };
    let db_id = first_task.database_id;

    // Retrieve catalog for lookup.
    let catalog_arc = state.credentials.catalog();
    let Some(catalog) = catalog_arc.as_ref() else {
        return Ok(None);
    };

    // Extract the collection name from the first read-type task.
    let Some(raw_coll) = extract_collection_from_plan(&first_task.plan) else {
        return Ok(None);
    };
    // Strip the database prefix that db_qualified() prepends, e.g. "1/users" → "users".
    let coll_name = strip_db_prefix(db_id, raw_coll);

    // Look up the stored collection descriptor.
    let Some(desc) = catalog
        .get_collection(db_id, tenant_id.as_u64(), coll_name)
        .map_err(|e| crate::Error::Storage {
            engine: "catalog".into(),
            detail: format!("clone resolver: get_collection failed: {e}"),
        })?
    else {
        return Ok(None);
    };

    // Short-circuit: not a clone or fully materialized.
    let Some(ref origin) = desc.cloned_from else {
        return Ok(None);
    };
    match desc.clone_status {
        CloneStatus::Materialized => return Ok(None),
        CloneStatus::Shadowed | CloneStatus::Materializing { .. } => {}
    }

    // Bitemporal correctness: check if T_lsn < clone_created_at.
    if params.query_lsn < origin.clone_created_at {
        return Ok(Some(ResolveOutcome::PreDatesClone(
            ClonePredicatesNote::new(params.query_lsn, origin.clone_created_at),
        )));
    }

    // Compute effective source LSN: min(T_lsn, as_of_lsn).
    let effective_source_lsn = if params.query_lsn > origin.as_of_lsn {
        origin.as_of_lsn
    } else {
        params.query_lsn
    };

    // Convert effective_source_lsn to wall-ms for the engine.
    let effective_source_ms = state.ms_to_lsn_inverse(effective_source_lsn);

    // Build source-side tasks (same plans but with source database_id and
    // effective_source_lsn encoded as system_as_of_ms).
    let source_db_id = origin.source_database;
    let source_coll_name = origin.source_collection.as_str();

    let mut augmented_tasks = tasks.clone();
    let source_start_idx = augmented_tasks.len();

    for task in &tasks {
        if let Some(source_plan) = rewrite_plan_for_source(
            &task.plan,
            db_id,
            source_db_id,
            coll_name,
            source_coll_name,
            effective_source_ms,
        ) {
            let source_vshard = VShardId::from_collection_in_database(
                source_db_id,
                &crate::control::planner::sql_plan_convert::convert::db_qualified(
                    source_db_id,
                    source_coll_name,
                ),
            );
            augmented_tasks.push(PhysicalTask {
                tenant_id,
                vshard_id: source_vshard,
                database_id: source_db_id,
                plan: source_plan,
                post_set_op: crate::control::planner::physical::PostSetOp::None,
            });
        }
    }

    let target_collection_key =
        crate::control::planner::sql_plan_convert::convert::db_qualified(db_id, coll_name);

    Ok(Some(ResolveOutcome::Augmented {
        tasks: augmented_tasks,
        source_start_idx,
        origin: origin.clone(),
        target_collection_key,
        note: None,
    }))
}

/// Rewrite a `PhysicalPlan` to target the source database and collection at
/// the effective source LSN.  Returns `None` for plan types that are not
/// read-type operations (writes, DDL).
fn rewrite_plan_for_source(
    plan: &PhysicalPlan,
    target_db_id: DatabaseId,
    source_db_id: DatabaseId,
    target_coll: &str,
    source_coll: &str,
    effective_source_ms: Option<i64>,
) -> Option<PhysicalPlan> {
    use crate::control::planner::sql_plan_convert::convert::db_qualified;

    let target_qualified = db_qualified(target_db_id, target_coll);
    let source_qualified = db_qualified(source_db_id, source_coll);

    match plan {
        PhysicalPlan::Document(DocumentOp::Scan {
            collection,
            limit,
            offset,
            sort_keys,
            filters,
            distinct,
            projection,
            computed_columns,
            window_functions,
            system_as_of_ms,
            valid_at_ms,
            prefilter,
        }) if collection == &target_qualified => Some(PhysicalPlan::Document(DocumentOp::Scan {
            collection: source_qualified,
            limit: *limit,
            offset: *offset,
            sort_keys: sort_keys.clone(),
            filters: filters.clone(),
            distinct: *distinct,
            projection: projection.clone(),
            computed_columns: computed_columns.clone(),
            window_functions: window_functions.clone(),
            system_as_of_ms: effective_source_ms.or(*system_as_of_ms),
            valid_at_ms: *valid_at_ms,
            prefilter: prefilter.clone(),
        })),

        PhysicalPlan::Kv(KvOp::Scan {
            collection,
            cursor,
            count,
            filters,
            match_pattern,
            sort_keys,
        }) if collection == &target_qualified => Some(PhysicalPlan::Kv(KvOp::Scan {
            collection: source_qualified,
            cursor: cursor.clone(),
            count: *count,
            filters: filters.clone(),
            match_pattern: match_pattern.clone(),
            sort_keys: sort_keys.clone(),
        })),

        PhysicalPlan::Kv(KvOp::Get {
            collection,
            key,
            rls_filters,
        }) if collection == &target_qualified => Some(PhysicalPlan::Kv(KvOp::Get {
            collection: source_qualified,
            key: key.clone(),
            rls_filters: rls_filters.clone(),
        })),

        PhysicalPlan::Columnar(ColumnarOp::Scan {
            collection,
            projection,
            limit,
            filters,
            rls_filters,
            sort_keys,
            system_as_of_ms,
            valid_at_ms,
            prefilter,
        }) if collection == &target_qualified => Some(PhysicalPlan::Columnar(ColumnarOp::Scan {
            collection: source_qualified,
            projection: projection.clone(),
            limit: *limit,
            filters: filters.clone(),
            rls_filters: rls_filters.clone(),
            sort_keys: sort_keys.clone(),
            system_as_of_ms: effective_source_ms.or(*system_as_of_ms),
            valid_at_ms: *valid_at_ms,
            prefilter: prefilter.clone(),
        })),

        PhysicalPlan::Timeseries(TimeseriesOp::Scan {
            collection,
            time_range,
            projection,
            limit,
            filters,
            bucket_interval_ms,
            group_by,
            aggregates,
            gap_fill,
            computed_columns,
            rls_filters,
            system_as_of_ms,
            valid_at_ms,
        }) if collection == &target_qualified => {
            Some(PhysicalPlan::Timeseries(TimeseriesOp::Scan {
                collection: source_qualified,
                time_range: *time_range,
                projection: projection.clone(),
                limit: *limit,
                filters: filters.clone(),
                bucket_interval_ms: *bucket_interval_ms,
                group_by: group_by.clone(),
                aggregates: aggregates.clone(),
                gap_fill: gap_fill.clone(),
                computed_columns: computed_columns.clone(),
                rls_filters: rls_filters.clone(),
                system_as_of_ms: effective_source_ms.or(*system_as_of_ms),
                valid_at_ms: *valid_at_ms,
            }))
        }

        // Write operations, DDL, and all other engine plans are not replicated
        // to the source — they execute against the target only.  Exhaustively
        // listing every PhysicalPlan top-level variant ensures the compiler
        // catches any newly added variants.
        PhysicalPlan::Document(_)
        | PhysicalPlan::Kv(_)
        | PhysicalPlan::Vector(_)
        | PhysicalPlan::Graph(_)
        | PhysicalPlan::Text(_)
        | PhysicalPlan::Columnar(_)
        | PhysicalPlan::Timeseries(_)
        | PhysicalPlan::Spatial(_)
        | PhysicalPlan::Crdt(_)
        | PhysicalPlan::Query(_)
        | PhysicalPlan::Meta(_)
        | PhysicalPlan::Array(_)
        | PhysicalPlan::ClusterArray(_) => None,
    }
}

/// Extract the raw (db_qualified) collection name from a plan.
fn extract_collection_from_plan(plan: &PhysicalPlan) -> Option<&str> {
    match plan {
        PhysicalPlan::Document(DocumentOp::Scan { collection, .. }) => Some(collection),
        PhysicalPlan::Document(DocumentOp::PointGet { collection, .. }) => Some(collection),
        PhysicalPlan::Document(DocumentOp::PointPut { collection, .. }) => Some(collection),
        PhysicalPlan::Document(DocumentOp::PointUpdate { collection, .. }) => Some(collection),
        PhysicalPlan::Document(DocumentOp::PointDelete { collection, .. }) => Some(collection),
        PhysicalPlan::Document(DocumentOp::PointInsert { collection, .. }) => Some(collection),
        PhysicalPlan::Kv(KvOp::Scan { collection, .. }) => Some(collection),
        PhysicalPlan::Kv(KvOp::Get { collection, .. }) => Some(collection),
        PhysicalPlan::Columnar(ColumnarOp::Scan { collection, .. }) => Some(collection),
        PhysicalPlan::Timeseries(TimeseriesOp::Scan { collection, .. }) => Some(collection),
        // All other plan types do not carry a collection in a form the clone
        // resolver needs to inspect.  Exhaustively listing every top-level
        // PhysicalPlan variant here ensures the compiler reports new variants.
        PhysicalPlan::Document(_)
        | PhysicalPlan::Kv(_)
        | PhysicalPlan::Vector(_)
        | PhysicalPlan::Graph(_)
        | PhysicalPlan::Text(_)
        | PhysicalPlan::Columnar(_)
        | PhysicalPlan::Timeseries(_)
        | PhysicalPlan::Spatial(_)
        | PhysicalPlan::Crdt(_)
        | PhysicalPlan::Query(_)
        | PhysicalPlan::Meta(_)
        | PhysicalPlan::Array(_)
        | PhysicalPlan::ClusterArray(_) => None,
    }
}

/// Strip the `"<db_id>/"` prefix added by `db_qualified()`, returning the
/// bare collection name.  If the collection was stored without a prefix
/// (default database, id == 0), the string is returned as-is.
fn strip_db_prefix(db_id: DatabaseId, qualified: &str) -> &str {
    if db_id == DatabaseId::DEFAULT {
        return qualified;
    }
    let prefix = format!("{}/", db_id.as_u64());
    if let Some(stripped) = qualified.strip_prefix(prefix.as_str()) {
        stripped
    } else {
        qualified
    }
}

/// Filter tombstoned source surrogates from response bytes.
///
/// Given the raw msgpack payload from a source scan, returns a filtered
/// payload that excludes rows whose surrogates are in `tombstoned`.
/// If `tombstoned` is empty this is a no-op and returns `None` (caller
/// keeps the original bytes).
pub fn filter_tombstoned_rows(
    payload: &[u8],
    tombstoned: &std::collections::HashSet<u32>,
) -> Option<Vec<u8>> {
    use nodedb_query::msgpack_scan;

    if tombstoned.is_empty() || payload.is_empty() {
        return None;
    }

    let (count, mut offset) = msgpack_scan::array_header(payload, 0)?;
    let mut kept: Vec<&[u8]> = Vec::with_capacity(count);

    for _ in 0..count {
        let entry_start = offset;
        // The "id" field is the surrogate in hex (e.g., "0000001a").
        let surrogate_opt = msgpack_scan::extract_field(payload, offset, "id")
            .and_then(|(s, _)| msgpack_scan::read_str(payload, s))
            .and_then(|id_str| u32::from_str_radix(id_str, 16).ok());

        let next = msgpack_scan::skip_value(payload, offset)?;
        offset = next;

        if surrogate_opt.is_some_and(|s| tombstoned.contains(&s)) {
            continue; // skip tombstoned row
        }
        kept.push(&payload[entry_start..next]);
    }

    if kept.len() == count {
        return None; // nothing filtered
    }

    // Re-encode as msgpack array.
    Some(encode_msgpack_array(&kept))
}

/// Encode a slice of raw msgpack values as a msgpack array.
fn encode_msgpack_array(items: &[&[u8]]) -> Vec<u8> {
    let count = items.len();
    let mut buf = Vec::with_capacity(items.iter().map(|b| b.len()).sum::<usize>() + 5);

    // msgpack array header.
    if count <= 15 {
        buf.push(0x90 | (count as u8));
    } else if count <= 0xFFFF {
        buf.push(0xdc);
        buf.push((count >> 8) as u8);
        buf.push(count as u8);
    } else {
        buf.push(0xdd);
        buf.push((count >> 24) as u8);
        buf.push((count >> 16) as u8);
        buf.push((count >> 8) as u8);
        buf.push(count as u8);
    }
    for item in items {
        buf.extend_from_slice(item);
    }
    buf
}
