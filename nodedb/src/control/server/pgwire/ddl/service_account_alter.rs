// SPDX-License-Identifier: BUSL-1.1

//! ALTER SERVICE ACCOUNT <name> SET DATABASES (db1, db2, ...)
//!
//! Superuser-only. Replaces the accessible_databases list on a service account.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::types::{require_superuser, sqlstate_error};

/// ALTER SERVICE ACCOUNT <name> SET DATABASES (db1, db2, ...)
///
/// Superuser only. Resolves database names to IDs; rejects unknown names with `42704`.
pub fn alter_service_account_set_databases(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    require_superuser(identity, "ALTER SERVICE ACCOUNT SET DATABASES")?;

    // parts: ["ALTER", "SERVICE", "ACCOUNT", <name>, "SET", "DATABASES", "(db1,", "db2", ...)"]
    if parts.len() < 7 {
        return Err(sqlstate_error(
            "42601",
            "syntax: ALTER SERVICE ACCOUNT <name> SET DATABASES (db1, db2, ...)",
        ));
    }

    if !parts[1].eq_ignore_ascii_case("SERVICE")
        || !parts[2].eq_ignore_ascii_case("ACCOUNT")
        || !parts[4].eq_ignore_ascii_case("SET")
        || !parts[5].eq_ignore_ascii_case("DATABASES")
    {
        return Err(sqlstate_error(
            "42601",
            "syntax: ALTER SERVICE ACCOUNT <name> SET DATABASES (db1, db2, ...)",
        ));
    }

    let name = parts[3];

    // Verify it's actually a service account.
    let user = state
        .credentials
        .get_user(name)
        .ok_or_else(|| sqlstate_error("42704", &format!("service account '{name}' not found")))?;
    if !user.is_service_account {
        return Err(sqlstate_error(
            "42809",
            &format!("'{name}' is a user, not a service account"),
        ));
    }

    // Collect and resolve database names from parts[6..].
    let raw_names: Vec<&str> = parts[6..]
        .iter()
        .map(|s| {
            s.trim_start_matches('(')
                .trim_end_matches(')')
                .trim_end_matches(',')
        })
        .filter(|s| !s.is_empty())
        .collect();

    if raw_names.is_empty() {
        return Err(sqlstate_error(
            "42601",
            "SET DATABASES requires at least one database name",
        ));
    }

    let catalog = state.credentials.catalog();
    let mut db_ids = Vec::with_capacity(raw_names.len());
    for db_name in raw_names {
        let resolved: Option<nodedb_types::id::DatabaseId> = catalog
            .as_ref()
            .map(|cat| cat.get_database_id_by_name(db_name))
            .transpose()
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?
            .flatten();
        match resolved {
            Some(id) => db_ids.push(id),
            None => {
                return Err(sqlstate_error(
                    "42704",
                    &format!("database '{db_name}' not found"),
                ));
            }
        }
    }

    state
        .credentials
        .set_service_account_databases(name, db_ids)
        .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;

    state.audit_record(
        AuditEvent::PrivilegeChange,
        Some(identity.tenant_id),
        &identity.username,
        &format!("altered service account '{name}': set databases"),
    );

    Ok(vec![Response::Execution(Tag::new("ALTER SERVICE ACCOUNT"))])
}
