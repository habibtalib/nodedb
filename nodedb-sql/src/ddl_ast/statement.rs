//! The [`NodedbStatement`] enum — one variant per DDL command.

pub use super::alter_ops::{AlterCollectionOp, AlterRoleOp, AlterUserOp};
pub use super::graph_types::{GraphDirection, GraphProperties};

/// Typed representation of every NodeDB DDL statement.
///
/// Handlers receive a fully-parsed variant instead of raw `&[&str]`
/// parts, eliminating array-index panics and enabling exhaustive
/// match coverage for new DDL commands.
#[derive(Debug, Clone, PartialEq)]
pub enum NodedbStatement {
    // ── Collection lifecycle ─────────────────────────────────────
    CreateCollection {
        name: String,
        if_not_exists: bool,
        /// Canonical engine name (e.g. `"kv"`, `"vector"`, `"document_strict"`).
        /// `None` means no `engine=` key was present.
        engine: Option<String>,
        /// `(col_name, col_type)` pairs from the parenthesised column list.
        columns: Vec<(String, String)>,
        /// Key-value pairs from the `WITH (...)` clause, excluding `engine=`.
        options: Vec<(String, String)>,
        /// Free-standing modifier keywords: `APPEND_ONLY`, `HASH_CHAIN`, `BITEMPORAL`.
        flags: Vec<String>,
        /// Raw interior of a `BALANCED ON (group_key = col, ...)` clause, or `None`.
        balanced_raw: Option<String>,
    },
    /// `CREATE TABLE <name> (<col_list>)` — Postgres-style strict-default DDL.
    /// Infers strict relational mode unless overridden via `WITH (engine='...')`.
    /// No column list → rejected with SQLSTATE `42601`.
    CreateTable {
        name: String,
        if_not_exists: bool,
        engine: Option<String>,
        columns: Vec<(String, String)>,
        options: Vec<(String, String)>,
        flags: Vec<String>,
        balanced_raw: Option<String>,
    },
    DropCollection {
        name: String,
        if_exists: bool,
        /// Skip the soft-delete step (requires superuser/tenant_admin).
        purge: bool,
        /// Recursively drop dependents (triggers, RLS, MVs, streams, schedules).
        cascade: bool,
        /// Like `cascade` but also drops schedules with `references_unknown = true`.
        cascade_force: bool,
    },
    /// `UNDROP COLLECTION <n>` — restore a soft-deleted collection within retention window.
    UndropCollection {
        name: String,
    },
    AlterCollection {
        name: String,
        operation: AlterCollectionOp,
    },
    DescribeCollection {
        name: String,
    },
    ShowCollections,

    // ── Index ────────────────────────────────────────────────────
    CreateIndex {
        unique: bool,
        index_name: Option<String>,
        collection: String,
        field: String,
        case_insensitive: bool,
        where_condition: Option<String>,
    },
    DropIndex {
        name: String,
        collection: Option<String>,
        if_exists: bool,
    },
    ShowIndexes {
        collection: Option<String>,
    },
    Reindex {
        collection: String,
        index_name: Option<String>,
        concurrent: bool,
    },

    // ── Trigger ──────────────────────────────────────────────────
    CreateTrigger {
        or_replace: bool,
        /// "ASYNC", "SYNC", or "DEFERRED".
        execution_mode: String,
        name: String,
        /// "BEFORE", "AFTER", or "INSTEAD OF".
        timing: String,
        events_insert: bool,
        events_update: bool,
        events_delete: bool,
        collection: String,
        /// "ROW" or "STATEMENT".
        granularity: String,
        when_condition: Option<String>,
        priority: i32,
        /// "INVOKER" or "DEFINER".
        security: String,
        body_sql: String,
    },
    DropTrigger {
        name: String,
        collection: String,
        if_exists: bool,
    },
    AlterTrigger {
        name: String,
        action: String,
        new_owner: Option<String>,
    },
    ShowTriggers {
        collection: Option<String>,
    },

    // ── Schedule ─────────────────────────────────────────────────
    CreateSchedule {
        name: String,
        cron_expr: String,
        body_sql: String,
        scope: String,
        missed_policy: String,
        allow_overlap: bool,
    },
    DropSchedule {
        name: String,
        if_exists: bool,
    },
    AlterSchedule {
        name: String,
        action: String,
        cron_expr: Option<String>,
    },
    ShowSchedules,
    ShowScheduleHistory {
        name: String,
    },

    // ── Sequence ─────────────────────────────────────────────────
    CreateSequence {
        name: String,
        if_not_exists: bool,
        start: Option<i64>,
        increment: Option<i64>,
        min_value: Option<i64>,
        max_value: Option<i64>,
        cycle: bool,
        cache: Option<i64>,
        /// Raw `FORMAT 'template'` string (quotes stripped), or `None`.
        format_template_raw: Option<String>,
        /// Raw `RESET YEARLY|MONTHLY|QUARTERLY|DAILY` token, or `None`.
        reset_period_raw: Option<String>,
        gap_free: bool,
        scope: Option<String>,
    },
    DropSequence {
        name: String,
        if_exists: bool,
    },
    AlterSequence {
        name: String,
        action: String,
        with_value: Option<String>,
    },
    DescribeSequence {
        name: String,
    },
    ShowSequences,

    // ── Alert ────────────────────────────────────────────────────
    CreateAlert {
        name: String,
        collection: String,
        where_filter: Option<String>,
        condition_raw: String,
        group_by: Vec<String>,
        window_raw: String,
        fire_after: u32,
        recover_after: u32,
        severity: String,
        notify_targets_raw: String,
    },
    DropAlert {
        name: String,
        if_exists: bool,
    },
    AlterAlert {
        name: String,
        action: String,
    },
    ShowAlerts,
    ShowAlertStatus {
        name: String,
    },

    // ── Retention policy ─────────────────────────────────────────
    CreateRetentionPolicy {
        name: String,
        collection: String,
        body_raw: String,
        eval_interval_raw: Option<String>,
    },
    DropRetentionPolicy {
        name: String,
        if_exists: bool,
    },
    AlterRetentionPolicy {
        name: String,
        action: String,
        set_key: Option<String>,
        set_value: Option<String>,
    },
    ShowRetentionPolicies,

    // ── Change stream ────────────────────────────────────────────
    CreateChangeStream {
        name: String,
        collection: String,
        with_clause_raw: String,
    },
    DropChangeStream {
        name: String,
        if_exists: bool,
    },
    AlterChangeStream {
        name: String,
        action: String,
    },
    ShowChangeStreams,

    // ── Consumer group ───────────────────────────────────────────
    CreateConsumerGroup {
        group_name: String,
        stream_name: String,
    },
    DropConsumerGroup {
        name: String,
        stream: String,
        if_exists: bool,
    },
    ShowConsumerGroups {
        stream: Option<String>,
    },

    // ── RLS policy ───────────────────────────────────────────────
    CreateRlsPolicy {
        name: String,
        collection: String,
        policy_type: String,
        predicate_raw: String,
        is_restrictive: bool,
        on_deny_raw: Option<String>,
        tenant_id_override: Option<u64>,
    },
    DropRlsPolicy {
        name: String,
        collection: String,
        if_exists: bool,
    },
    ShowRlsPolicies {
        collection: Option<String>,
    },

    // ── Materialized view ────────────────────────────────────────
    CreateMaterializedView {
        name: String,
        source: String,
        query_sql: String,
        refresh_mode: String,
    },
    DropMaterializedView {
        name: String,
        if_exists: bool,
    },
    ShowMaterializedViews,

    // ── Continuous aggregate ─────────────────────────────────────
    CreateContinuousAggregate {
        name: String,
        source: String,
        bucket_raw: String,
        aggregate_exprs_raw: String,
        group_by: Vec<String>,
        with_clause_raw: String,
    },
    DropContinuousAggregate {
        name: String,
        if_exists: bool,
    },
    ShowContinuousAggregates,

    // ── Backup / restore ─────────────────────────────────────────
    BackupTenant {
        tenant_id: String,
    },
    RestoreTenant {
        dry_run: bool,
        tenant_id: String,
    },

    // ── Cluster admin ────────────────────────────────────────────
    ShowNodes,
    ShowNode {
        node_id: String,
    },
    RemoveNode {
        node_id: String,
    },
    ShowCluster,
    ShowMigrations,
    ShowRanges,
    ShowRouting,
    ShowSchemaVersion,
    ShowPeerHealth,
    Rebalance,
    ShowRaftGroups,
    ShowRaftGroup {
        group_id: String,
    },
    AlterRaftGroup {
        group_id: String,
        action: String,
        node_id: String,
    },

    // ── Maintenance ──────────────────────────────────────────────
    Analyze {
        collection: Option<String>,
    },
    Compact {
        collection: String,
    },
    ShowStorage {
        collection: Option<String>,
    },
    ShowCompactionStatus,

    // ── User / auth / grant ──────────────────────────────────────
    CreateUser {
        username: String,
        password: String,
        role: Option<String>,
        tenant_id: Option<u64>,
    },
    DropUser {
        username: String,
    },
    AlterUser {
        username: String,
        op: AlterUserOp,
    },
    ShowUsers,
    /// `ALTER ROLE <name> GRANT/REVOKE/SET`.
    AlterRole {
        name: String,
        sub_op: AlterRoleOp,
    },
    GrantRole {
        role: String,
        username: String,
    },
    RevokeRole {
        role: String,
        username: String,
    },
    GrantPermission {
        permission: String,
        target_type: String,
        target_name: String,
        grantee: String,
    },
    RevokePermission {
        permission: String,
        target_type: String,
        target_name: String,
        grantee: String,
    },
    ShowPermissions {
        on_collection: Option<String>,
        for_grantee: Option<String>,
    },
    ShowGrants {
        username: Option<String>,
    },

    // ── CRDT conflict policy ─────────────────────────────────────
    /// `SHOW CONFLICT POLICY ON <collection>`
    ShowConflictPolicy {
        collection: String,
    },

    // ── Miscellaneous ────────────────────────────────────────────
    ShowTenants,
    ShowAuditLog,
    ShowConstraints {
        collection: String,
    },
    ShowTypeGuards {
        collection: String,
    },

    // ── Synonym groups ───────────────────────────────────────────
    /// `CREATE SYNONYM GROUP <name> AS ('term1', 'term2', ...)`
    CreateSynonymGroup {
        name: String,
        terms: Vec<String>,
    },
    /// `DROP SYNONYM GROUP [IF EXISTS] <name>`
    DropSynonymGroup {
        name: String,
        if_exists: bool,
    },
    /// `SHOW SYNONYM GROUPS`
    ShowSynonymGroups,

    // ── Graph DSL ────────────────────────────────────────────────
    GraphInsertEdge {
        collection: String,
        src: String,
        dst: String,
        label: String,
        properties: GraphProperties,
    },
    GraphDeleteEdge {
        collection: String,
        src: String,
        dst: String,
        label: String,
    },
    GraphSetLabels {
        node_id: String,
        labels: Vec<String>,
        remove: bool,
    },
    GraphTraverse {
        start: String,
        depth: usize,
        edge_label: Option<String>,
        direction: GraphDirection,
    },
    GraphNeighbors {
        node: String,
        edge_label: Option<String>,
        direction: GraphDirection,
    },
    GraphPath {
        src: String,
        dst: String,
        max_depth: usize,
        edge_label: Option<String>,
    },
    GraphAlgo {
        algorithm: String,
        collection: String,
        edge_label: Option<String>,
        damping: Option<f64>,
        tolerance: Option<f64>,
        resolution: Option<f64>,
        max_iterations: Option<usize>,
        sample_size: Option<usize>,
        source_node: Option<String>,
        direction: Option<String>,
        mode: Option<String>,
    },
    /// `MATCH (x)-[:l]->(y) RETURN x, y` — body forwarded verbatim to the graph pattern compiler.
    MatchQuery {
        body: String,
    },
    /// `GRAPH RAG FUSION ON <collection> QUERY ARRAY[…] [options…]`
    GraphRagFusion {
        collection: String,
        params: crate::ddl_ast::graph_parse::FusionParams,
    },

    // ── Bulk import ──────────────────────────────────────────────
    /// `COPY <collection> FROM '<path>' [WITH (FORMAT ..., DELIMITER ..., HEADER ...)]`
    ///
    /// Server-side file-path bulk import. Does not handle STDIN streaming
    /// (that is a different protocol path) or COPY ... TO.
    CopyFromFile {
        collection: String,
        path: String,
        format: Option<CopyFormat>,
        delimiter: Option<char>,
        header: bool,
    },
}

/// Format for `COPY ... FROM` bulk import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyFormat {
    /// One JSON object per line (`.ndjson` / `.jsonl`).
    Ndjson,
    /// A JSON array of objects (`.json`).
    JsonArray,
    /// CSV with an optional header row (`.csv`).
    Csv,
}
