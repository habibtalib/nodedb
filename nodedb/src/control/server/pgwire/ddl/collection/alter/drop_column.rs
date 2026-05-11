// SPDX-License-Identifier: BUSL-1.1

//! `ALTER COLLECTION <name> DROP COLUMN <col>` — remove a column from a
//! strict-document collection's schema.
//!
//! Current scope: strict collections only. The column is removed from the
//! schema metadata and the schema version is bumped; existing rows are not
//! re-encoded, so any row written before the drop retains the column's
//! physical bytes. Reads of those rows will see trailing bytes as extra
//! tuple elements — acceptable for the "fix after drop, new writes work"
//! workflow. A full online rewrite is tracked as a separate enhancement.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::super::types::sqlstate_error;
use super::strict_schema::{load_strict_collection, persist_schema_change, write_schema_back};

/// ALTER COLLECTION <name> DROP COLUMN <column_name>
///
/// All fields arrive pre-parsed:
/// - `name`: collection name.
/// - `column_name`: column to drop.
pub async fn alter_collection_drop_column(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    column_name: &str,
) -> PgWireResult<Vec<Response>> {
    let tenant_id = identity.tenant_id;

    let (coll, mut schema) =
        load_strict_collection(state, tenant_id.as_u64(), name, "DROP COLUMN")?;

    let idx = schema
        .columns
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case(column_name))
        .ok_or_else(|| {
            sqlstate_error(
                "42703",
                &format!("column '{column_name}' does not exist on '{name}'"),
            )
        })?;

    if schema.columns[idx].primary_key {
        return Err(sqlstate_error(
            "42601",
            &format!("cannot drop primary key column '{column_name}'"),
        ));
    }

    let dropped_def = schema.columns.remove(idx);
    let new_version = schema.version.saturating_add(1);
    schema
        .dropped_columns
        .push(nodedb_types::columnar::DroppedColumn {
            def: dropped_def,
            position: idx,
            dropped_at_version: new_version,
        });
    schema.version = new_version;

    let mut updated = coll;
    write_schema_back(&mut updated, schema);
    persist_schema_change(state, &updated).await?;

    state.audit_record(
        AuditEvent::AdminAction,
        Some(tenant_id),
        &identity.username,
        &format!("ALTER COLLECTION '{name}' DROP COLUMN '{column_name}'"),
    );

    Ok(vec![Response::Execution(Tag::new("ALTER COLLECTION"))])
}
