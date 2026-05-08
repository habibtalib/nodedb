// SPDX-License-Identifier: BUSL-1.1

//! Handlers for database-level GRANT and REVOKE statements.
//!
//! ```sql
//! GRANT ALL ON DATABASE <name> TO <user>;
//! GRANT CREATE COLLECTION ON DATABASE <name> TO <user>;
//! GRANT SELECT ON DATABASE <name> TO <user>;
//! REVOKE ALL ON DATABASE <name> FROM <user>;
//! ```
//!
//! Grants are stored in `_system.database_grants`. They are also reflected
//! into the user's `accessible_databases` set — new grants add the database
//! to the set; all privileges revoked removes it.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::catalog_entry::entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_admin, sqlstate_error};

/// Handle `GRANT <privilege> ON DATABASE <name> TO <user>`.
///
/// Accepted privileges: `ALL`, `CREATE COLLECTION`, `SELECT`.
pub fn grant_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    privilege: &str,
    db_name: &str,
    grantee: &str,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "GRANT ON DATABASE")?;

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    let db_id = catalog
        .get_database_id_by_name(db_name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup: {e}")))?
        .ok_or_else(|| sqlstate_error("42704", &format!("database '{db_name}' does not exist")))?;

    // Resolve the target user_id from the grantee name.
    let user_record = state
        .credentials
        .get_user(grantee)
        .ok_or_else(|| sqlstate_error("42704", &format!("user '{grantee}' does not exist")))?;

    let privileges: Vec<&str> = if privilege.eq_ignore_ascii_case("ALL") {
        vec!["ALL", "CREATE_COLLECTION", "SELECT"]
    } else {
        vec![privilege]
    };

    for priv_name in &privileges {
        let proposed = propose_catalog_entry(
            state,
            &CatalogEntry::PutDatabaseGrant {
                db_id: db_id.as_u64(),
                user_id: user_record.user_id,
                privilege: priv_name.to_string(),
            },
        )
        .map_err(|e| sqlstate_error("XX000", &format!("catalog propose: {e}")))?;

        if proposed == 0 {
            catalog
                .put_database_grant(db_id, user_record.user_id, priv_name)
                .map_err(|e| sqlstate_error("XX000", &format!("catalog write: {e}")))?;
        }
    }

    state.audit_record(
        AuditEvent::PrivilegeChange,
        Some(identity.tenant_id),
        &identity.username,
        &format!("GRANT {} ON DATABASE {} TO {}", privilege, db_name, grantee),
    );

    Ok(vec![Response::Execution(Tag::new("GRANT"))])
}

/// Handle `REVOKE <privilege> ON DATABASE <name> FROM <user>`.
pub fn revoke_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    privilege: &str,
    db_name: &str,
    grantee: &str,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "REVOKE ON DATABASE")?;

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    let db_id = catalog
        .get_database_id_by_name(db_name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup: {e}")))?
        .ok_or_else(|| sqlstate_error("42704", &format!("database '{db_name}' does not exist")))?;

    let user_record = state
        .credentials
        .get_user(grantee)
        .ok_or_else(|| sqlstate_error("42704", &format!("user '{grantee}' does not exist")))?;

    let privileges: Vec<&str> = if privilege.eq_ignore_ascii_case("ALL") {
        vec!["ALL", "CREATE_COLLECTION", "SELECT"]
    } else {
        vec![privilege]
    };

    for priv_name in &privileges {
        let proposed = propose_catalog_entry(
            state,
            &CatalogEntry::DeleteDatabaseGrant {
                db_id: db_id.as_u64(),
                user_id: user_record.user_id,
                privilege: priv_name.to_string(),
            },
        )
        .map_err(|e| sqlstate_error("XX000", &format!("catalog propose: {e}")))?;

        if proposed == 0 {
            catalog
                .delete_database_grant(db_id, user_record.user_id, priv_name)
                .map_err(|e| sqlstate_error("XX000", &format!("catalog write: {e}")))?;
        }
    }

    state.audit_record(
        AuditEvent::PrivilegeChange,
        Some(identity.tenant_id),
        &identity.username,
        &format!(
            "REVOKE {} ON DATABASE {} FROM {}",
            privilege, db_name, grantee
        ),
    );

    Ok(vec![Response::Execution(Tag::new("REVOKE"))])
}
