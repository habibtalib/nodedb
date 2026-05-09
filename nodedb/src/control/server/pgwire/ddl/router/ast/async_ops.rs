// SPDX-License-Identifier: BUSL-1.1

//! Asynchronous DDL dispatch arms (variants that require `.await`).

use pgwire::api::results::Response;
use pgwire::error::PgWireResult;

use nodedb_sql::ddl_ast::NodedbStatement;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::server::pgwire::ddl::change_stream::create_change_stream;
use crate::control::server::pgwire::ddl::collection::copy_from::CopyFromOptions;
use crate::control::server::pgwire::ddl::collection::{
    CreateCollectionRequest, CreateIndexRequest, copy_from_file, copy_to_file, create_collection,
    create_index, create_table, dispatch_register_by_name,
};
use crate::control::server::pgwire::ddl::conflict_policy::show_conflict_policy;
use crate::control::server::pgwire::ddl::continuous_agg::{
    CreateContinuousAggregateRequest, create_continuous_aggregate,
};
use crate::control::server::pgwire::ddl::custom_type::{
    alter_type_add_value, create_composite_type, create_enum_type, drop_type, show_types,
};
use crate::control::server::pgwire::ddl::materialized_view::create_materialized_view;
use crate::control::server::pgwire::ddl::retention_policy::create_retention_policy;
use crate::control::server::pgwire::ddl::schedule::{CreateScheduleRequest, create_schedule};
use crate::control::server::pgwire::ddl::synonym_group::{
    create_synonym_group, drop_synonym_group, show_synonym_groups,
};
use crate::control::server::pgwire::ddl::tenant::handle_move_tenant;
use crate::control::server::pgwire::ddl::trigger::create_trigger;
use crate::control::state::SharedState;
use crate::types::DatabaseId;

use super::alter::dispatch_alter_collection;

/// Try to dispatch asynchronous DDL statement variants.
/// Returns `Some(result)` if handled, `None` to fall through to legacy dispatch.
pub(super) async fn try_dispatch_async(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    stmt: &NodedbStatement,
    database_id: DatabaseId,
) -> Option<PgWireResult<Vec<Response>>> {
    match stmt {
        NodedbStatement::CreateTrigger {
            or_replace,
            execution_mode,
            name,
            timing,
            events_insert,
            events_update,
            events_delete,
            collection,
            granularity,
            when_condition,
            priority,
            security,
            body_sql,
        } => Some(create_trigger(
            state,
            identity,
            *or_replace,
            execution_mode,
            name,
            timing,
            *events_insert,
            *events_update,
            *events_delete,
            collection,
            granularity,
            when_condition.as_deref(),
            *priority,
            security,
            body_sql,
        )),

        NodedbStatement::CreateSchedule {
            name,
            cron_expr,
            body_sql,
            scope,
            missed_policy,
            allow_overlap,
        } => Some(create_schedule(
            state,
            identity,
            &CreateScheduleRequest {
                name,
                cron_expr,
                body_sql,
                scope,
                missed_policy,
                allow_overlap: *allow_overlap,
            },
        )),

        NodedbStatement::CreateChangeStream {
            name,
            collection,
            with_clause_raw,
        } => Some(create_change_stream(
            state,
            identity,
            name,
            collection,
            with_clause_raw,
        )),

        NodedbStatement::CreateMaterializedView {
            name,
            source,
            query_sql,
            refresh_mode,
        } => Some(
            create_materialized_view(state, identity, name, source, query_sql, refresh_mode).await,
        ),

        NodedbStatement::CreateContinuousAggregate {
            name,
            source,
            bucket_raw,
            aggregate_exprs_raw,
            group_by,
            with_clause_raw,
        } => Some(
            create_continuous_aggregate(
                state,
                identity,
                &CreateContinuousAggregateRequest {
                    name,
                    source,
                    bucket_raw,
                    aggregate_exprs_raw,
                    group_by,
                    with_clause_raw,
                },
            )
            .await,
        ),

        NodedbStatement::CreateRetentionPolicy {
            name,
            collection,
            body_raw,
            eval_interval_raw,
        } => Some(
            create_retention_policy(
                state,
                identity,
                name,
                collection,
                body_raw,
                eval_interval_raw.as_deref(),
            )
            .await,
        ),

        NodedbStatement::CreateIndex {
            unique,
            index_name,
            collection,
            field,
            case_insensitive,
            where_condition,
        } => Some(
            create_index(
                state,
                identity,
                &CreateIndexRequest {
                    is_unique: *unique,
                    index_name_opt: index_name.as_deref(),
                    collection,
                    field,
                    case_insensitive: *case_insensitive,
                    where_condition: where_condition.as_deref(),
                },
            )
            .await,
        ),

        NodedbStatement::CreateCollection {
            name,
            if_not_exists: _,
            engine,
            columns,
            options,
            flags,
            balanced_raw,
        } => {
            let result = create_collection(
                state,
                identity,
                &CreateCollectionRequest {
                    name,
                    engine: engine.as_deref(),
                    columns,
                    options,
                    flags,
                    balanced_raw: balanced_raw.as_deref(),
                },
                database_id,
            );
            let result = match result {
                Ok(resp) => dispatch_register_by_name(state, identity, name, database_id)
                    .await
                    .map(|()| resp)
                    .map_err(|e| {
                        super::super::super::super::types::sqlstate_error("XX000", &e.to_string())
                    }),
                Err(e) => Err(e),
            };
            Some(result)
        }

        NodedbStatement::CreateTable {
            name,
            // Both false (normal create) and true (IF NOT EXISTS — guard
            // already returned early if the collection existed) fall through
            // to the same create_table handler.
            if_not_exists: _,
            engine,
            columns,
            options,
            flags,
            balanced_raw,
        } => {
            let result = create_table(
                state,
                identity,
                &CreateCollectionRequest {
                    name,
                    engine: engine.as_deref(),
                    columns,
                    options,
                    flags,
                    balanced_raw: balanced_raw.as_deref(),
                },
                database_id,
            )
            .await;
            let result = match result {
                Ok(resp) => dispatch_register_by_name(state, identity, name, database_id)
                    .await
                    .map(|()| resp)
                    .map_err(|e| {
                        super::super::super::super::types::sqlstate_error("XX000", &e.to_string())
                    }),
                Err(e) => Err(e),
            };
            Some(result)
        }

        NodedbStatement::AlterCollection { name, operation } => {
            Some(dispatch_alter_collection(state, identity, name, operation).await)
        }

        NodedbStatement::ShowConflictPolicy { collection } => {
            Some(show_conflict_policy(state, identity, collection).await)
        }

        NodedbStatement::CreateSynonymGroup { name, terms } => {
            Some(create_synonym_group(state, identity, name, terms).await)
        }

        NodedbStatement::DropSynonymGroup { name, if_exists } => {
            Some(drop_synonym_group(state, identity, name, *if_exists).await)
        }

        NodedbStatement::ShowSynonymGroups => Some(show_synonym_groups(state, identity)),

        NodedbStatement::CreateEnumType { name, labels } => {
            Some(create_enum_type(state, identity, name, labels).await)
        }

        NodedbStatement::CreateCompositeType { name, fields } => {
            Some(create_composite_type(state, identity, name, fields).await)
        }

        NodedbStatement::DropType { name, if_exists } => {
            Some(drop_type(state, identity, name, *if_exists).await)
        }

        NodedbStatement::AlterTypeAddValue { type_name, label } => {
            Some(alter_type_add_value(state, identity, type_name, label).await)
        }

        NodedbStatement::ShowTypes => Some(show_types(state, identity)),

        NodedbStatement::Reindex {
            collection,
            index_name,
            concurrent,
        } => Some(
            super::super::super::maintenance::handle_reindex(
                state,
                identity,
                collection,
                index_name.as_deref(),
                *concurrent,
            )
            .await,
        ),

        NodedbStatement::CopyFromFile {
            collection,
            path,
            format,
            delimiter,
            header,
        } => Some(
            copy_from_file(
                state,
                identity,
                collection,
                path,
                CopyFromOptions {
                    format: format.as_ref(),
                    delimiter: *delimiter,
                    header: *header,
                },
                database_id,
            )
            .await,
        ),

        NodedbStatement::CopyToFile {
            source,
            path,
            format,
            delimiter,
            header,
        } => Some(
            copy_to_file(
                state,
                identity,
                source,
                path,
                format.as_ref(),
                *delimiter,
                *header,
            )
            .await,
        ),

        NodedbStatement::MoveTenant {
            tenant_name,
            from_db,
            to_db,
        } => Some(handle_move_tenant(state, identity, tenant_name, from_db, to_db).await),

        _ => None,
    }
}
