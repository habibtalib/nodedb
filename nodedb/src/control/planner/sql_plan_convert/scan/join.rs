// SPDX-License-Identifier: BUSL-1.1

//! Hash-join converter and the filter/condition merger and bitmap-hint plan
//! synthesis it depends on.

use nodedb_sql::planner::bitmap_emit::predicate::BitmapHint;
use nodedb_sql::types::{Filter, SqlPlan};

use crate::bridge::envelope::PhysicalPlan;
use crate::bridge::physical_plan::*;
use crate::types::{DatabaseId, VShardId};

use super::super::super::physical::{PhysicalTask, PostSetOp};
use super::super::aggregate::{
    extract_collection_name, extract_join_projection_specs, extract_scan_alias,
};
use super::super::convert::convert_one;
use super::super::filter::{expr_filter_qualified, serialize_filters};
use super::super::scan_params::JoinPlanParams;
use super::super::value::sql_value_to_string;

/// Serialize WHERE filters + non-equi join condition into a single `Vec<u8>`.
///
/// The non-equi condition (from the ON clause) is appended as a
/// `FilterOp::Expr` ScanFilter so the join executor evaluates it on
/// merged rows alongside any post-join WHERE filters.
fn serialize_join_filters(
    filters: &[Filter],
    condition: &Option<nodedb_sql::types::SqlExpr>,
) -> crate::Result<Vec<u8>> {
    match condition {
        None => serialize_filters(filters),
        Some(cond) => {
            let mut scan_filters: Vec<nodedb_query::scan_filter::ScanFilter> =
                if !filters.is_empty() {
                    let base = serialize_filters(filters)?;
                    if base.is_empty() {
                        Vec::new()
                    } else {
                        zerompk::from_msgpack(&base).unwrap_or_default()
                    }
                } else {
                    Vec::new()
                };
            scan_filters.push(expr_filter_qualified(cond));
            zerompk::to_msgpack_vec(&scan_filters).map_err(|e| crate::Error::Serialization {
                format: "msgpack".into(),
                detail: format!("join filter serialization: {e}"),
            })
        }
    }
}

/// Build a `PhysicalPlan` bitmap-producer sub-plan from a `BitmapHint`.
///
/// Returns `None` for hint shapes that cannot be represented as an
/// `IndexedFetch` (e.g. non-string primary values that have no reasonable
/// index-path encoding). The caller treats `None` as "no bitmap pushdown".
fn bitmap_hint_to_plan(hint: &BitmapHint, database_id: DatabaseId) -> Option<Box<PhysicalPlan>> {
    if !hint.extra_values.is_empty() {
        return None;
    }
    let collection = super::super::convert::db_qualified(database_id, &hint.collection);
    let value_str = sql_value_to_string(&hint.primary_value);
    Some(Box::new(PhysicalPlan::Document(DocumentOp::IndexedFetch {
        collection,
        path: hint.field.clone(),
        value: value_str,
        filters: Vec::new(),
        projection: Vec::new(),
        limit: 10_000,
        offset: 0,
    })))
}

pub(in crate::control::planner::sql_plan_convert) fn convert_join(
    p: JoinPlanParams<'_>,
) -> crate::Result<Vec<PhysicalTask>> {
    let JoinPlanParams {
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
    } = p;
    let mut left_collection =
        super::super::convert::db_qualified(p.ctx.database_id, &extract_collection_name(left));
    let mut right_collection =
        super::super::convert::db_qualified(p.ctx.database_id, &extract_collection_name(right));
    let mut left_alias = extract_scan_alias(left);
    let mut right_alias = extract_scan_alias(right);
    let join_projection = extract_join_projection_specs(projection);
    let filter_bytes = serialize_join_filters(filters, condition)?;

    // Check if the left side is a nested join (multi-way join).
    // If so, convert the inner join to a physical plan and pass it
    // as `inline_left` so the executor runs it first.
    let inline_left = if matches!(left, SqlPlan::Join { .. }) {
        let inner_tasks = convert_one(left, tenant_id, ctx)?;
        inner_tasks.into_iter().next().map(|t| Box::new(t.plan))
    } else {
        None
    };
    let inline_right = super::super::aggregate::inline_join_side(right, tenant_id, ctx)?;

    // RIGHT JOIN → swap sides and convert to LEFT JOIN.
    let mut on_keys = on.to_vec();
    let mut inline_left = inline_left;
    let mut inline_right = inline_right;
    let effective_join_type = if join_type.as_str() == "right" {
        std::mem::swap(&mut left_collection, &mut right_collection);
        std::mem::swap(&mut left_alias, &mut right_alias);
        std::mem::swap(&mut inline_left, &mut inline_right);
        on_keys = on_keys.into_iter().map(|(l, r)| (r, l)).collect();
        "left".to_string()
    } else {
        join_type.as_str().to_string()
    };

    // Analyze join children for selective-predicate bitmap pushdown.
    // The analysis runs on the *original* (pre-swap) children since it inspects
    // SqlPlan shape. After the RIGHT→LEFT swap, we swap the resulting hints too.
    let bitmap_hints = nodedb_sql::planner::bitmap_emit::hashjoin::analyze_join_sides(left, right);
    let (mut raw_left_bm, mut raw_right_bm) = (bitmap_hints.left, bitmap_hints.right);
    if join_type.as_str() == "right" {
        std::mem::swap(&mut raw_left_bm, &mut raw_right_bm);
    }
    let db_id = p.ctx.database_id;
    let inline_left_bitmap = raw_left_bm.and_then(|h| bitmap_hint_to_plan(&h, db_id));
    let inline_right_bitmap = raw_right_bm.and_then(|h| bitmap_hint_to_plan(&h, db_id));

    let vshard = VShardId::from_collection_in_database(p.ctx.database_id, &left_collection);

    Ok(vec![PhysicalTask {
        tenant_id,
        vshard_id: vshard,
        database_id: p.ctx.database_id,
        plan: PhysicalPlan::Query(QueryOp::HashJoin {
            left_collection,
            right_collection,
            left_alias,
            right_alias,
            on: on_keys,
            join_type: effective_join_type,
            limit: *limit,
            post_group_by: Vec::new(),
            post_aggregates: Vec::new(),
            projection: join_projection,
            post_filters: filter_bytes,
            inline_left,
            inline_right,
            inline_left_bitmap,
            inline_right_bitmap,
        }),
        post_set_op: PostSetOp::None,
    }])
}
