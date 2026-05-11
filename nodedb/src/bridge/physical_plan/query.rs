// SPDX-License-Identifier: BUSL-1.1

//! Query operations (joins, aggregates) dispatched to the Data Plane.

/// Aggregate specification for Data Plane aggregate execution.
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct AggregateSpec {
    pub function: String,
    /// Internal aggregate key used by HAVING and downstream references.
    pub alias: String,
    /// Optional user-facing SQL alias for final output naming.
    pub user_alias: Option<String>,
    /// Field name for simple field-based aggregates. `"*"` is used for COUNT(*).
    pub field: String,
    /// Optional expression to evaluate per-document before aggregating.
    pub expr: Option<crate::bridge::expr_eval::SqlExpr>,
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct JoinProjection {
    pub source: String,
    pub output: String,
}

/// Query-level physical operations (joins, aggregates).
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub enum QueryOp {
    /// Aggregate: GROUP BY + aggregate functions.
    Aggregate {
        collection: String,
        group_by: Vec<String>,
        aggregates: Vec<AggregateSpec>,
        filters: Vec<u8>,
        /// HAVING predicates applied post-aggregation.
        having: Vec<u8>,
        limit: usize,
        sub_group_by: Vec<String>,
        sub_aggregates: Vec<AggregateSpec>,
        /// ROLLUP / CUBE / GROUPING SETS expansion.  Each inner `Vec<u32>` is
        /// one grouping set — the indices into `group_by` that are *present*
        /// (non-NULL) for rows in that set.  Empty outer vec = plain single-set
        /// GROUP BY (no null-filling needed).
        grouping_sets: Vec<Vec<u32>>,
        /// Post-aggregation sort keys: `(column_name, ascending)`.
        /// Empty = preserve executor's natural order (hash-map iteration
        /// for plain GROUP BY). The executor applies the sort after all
        /// groups are finalized and HAVING is filtered.
        #[serde(default)]
        #[msgpack(default)]
        sort_keys: Vec<(String, bool)>,
    },

    /// Partial aggregate: each core computes locally, Control Plane merges.
    PartialAggregate {
        collection: String,
        group_by: Vec<String>,
        aggregates: Vec<AggregateSpec>,
        filters: Vec<u8>,
    },

    /// Hash join: build hash map on right, probe with left.
    HashJoin {
        left_collection: String,
        right_collection: String,
        left_alias: Option<String>,
        right_alias: Option<String>,
        on: Vec<(String, String)>,
        join_type: String,
        limit: usize,
        /// Post-join GROUP BY columns (empty = no aggregation).
        post_group_by: Vec<String>,
        /// Post-join aggregates: (op, field) pairs (empty = no aggregation).
        post_aggregates: Vec<(String, String)>,
        /// Post-join projection: column names to keep (empty = all).
        projection: Vec<JoinProjection>,
        /// Post-join WHERE filter predicates (MessagePack).
        post_filters: Vec<u8>,
        /// Inline left sub-plan for multi-way joins. When set, the executor
        /// runs this sub-plan first and uses its result as the left side
        /// instead of scanning `left_collection`.
        inline_left: Option<Box<crate::bridge::envelope::PhysicalPlan>>,
        /// Inline right sub-plan for scalar subqueries or other materialized
        /// small-side inputs. The Control Plane executes this plan first,
        /// merges it if needed, then embeds the result into `BroadcastJoin`.
        inline_right: Option<Box<crate::bridge::envelope::PhysicalPlan>>,
        /// Bitmap-producer sub-plan for the left side. When set, the executor
        /// executes this plan first, collects surrogates from all returned rows,
        /// and injects the resulting bitmap into the probe-side prefilter before
        /// scanning. `None` = no bitmap pushdown for the left side.
        inline_left_bitmap: Option<Box<crate::bridge::envelope::PhysicalPlan>>,
        /// Bitmap-producer sub-plan for the right side. Same semantics as
        /// `inline_left_bitmap` but applied to the right (probe) collection.
        inline_right_bitmap: Option<Box<crate::bridge::envelope::PhysicalPlan>>,
    },

    /// Inline hash join: both sides are pre-gathered msgpack data.
    /// Used for multi-way joins where the left side is the result of another join.
    InlineHashJoin {
        /// Left side: msgpack array of maps (from inner join result).
        left_data: Vec<u8>,
        /// Right side: raw broadcast scan data.
        right_data: Vec<u8>,
        right_alias: Option<String>,
        on: Vec<(String, String)>,
        join_type: String,
        limit: usize,
        projection: Vec<JoinProjection>,
        post_filters: Vec<u8>,
    },

    /// Broadcast join: small side serialized in the plan.
    BroadcastJoin {
        large_collection: String,
        small_collection: String,
        large_alias: Option<String>,
        small_alias: Option<String>,
        broadcast_data: Vec<u8>,
        on: Vec<(String, String)>,
        join_type: String,
        limit: usize,
        /// Post-join GROUP BY columns (empty = no aggregation).
        post_group_by: Vec<String>,
        /// Post-join aggregates: (op, field) pairs (empty = no aggregation).
        post_aggregates: Vec<(String, String)>,
        /// Post-join projection: column names to keep (empty = all).
        projection: Vec<JoinProjection>,
        /// Post-join WHERE filter predicates (MessagePack).
        post_filters: Vec<u8>,
    },

    /// Shuffle join: repartition by join key via SPSC.
    ShuffleJoin {
        left_collection: String,
        right_collection: String,
        on: Vec<(String, String)>,
        join_type: String,
        limit: usize,
        target_core: usize,
    },

    /// Nested loop join: fallback for non-equi joins.
    NestedLoopJoin {
        left_collection: String,
        right_collection: String,
        /// Join condition as serialized `Vec<ScanFilter>`.
        condition: Vec<u8>,
        join_type: String,
        limit: usize,
    },

    /// Sort-merge join: both sides pre-sorted by join key.
    /// Optimal when both collections have index-ordered scans or
    /// when the planner sorts both sides before joining.
    SortMergeJoin {
        left_collection: String,
        right_collection: String,
        on: Vec<(String, String)>,
        join_type: String,
        limit: usize,
        /// If true, both sides are assumed pre-sorted by join key (skip sort phase).
        pre_sorted: bool,
    },

    /// Multi-facet aggregation: compute facet counts for multiple fields
    /// in a single query, sharing the filter evaluation across all facets.
    FacetCounts {
        collection: String,
        /// Serialized `Vec<ScanFilter>` predicates (MessagePack).
        filters: Vec<u8>,
        /// Field names to facet on (each produces a `[{value, count}]` array).
        fields: Vec<String>,
        /// Maximum number of values to return per facet field (0 = unlimited).
        limit_per_facet: usize,
    },

    /// Recursive CTE: iterative fixed-point execution.
    ///
    /// Executes the base query once, then repeatedly executes the recursive
    /// query using the previous iteration's results as the working table,
    /// until no new rows are produced (fixed point).
    RecursiveScan {
        /// Collection for the recursive scan.
        collection: String,
        /// Base query filters (seeded once).
        base_filters: Vec<u8>,
        /// Recursive step filters (applied to working table each iteration).
        recursive_filters: Vec<u8>,
        /// Equi-join link for tree-traversal recursion:
        /// `(collection_field, working_table_field)`.
        /// Each iteration finds rows where `collection_field` value
        /// matches a `working_table_field` value from the previous iteration.
        join_link: Option<(String, String)>,
        /// Maximum iterations to prevent infinite loops. Default: 100.
        max_iterations: usize,
        /// Whether to deduplicate results (UNION vs UNION ALL).
        distinct: bool,
        limit: usize,
    },

    /// Value-generating recursive CTE: iterative expression evaluation.
    ///
    /// No collection is needed.  The executor evaluates the anchor expressions
    /// once to produce the first row, then repeatedly applies the step
    /// expressions to the previous row until the condition becomes false,
    /// a fixed point is reached, or `max_depth` is exceeded (typed error).
    RecursiveValue {
        /// CTE name (used in error messages).
        cte_name: String,
        /// Column names (length == `init_exprs.len()` == `step_exprs.len()`).
        columns: Vec<String>,
        /// Anchor SELECT expressions as raw SQL text.
        init_exprs: Vec<String>,
        /// Recursive step SELECT expressions as raw SQL text.
        step_exprs: Vec<String>,
        /// Optional WHERE condition as raw SQL text.
        condition: Option<String>,
        /// Maximum iterations before returning a depth-exceeded error.
        max_depth: usize,
        /// Whether to deduplicate (UNION vs UNION ALL).
        distinct: bool,
    },

    /// LATERAL equi-correlated top-K: scan `inner_collection` once per outer
    /// row, applying the equi-correlation as an equality filter, then return
    /// the top `inner_limit` rows ordered by `inner_order_by`.
    ///
    /// The executor first runs `outer_plan` to materialise outer rows, then
    /// for each outer row injects the equi-correlation as an equality filter
    /// on `inner_collection`, applies `inner_order_by`, and keeps the top
    /// `inner_limit` rows.  Output rows are `(outer_row merged with inner_row)`.
    ///
    /// `correlation_keys` are `(outer_col, inner_col)` equi-join pairs.
    LateralTopK {
        /// Sub-plan that produces the outer (driving) rows.
        outer_plan: Box<crate::bridge::envelope::PhysicalPlan>,
        /// Alias qualifying the outer columns in output rows.
        outer_alias: String,
        /// Inner collection to scan per outer row.
        inner_collection: String,
        /// Non-correlated filters applied to every inner scan (msgpack bytes).
        inner_filters: Vec<u8>,
        /// Sort keys for the inner per-outer-row result.
        /// Each entry is `(field_name, ascending)`.
        inner_order_by: Vec<(String, bool)>,
        /// Maximum inner rows per outer row.
        inner_limit: usize,
        /// Equi-join pairs `(outer_col, inner_col)`.
        correlation_keys: Vec<(String, String)>,
        /// Alias qualifying inner columns in output rows.
        lateral_alias: String,
        /// Output projection (empty = all columns).
        projection: Vec<JoinProjection>,
        /// LEFT join semantics: preserve outer rows when inner is empty.
        left_join: bool,
    },

    /// LATERAL nested-loop: for each outer row, re-execute the inner plan
    /// with correlated values injected as additional equality filters.
    ///
    /// The executor runs `outer_plan`, then for each outer row reads the
    /// `outer_col` values from `correlation_predicates`, appends equality
    /// filters on the corresponding `inner_col` fields, and scans
    /// `inner_collection`.
    LateralLoop {
        /// Sub-plan that produces the outer (driving) rows.
        outer_plan: Box<crate::bridge::envelope::PhysicalPlan>,
        /// Alias qualifying the outer columns in output rows.
        outer_alias: String,
        /// Inner collection to scan per outer row.
        inner_collection: String,
        /// Base inner filters (non-correlated, msgpack bytes).
        inner_filters: Vec<u8>,
        /// Correlated predicates: `(inner_field, outer_field)`.
        correlation_predicates: Vec<(String, String)>,
        /// Alias qualifying inner columns in output rows.
        lateral_alias: String,
        /// Output projection (empty = all columns).
        projection: Vec<JoinProjection>,
        /// LEFT join semantics.
        left_join: bool,
        /// Hard cap on outer rows. Queries that exceed this cap return a
        /// typed `LateralCapExceeded` error instead of silently truncating.
        outer_row_cap: usize,
    },
}
