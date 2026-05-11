// SPDX-License-Identifier: BUSL-1.1

//! `ALTER COLLECTION <name> ALTER COLUMN <col> TYPE <type>` — change a
//! column's declared type in a strict-document collection's schema.
//!
//! The current implementation only accepts type changes that map to the
//! same underlying `ColumnType` discriminant as the existing column —
//! equivalently, no-op aliases like INT → BIGINT (both map to `Int64`).
//! Widening across discriminants would require a full online rewrite and
//! is tracked as a separate enhancement.

use std::str::FromStr;

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::super::types::sqlstate_error;
use super::strict_schema::{load_strict_collection, persist_schema_change, write_schema_back};

/// ALTER COLLECTION <name> ALTER COLUMN <column_name> TYPE <new_type>
///
/// All fields arrive pre-parsed:
/// - `name`: collection name.
/// - `column_name`: column to alter.
/// - `new_type_str`: new type string (e.g. `"BIGINT"`).
pub async fn alter_collection_alter_column_type(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    column_name: &str,
    new_type_str: &str,
) -> PgWireResult<Vec<Response>> {
    let tenant_id = identity.tenant_id;

    let new_type = nodedb_types::columnar::ColumnType::from_str(new_type_str)
        .map_err(|e| sqlstate_error("42601", &format!("invalid type '{new_type_str}': {e}")))?;

    let (coll, mut schema) =
        load_strict_collection(state, tenant_id.as_u64(), name, "ALTER COLUMN TYPE")?;

    let col = schema
        .columns
        .iter_mut()
        .find(|c| c.name.eq_ignore_ascii_case(column_name))
        .ok_or_else(|| {
            sqlstate_error(
                "42703",
                &format!("column '{column_name}' does not exist on '{name}'"),
            )
        })?;

    // Reject a true type change that would require re-encoding existing rows.
    if std::mem::discriminant(&col.column_type) != std::mem::discriminant(&new_type) {
        return Err(sqlstate_error(
            "0A000",
            &format!(
                "cross-type change from {:?} to {:?} requires an online rewrite; \
                 only alias type changes (e.g. INT ↔ BIGINT) are supported today",
                col.column_type, new_type
            ),
        ));
    }
    col.column_type = new_type;
    schema.version = schema.version.saturating_add(1);

    let mut updated = coll;
    write_schema_back(&mut updated, schema);
    persist_schema_change(state, &updated).await?;

    state.audit_record(
        AuditEvent::AdminAction,
        Some(tenant_id),
        &identity.username,
        &format!("ALTER COLLECTION '{name}' ALTER COLUMN '{column_name}' TYPE {new_type_str}"),
    );

    Ok(vec![Response::Execution(Tag::new("ALTER COLLECTION"))])
}
