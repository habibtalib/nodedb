// SPDX-License-Identifier: BUSL-1.1

//! Synchronous DDL dispatch arms (no `.await`).

use pgwire::api::results::Response;
use pgwire::error::PgWireResult;

use nodedb_sql::ddl_ast::NodedbStatement;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::server::pgwire::ddl::alert::alter_alert;
use crate::control::server::pgwire::ddl::alert::{CreateAlertRequest, create_alert};
use crate::control::server::pgwire::ddl::change_stream::alter_change_stream;
use crate::control::server::pgwire::ddl::cluster::alter_raft_group;
use crate::control::server::pgwire::ddl::consumer_group::create_consumer_group;
use crate::control::server::pgwire::ddl::grant::database_permission::{
    grant_database, revoke_database,
};
use crate::control::server::pgwire::ddl::grant::permission::{grant_permission, revoke_permission};
use crate::control::server::pgwire::ddl::grant::role::{grant_role, revoke_role};
use crate::control::server::pgwire::ddl::inspect::show_permissions;
use crate::control::server::pgwire::ddl::retention_policy::alter_retention_policy;
use crate::control::server::pgwire::ddl::rls::{CreateRlsPolicyRequest, create_rls_policy};
use crate::control::server::pgwire::ddl::role::alter_role_typed;
use crate::control::server::pgwire::ddl::schedule::alter_schedule;
use crate::control::server::pgwire::ddl::sequence::{alter_sequence, create_sequence};
use crate::control::server::pgwire::ddl::trigger::alter_trigger;
use crate::control::server::pgwire::ddl::user::{alter_user, create_user};
use crate::control::state::SharedState;

use super::database_ops::try_dispatch_database;

/// Try to dispatch synchronous (non-async) DDL statement variants.
/// Returns `Some(result)` if handled, `None` to fall through.
pub(super) fn try_dispatch_sync(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    stmt: &NodedbStatement,
) -> Option<PgWireResult<Vec<Response>>> {
    // Database DDL (all synchronous — catalog reads/writes only).
    if let Some(result) = try_dispatch_database(state, identity, stmt) {
        return Some(result);
    }

    match stmt {
        NodedbStatement::GrantRole { role, username } => {
            Some(grant_role(state, identity, role, username))
        }

        NodedbStatement::RevokeRole { role, username } => {
            Some(revoke_role(state, identity, role, username))
        }

        NodedbStatement::AlterAlert { name, action } => {
            Some(alter_alert(state, identity, name, action))
        }

        NodedbStatement::AlterChangeStream { name, action } => {
            Some(alter_change_stream(state, identity, name, action))
        }

        NodedbStatement::BackupTenant { .. } => {
            Some(Err(super::super::super::super::types::sqlstate_error(
                "0A000",
                "use `COPY (BACKUP TENANT <id>) TO STDOUT` to stream backup bytes to the client",
            )))
        }

        NodedbStatement::RestoreTenant { .. } => {
            Some(Err(super::super::super::super::types::sqlstate_error(
                "0A000",
                "use `COPY tenant_restore(<id>) FROM STDIN` to stream backup bytes from the client",
            )))
        }

        NodedbStatement::AlterTrigger {
            name,
            action,
            new_owner,
        } => Some(alter_trigger(
            state,
            identity,
            name,
            action,
            new_owner.as_deref(),
        )),

        NodedbStatement::AlterRaftGroup {
            group_id,
            action,
            node_id,
        } => Some(alter_raft_group(state, identity, group_id, action, node_id)),

        NodedbStatement::GrantPermission {
            permission,
            target_type,
            target_name,
            grantee,
        } => Some(grant_permission(
            state,
            identity,
            permission,
            target_type,
            target_name,
            grantee,
        )),

        NodedbStatement::GrantDatabasePermission {
            permission,
            db_name,
            grantee,
        } => Some(grant_database(
            state, identity, permission, db_name, grantee,
        )),

        NodedbStatement::RevokePermission {
            permission,
            target_type,
            target_name,
            grantee,
        } => Some(revoke_permission(
            state,
            identity,
            permission,
            target_type,
            target_name,
            grantee,
        )),

        NodedbStatement::RevokeDatabasePermission {
            permission,
            db_name,
            grantee,
        } => Some(revoke_database(
            state, identity, permission, db_name, grantee,
        )),

        NodedbStatement::AlterSchedule {
            name,
            action,
            cron_expr,
        } => Some(alter_schedule(
            state,
            identity,
            name,
            action,
            cron_expr.as_deref(),
        )),

        NodedbStatement::AlterRetentionPolicy {
            name,
            action,
            set_key,
            set_value,
        } => Some(alter_retention_policy(
            state,
            identity,
            name,
            action,
            set_key.as_deref(),
            set_value.as_deref(),
        )),

        NodedbStatement::AlterSequence {
            name,
            action,
            with_value,
        } => Some(alter_sequence(
            state,
            identity,
            name,
            action,
            with_value.as_deref(),
        )),

        NodedbStatement::CreateConsumerGroup {
            group_name,
            stream_name,
        } => Some(create_consumer_group(
            state,
            identity,
            group_name,
            stream_name,
        )),

        NodedbStatement::CreateAlert {
            name,
            collection,
            where_filter,
            condition_raw,
            group_by,
            window_raw,
            fire_after,
            recover_after,
            severity,
            notify_targets_raw,
        } => Some(create_alert(
            state,
            identity,
            &CreateAlertRequest {
                name,
                collection,
                where_filter: where_filter.as_deref(),
                condition_raw,
                group_by,
                window_raw,
                fire_after: *fire_after,
                recover_after: *recover_after,
                severity,
                notify_targets_raw,
            },
        )),

        NodedbStatement::CreateRlsPolicy {
            name,
            collection,
            policy_type,
            predicate_raw,
            is_restrictive,
            on_deny_raw,
            tenant_id_override,
        } => Some(create_rls_policy(
            state,
            identity,
            &CreateRlsPolicyRequest {
                name,
                collection,
                policy_type_raw: policy_type,
                predicate_raw,
                is_restrictive: *is_restrictive,
                on_deny_raw: on_deny_raw.as_deref(),
                tenant_id_override: *tenant_id_override,
            },
        )),

        NodedbStatement::CreateSequence {
            name,
            if_not_exists: false,
            start,
            increment,
            min_value,
            max_value,
            cycle,
            cache,
            format_template_raw,
            reset_period_raw,
            gap_free,
            scope,
        } => Some(create_sequence(
            state,
            identity,
            name,
            *start,
            *increment,
            *min_value,
            *max_value,
            *cycle,
            *cache,
            format_template_raw.as_deref(),
            reset_period_raw.as_deref(),
            *gap_free,
            scope.as_deref(),
        )),

        NodedbStatement::CreateUser {
            username,
            password,
            role,
            tenant_id,
        } => Some(create_user(
            state,
            identity,
            username,
            password,
            role.as_deref(),
            *tenant_id,
        )),

        NodedbStatement::AlterUser { username, op } => {
            Some(alter_user(state, identity, username, op))
        }

        NodedbStatement::ShowPermissions {
            on_collection,
            for_grantee,
        } => Some(show_permissions(
            state,
            identity,
            on_collection.as_deref(),
            for_grantee.as_deref(),
        )),

        NodedbStatement::AlterRole { name, sub_op } => {
            Some(alter_role_typed(state, identity, name, sub_op))
        }

        _ => None,
    }
}
