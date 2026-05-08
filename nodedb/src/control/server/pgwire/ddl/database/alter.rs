// SPDX-License-Identifier: BUSL-1.1

//! Handler for `ALTER DATABASE <name> <operation>`.
//!
//! Supported operations:
//!   RENAME TO <new>           — updates name in `_system.databases` and rebuilds the
//!                               `_system.databases_by_name` reverse index atomically.
//!   SET QUOTA (<quota_id>)    — stores the quota reference id; enforcement is owned
//!                               by the quota subsystem and reads from `quota_ref`.
//!   SET DEFAULT               — marks this database as the per-user default. Returns
//!                               FEATURE_NOT_YET_IMPLEMENTED until per-user default
//!                               binding lands (use ALTER USER ... SET DEFAULT DATABASE).
//!   MATERIALIZE               — triggers background clone materialization. Returns
//!                               FEATURE_NOT_YET_IMPLEMENTED until the clone subsystem lands.
//!   PROMOTE                   — promotes a mirror to writable primary. Returns
//!                               FEATURE_NOT_YET_IMPLEMENTED until the mirror subsystem lands.

use nodedb_sql::ddl_ast::AlterDatabaseOperation;
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::catalog_entry::entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_admin, sqlstate_error};

/// Handle `ALTER DATABASE <name> <operation>`.
pub fn handle_alter_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    operation: &AlterDatabaseOperation,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "alter databases")?;

    let catalog = match state.credentials.catalog() {
        Some(c) => c,
        None => {
            return Err(sqlstate_error("XX000", "system catalog unavailable"));
        }
    };

    let db_id = catalog
        .get_database_id_by_name(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
        .ok_or_else(|| sqlstate_error("3D000", &format!("database '{name}' does not exist")))?;

    let mut descriptor = catalog
        .get_database(db_id)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog read failed: {e}")))?
        .ok_or_else(|| sqlstate_error("XX000", &format!("database '{name}' descriptor missing")))?;

    match operation {
        AlterDatabaseOperation::Rename { new_name } => {
            // Reject rename if a different database already holds the target name.
            match catalog.get_database_id_by_name(new_name) {
                Ok(Some(existing_id)) if existing_id != db_id => {
                    return Err(sqlstate_error(
                        "42P04",
                        &format!("database '{new_name}' already exists"),
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(sqlstate_error(
                        "XX000",
                        &format!("catalog lookup failed: {e}"),
                    ));
                }
            }
            descriptor.name = new_name.clone();
            let proposed = propose_catalog_entry(
                state,
                &CatalogEntry::PutDatabase(Box::new(descriptor.clone())),
            )
            .map_err(|e| sqlstate_error("XX000", &format!("catalog propose failed: {e}")))?;
            if proposed == 0 {
                catalog
                    .put_database(&descriptor)
                    .map_err(|e| sqlstate_error("XX000", &format!("catalog write failed: {e}")))?;
            }

            state.audit_record(
                crate::control::security::audit::AuditEvent::DdlChange,
                None,
                &identity.username,
                &format!("ALTER DATABASE {name} RENAME TO {new_name}"),
            );
        }

        AlterDatabaseOperation::SetQuota { quota_id } => {
            descriptor.quota_ref = *quota_id;
            let proposed = propose_catalog_entry(
                state,
                &CatalogEntry::PutDatabase(Box::new(descriptor.clone())),
            )
            .map_err(|e| sqlstate_error("XX000", &format!("catalog propose failed: {e}")))?;
            if proposed == 0 {
                catalog
                    .put_database(&descriptor)
                    .map_err(|e| sqlstate_error("XX000", &format!("catalog write failed: {e}")))?;
            }

            state.audit_record(
                crate::control::security::audit::AuditEvent::DdlChange,
                None,
                &identity.username,
                &format!("ALTER DATABASE {name} SET QUOTA {quota_id}"),
            );
        }

        AlterDatabaseOperation::SetDefault => {
            // The per-user default database field lives on AuthenticatedIdentity;
            // the canonical wiring is `ALTER USER <name> SET DEFAULT DATABASE <db>`,
            // which is owned by the user-management DDL path, not this one.
            return Err(sqlstate_error(
                "0A000",
                "ALTER DATABASE SET DEFAULT is not yet implemented; \
                 use ALTER USER <name> SET DEFAULT DATABASE <db>",
            ));
        }

        AlterDatabaseOperation::Materialize => {
            return Err(sqlstate_error(
                "0A000",
                "ALTER DATABASE MATERIALIZE is not yet implemented",
            ));
        }

        AlterDatabaseOperation::Promote => {
            return Err(sqlstate_error(
                "0A000",
                "ALTER DATABASE PROMOTE is not yet implemented",
            ));
        }
    }

    Ok(vec![Response::Execution(Tag::new("ALTER DATABASE"))])
}
