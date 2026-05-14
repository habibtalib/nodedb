// SPDX-License-Identifier: Apache-2.0

//! The [`NodedbStatement`] enum — one variant per DDL command.
//!
//! Variant payload structs/enums live in the per-area sibling files
//! (`collection.rs`, `auth.rs`, `maintenance.rs`); this file only
//! declares the unified enum that references them.

use super::auth::*;
use super::collection::*;
use super::maintenance::*;
use super::super::alter_ops::{AlterCollectionOp, AlterRoleOp, AlterUserOp};
use super::super::graph_types::{GraphDirection, GraphProperties};

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

    // ── Database lifecycle ───────────────────────────────────────
    /// `CREATE DATABASE [IF NOT EXISTS] <name> [WITH (...)]`
    CreateDatabase {
        name: String,
        if_not_exists: bool,
        /// Key-value pairs from `WITH (...)`, if present.
        options: Vec<(String, String)>,
    },
    /// `DROP DATABASE [IF EXISTS] <name> [CASCADE | FORCE]`
    ///
    /// `FORCE` and `CASCADE` are accepted as synonyms by the parser and both
    /// set `cascade = true`. PostgreSQL's `WITH (FORCE)` extension also
    /// terminates active sessions; that is a separate concern handled by the
    /// session registry at drop time and does not require a distinct AST flag.
    DropDatabase {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    /// `ALTER DATABASE <name> <operation>`
    AlterDatabase {
        name: String,
        operation: AlterDatabaseOperation,
    },
    /// `SHOW DATABASES`
    ShowDatabases,
    /// `SHOW DATABASE QUOTA FOR <name>` — quota limits for a named database.
    ShowDatabaseQuota {
        name: String,
    },
    /// `SHOW DATABASE USAGE FOR <name>` — runtime usage counters for a database.
    ShowDatabaseUsage {
        name: String,
    },
    /// `SHOW DATABASE LINEAGE FOR <name>` — walks the parent clone chain from
    /// `<name>` up to the root, returning one row per ancestor with
    /// `(database_id, name, as_of_lsn, clone_created_at_lsn)`.
    ShowDatabaseLineage {
        name: String,
    },
    /// `ALTER TENANT <name> IN DATABASE <db> <operation>`
    ///
    /// New SQL surface. Sets per-tenant resource budgets within a specific database.
    AlterTenant {
        name: String,
        database: String,
        operation: AlterTenantOperation,
    },
    /// `SHOW TENANT QUOTA FOR <name> IN DATABASE <db>`
    ShowTenantQuotaInDatabase {
        name: String,
        database: String,
    },
    /// `SHOW TENANT USAGE FOR <name> IN DATABASE <db>`
    ShowTenantUsageInDatabase {
        name: String,
        database: String,
    },
    /// `USE DATABASE <name>` — session reset to a different database.
    UseDatabase {
        name: String,
    },
    /// `CLONE DATABASE <new> FROM <source> [AS OF SYSTEM TIME <ms> | LATEST]`
    CloneDatabase {
        new_name: String,
        source_name: String,
        /// The temporal anchor for this clone. `Latest` means "use the
        /// source's current commit LSN at clone time".
        as_of: CloneAsOf,
    },
    /// `MIRROR DATABASE <local_name> FROM <source_cluster>.<source_database> [MODE = sync | async]`
    ///
    /// Creates a continuously-updated read-only replica of `source_database` in
    /// `source_cluster`. The local database is initialized with
    /// `MirrorStatus::Bootstrapping` and transitions to `Following` once the
    /// initial snapshot transfer completes.
    ///
    /// Every match on this variant must be exhaustive — no `_ =>` arms.
    MirrorDatabase {
        /// Name of the new local mirror database.
        local_name: String,
        /// Cluster identifier of the source cluster.
        source_cluster: String,
        /// Name of the database in the source cluster to mirror.
        source_database: String,
        /// Replication mode: `Sync` means the source waits for mirror ack;
        /// `Async` (default) means the mirror trails the source.
        mode: nodedb_types::MirrorMode,
    },
    /// `SHOW DATABASE MIRROR STATUS [FOR <name>]`
    ///
    /// Returns one row per mirror database (or one row if `FOR <name>` is given):
    /// `name`, `source_cluster`, `source_database`, `mode`, `status`,
    /// `last_applied_lsn`, `last_apply_ms`.
    ///
    /// Every match on this variant must be exhaustive — no `_ =>` arms.
    ShowDatabaseMirrorStatus {
        /// Filter to a specific mirror by name, or `None` to show all mirrors.
        name: Option<String>,
    },
    /// `MOVE TENANT <tenant> FROM <db_a> TO <db_b>`
    ///
    /// Returns `FEATURE_NOT_YET_IMPLEMENTED` until the tenant-move subsystem lands.
    MoveTenant {
        tenant_name: String,
        from_db: String,
        to_db: String,
    },
    /// `BACKUP DATABASE <name> TO <uri>`
    ///
    /// Returns `FEATURE_NOT_YET_IMPLEMENTED` until the backup subsystem lands.
    BackupDatabase {
        name: String,
        uri: String,
    },
    /// `RESTORE DATABASE <name> FROM <uri>`
    ///
    /// Returns `FEATURE_NOT_YET_IMPLEMENTED` until the restore subsystem lands.
    RestoreDatabase {
        name: String,
        uri: String,
    },

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
    /// `GRANT <privilege> ON DATABASE <name> TO <user>`
    GrantDatabasePermission {
        permission: String,
        db_name: String,
        grantee: String,
    },
    RevokePermission {
        permission: String,
        target_type: String,
        target_name: String,
        grantee: String,
    },
    /// `REVOKE <privilege> ON DATABASE <name> FROM <user>`
    RevokeDatabasePermission {
        permission: String,
        db_name: String,
        grantee: String,
    },
    ShowPermissions {
        on_collection: Option<String>,
        for_grantee: Option<String>,
    },
    ShowGrants {
        username: Option<String>,
    },

    // ── OIDC providers ───────────────────────────────────────────
    /// `CREATE OIDC PROVIDER <name> ISSUER '<iss>' JWKS_URI '<uri>'
    ///  [AUDIENCE '<aud>'] [CLAIM MAPPING WHEN <claim_name> = '<value>'
    ///  SET DEFAULT_DATABASE = <id>, ADD DATABASES [<ids>], ADD ROLES ['<role>', ...]]`
    CreateOidcProvider {
        name: String,
        issuer: String,
        jwks_uri: String,
        audience: Option<String>,
        /// `(claim_name, claim_value, default_database, add_databases, add_roles)` tuples.
        claim_mappings: Vec<OidcClaimMappingClause>,
    },
    /// `ALTER OIDC PROVIDER <name> SET CLAIM MAPPING WHEN <claim_name> = '<value>'
    ///  SET DEFAULT_DATABASE = <id>, ADD DATABASES [<ids>], ADD ROLES ['<role>', ...]`
    ///
    /// Replaces the entire claim-mapping list for the named provider.
    AlterOidcProviderClaimMapping {
        name: String,
        claim_mappings: Vec<OidcClaimMappingClause>,
    },
    /// `DROP OIDC PROVIDER [IF EXISTS] <name>`
    DropOidcProvider {
        name: String,
        if_exists: bool,
    },
    /// `SHOW OIDC PROVIDERS`
    ShowOidcProviders,

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

    // ── Custom types ─────────────────────────────────────────────
    /// `CREATE TYPE <name> AS ENUM ('label1', 'label2', ...)`
    CreateEnumType {
        name: String,
        labels: Vec<String>,
    },
    /// `CREATE TYPE <name> AS (<field1> <type1>, <field2> <type2>, ...)`
    CreateCompositeType {
        name: String,
        /// `(field_name, type_name)` pairs.
        fields: Vec<(String, String)>,
    },
    /// `DROP TYPE [IF EXISTS] <name>`
    DropType {
        name: String,
        if_exists: bool,
    },
    /// `ALTER TYPE <name> ADD VALUE 'label'`
    AlterTypeAddValue {
        type_name: String,
        label: String,
    },
    /// `SHOW TYPES`
    ShowTypes,

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

    // ── Bulk export ──────────────────────────────────────────────
    /// `COPY <collection> TO '<path>' [WITH (FORMAT ..., DELIMITER ..., HEADER ...)]`
    /// `COPY (SELECT ...) TO '<path>' [WITH (...)]`
    ///
    /// Server-side file-path bulk export. Streams scan results to a file.
    CopyToFile {
        /// The source: either a bare collection name or a SELECT query.
        source: CopyToSource,
        path: String,
        format: Option<CopyFormat>,
        delimiter: Option<char>,
        header: bool,
    },
}
