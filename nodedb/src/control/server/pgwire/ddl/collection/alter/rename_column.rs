// SPDX-License-Identifier: BUSL-1.1

//! `ALTER COLLECTION <name> RENAME COLUMN <old> TO <new>` — rename a
//! column in a strict-document collection's schema.
//!
//! Binary-tuple layout is positional, so a rename is pure metadata: no row
//! re-encoding is required. The schema version is bumped so the Data Plane
//! picks up the new name on the next register dispatch.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::super::types::sqlstate_error;
use super::strict_schema::{load_strict_collection, persist_schema_change, write_schema_back};

/// ALTER COLLECTION <name> RENAME COLUMN <old_name> TO <new_name>
///
/// All fields arrive pre-parsed:
/// - `name`: collection name.
/// - `old_name`: current column name.
/// - `new_name`: new column name.
pub async fn alter_collection_rename_column(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    old_name: &str,
    new_name: &str,
) -> PgWireResult<Vec<Response>> {
    let tenant_id = identity.tenant_id;

    let (coll, mut schema) =
        load_strict_collection(state, tenant_id.as_u64(), name, "RENAME COLUMN")?;

    if schema
        .columns
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(new_name))
    {
        return Err(sqlstate_error(
            "42P07",
            &format!("column '{new_name}' already exists on '{name}'"),
        ));
    }

    let col = schema
        .columns
        .iter_mut()
        .find(|c| c.name.eq_ignore_ascii_case(old_name))
        .ok_or_else(|| {
            sqlstate_error(
                "42703",
                &format!("column '{old_name}' does not exist on '{name}'"),
            )
        })?;
    col.name = new_name.to_string();
    schema.version = schema.version.saturating_add(1);

    let mut updated = coll;
    write_schema_back(&mut updated, schema);
    persist_schema_change(state, &updated).await?;

    state.audit_record(
        AuditEvent::AdminAction,
        Some(tenant_id),
        &identity.username,
        &format!("ALTER COLLECTION '{name}' RENAME COLUMN '{old_name}' TO '{new_name}'"),
    );

    Ok(vec![Response::Execution(Tag::new("ALTER COLLECTION"))])
}
