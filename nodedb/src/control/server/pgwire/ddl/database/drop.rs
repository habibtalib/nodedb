// SPDX-License-Identifier: BUSL-1.1

//! Handler for `DROP [IF EXISTS] DATABASE <name> [CASCADE | FORCE]`.
//!
//! Rejects non-CASCADE drops when the database has collections. The built-in
//! `default` database (`DatabaseId(0)`) cannot be dropped. With `CASCADE`,
//! all collections in the database are dropped before removing the descriptor;
//! a single collection delete failure aborts the cascade with no descriptor
//! mutation, so the catalog never observes a half-dropped database.

use nodedb_types::DatabaseId;
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::catalog::{StoredCollection, SystemCatalog};
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_admin, sqlstate_error};

/// Handle `DROP [IF EXISTS] DATABASE <name> [CASCADE | FORCE]`.
///
/// `FORCE` and `CASCADE` are synonyms at the parser level — both arrive here
/// as `cascade = true`. Distinct PG-style FORCE semantics (active-session
/// termination) are out of scope; the per-database session registry that
/// would back that semantic does not yet exist, so adding a separate `force`
/// flag would be unused state, not a feature.
pub fn handle_drop_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    if_exists: bool,
    cascade: bool,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "drop databases")?;

    // `default` is immutable — cannot be dropped.
    if name.eq_ignore_ascii_case("default") {
        return Err(sqlstate_error(
            "0A000",
            "cannot drop the built-in 'default' database",
        ));
    }

    let catalog = state.credentials.catalog();
    let catalog = catalog
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    let db_id = match catalog
        .get_database_id_by_name(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
    {
        Some(id) => id,
        None => {
            if if_exists {
                return Ok(vec![Response::Execution(Tag::new("DROP DATABASE"))]);
            }
            return Err(sqlstate_error(
                "3D000",
                &format!("database '{name}' does not exist"),
            ));
        }
    };

    // Guard: `default` identity check by id (rename resilience).
    if db_id == DatabaseId::DEFAULT {
        return Err(sqlstate_error(
            "0A000",
            "cannot drop the built-in 'default' database",
        ));
    }

    // Load collections once and reuse for both the count guard and the cascade.
    let collections = catalog
        .load_all_collections(db_id)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog scan failed: {e}")))?;

    if !cascade && !collections.is_empty() {
        return Err(sqlstate_error(
            "2BP01",
            &format!(
                "database '{name}' has {} collection(s); \
                 use CASCADE to drop all collections automatically",
                collections.len()
            ),
        ));
    }

    if cascade {
        drop_all_collections_in_database(catalog, db_id, &collections)?;
    }

    catalog
        .delete_database(db_id)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog delete failed: {e}")))?;

    state.audit_record(
        crate::control::security::audit::AuditEvent::DdlChange,
        None,
        &identity.username,
        &format!("DROP DATABASE {name}"),
    );

    Ok(vec![Response::Execution(Tag::new("DROP DATABASE"))])
}

/// Drop every collection in `collections` from the catalog under `db_id`.
///
/// On the first failure the cascade aborts and the error is returned to the
/// caller. The descriptor is left intact so retrying the DROP picks up the
/// remaining collections; this is the only way to avoid a half-dropped
/// database where the descriptor is gone but the collection rows persist.
fn drop_all_collections_in_database(
    catalog: &SystemCatalog,
    db_id: DatabaseId,
    collections: &[StoredCollection],
) -> PgWireResult<()> {
    for coll in collections {
        catalog
            .delete_collection(db_id, coll.tenant_id, &coll.name)
            .map_err(|e| {
                sqlstate_error(
                    "XX000",
                    &format!(
                        "CASCADE DROP DATABASE {}: failed to delete collection '{}': {e}",
                        db_id.as_u64(),
                        coll.name
                    ),
                )
            })?;
    }
    Ok(())
}
