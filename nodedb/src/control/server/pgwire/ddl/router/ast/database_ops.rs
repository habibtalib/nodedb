// SPDX-License-Identifier: BUSL-1.1

//! Dispatch arms for database DDL statement variants.

use pgwire::api::results::Response;
use pgwire::error::PgWireResult;

use nodedb_sql::ddl_ast::NodedbStatement;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::server::pgwire::ddl::database::{
    handle_alter_database, handle_clone_database, handle_create_database, handle_drop_database,
    handle_show_database_lineage, handle_show_database_quota, handle_show_database_usage,
    handle_show_databases,
};
use crate::control::server::pgwire::ddl::tenant::{
    handle_alter_tenant_quota, handle_show_tenant_quota_in_database,
    handle_show_tenant_usage_in_database,
};
use crate::control::state::SharedState;

use super::super::super::super::types::sqlstate_error;

/// Try to dispatch a database DDL statement that does NOT require session-store
/// access (i.e. everything except `USE DATABASE`).
///
/// `USE DATABASE` requires the per-handler `SessionStore` and `SocketAddr`
/// and is intercepted in `execute_single_sql` before the DDL router runs.
///
/// Returns `Some(result)` if handled, `None` to fall through.
pub(super) fn try_dispatch_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    stmt: &NodedbStatement,
) -> Option<PgWireResult<Vec<Response>>> {
    match stmt {
        NodedbStatement::CreateDatabase {
            name,
            if_not_exists,
            options,
        } => Some(handle_create_database(
            state,
            identity,
            name,
            *if_not_exists,
            options,
        )),

        NodedbStatement::DropDatabase {
            name,
            if_exists,
            cascade,
        } => Some(handle_drop_database(
            state, identity, name, *if_exists, *cascade,
        )),

        NodedbStatement::AlterDatabase { name, operation } => {
            Some(handle_alter_database(state, identity, name, operation))
        }

        NodedbStatement::ShowDatabases => Some(handle_show_databases(state, identity)),

        NodedbStatement::ShowDatabaseQuota { name } => {
            Some(handle_show_database_quota(state, identity, name))
        }

        NodedbStatement::ShowDatabaseUsage { name } => {
            Some(handle_show_database_usage(state, identity, name))
        }

        NodedbStatement::ShowDatabaseLineage { name } => {
            Some(handle_show_database_lineage(state, identity, name))
        }

        NodedbStatement::AlterTenant {
            name,
            database,
            operation,
        } => Some(handle_alter_tenant_quota(
            state, identity, name, database, operation,
        )),

        NodedbStatement::ShowTenantQuotaInDatabase { name, database } => Some(
            handle_show_tenant_quota_in_database(state, identity, name, database),
        ),

        NodedbStatement::ShowTenantUsageInDatabase { name, database } => Some(
            handle_show_tenant_usage_in_database(state, identity, name, database),
        ),

        // UseDatabase is handled before the DDL router in execute_single_sql;
        // if it reaches here, something went wrong in the call chain.
        NodedbStatement::UseDatabase { name } => Some(Err(sqlstate_error(
            "XX000",
            &format!("USE DATABASE {name}: reached router after expected intercept"),
        ))),

        NodedbStatement::CloneDatabase {
            new_name,
            source_name,
            as_of,
        } => {
            use crate::control::server::pgwire::ddl::database::clone::CloneDatabaseParams;
            Some(handle_clone_database(
                state,
                identity,
                CloneDatabaseParams {
                    new_name,
                    source_name,
                    as_of,
                },
            ))
        }

        NodedbStatement::MirrorDatabase { .. } => Some(Err(sqlstate_error(
            "0A000",
            "MIRROR DATABASE is not yet implemented",
        ))),

        NodedbStatement::MoveTenant { .. } => {
            // Async — handled in try_dispatch_async (async_ops.rs).
            None
        }

        NodedbStatement::BackupDatabase { .. } => Some(Err(sqlstate_error(
            "0A000",
            "BACKUP DATABASE is not yet implemented",
        ))),

        NodedbStatement::RestoreDatabase { .. } => Some(Err(sqlstate_error(
            "0A000",
            "RESTORE DATABASE is not yet implemented",
        ))),

        _ => None,
    }
}
