// SPDX-License-Identifier: BUSL-1.1

//! Handler for `ALTER DATABASE <name> PROMOTE`.
//!
//! Promotes a mirror database to a writable primary. The operation is:
//! - One-way and irreversible (no DEMOTE SQL surface exists).
//! - Idempotent: promoting an already-promoted database is a no-op.
//! - Durable: `MirrorStatus::Promoted` is written via a Raft-proposed
//!   `PutDatabase` catalog entry, so it survives restart.
//!
//! After promotion:
//! - `DatabaseDescriptor.status` is set to `Active`.
//! - `MirrorOrigin.status` is set to `MirrorStatus::Promoted`.
//! - `mirror_origin` is retained for historical lineage.
//! - The database accepts writes normally; the source observer link is torn down.
//!
//! Link teardown happens BEFORE the catalog mutation so there is no window
//! where the database is Promoted but the link is still live. If teardown
//! fails (e.g. the source is unreachable), we log and continue — the
//! operator's intent to stop following the source must succeed regardless
//! of the source's availability.
//!
//! Restart recovery: on server start, databases with `MirrorStatus::Promoted`
//! in their catalog descriptor are treated as normal writable databases.
//! The bootstrap loop skips them; they do NOT attempt to reconnect.
//!
//! Privilege gate: Superuser required (matches MIRROR DATABASE privilege level).

use nodedb_types::MirrorStatus;
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::catalog_entry::entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::catalog::database_types::DatabaseStatus;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::super::types::{require_superuser, sqlstate_error};

/// Handle `ALTER DATABASE <name> PROMOTE`.
///
/// Required role: `Superuser`.
pub fn handle_promote_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
) -> PgWireResult<Vec<Response>> {
    let catalog = state.credentials.catalog();
    let catalog = catalog
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    let db_id = catalog
        .get_database_id_by_name(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
        .ok_or_else(|| sqlstate_error("3D000", &format!("database '{name}' does not exist")))?;

    // Gate after db_id resolution so the audit record carries the database id.
    require_superuser(
        state,
        identity,
        Some(db_id),
        &format!("ALTER DATABASE {name} PROMOTE"),
    )?;

    let mut descriptor = catalog
        .get_database(db_id)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog read failed: {e}")))?
        .ok_or_else(|| sqlstate_error("XX000", &format!("database '{name}' descriptor missing")))?;

    // Idempotent: if already promoted (or Active without any mirror_origin),
    // return success immediately.
    let already_promoted = match &descriptor.mirror_origin {
        Some(origin) => matches!(origin.status, MirrorStatus::Promoted),
        None => descriptor.status == DatabaseStatus::Active,
    };
    if already_promoted {
        return Ok(vec![Response::Execution(Tag::new("ALTER DATABASE"))]);
    }

    // Reject PROMOTE on a database that is not a mirror at all
    // (not Mirroring status and has no mirror_origin).
    if descriptor.mirror_origin.is_none() && descriptor.status != DatabaseStatus::Mirroring {
        return Err(sqlstate_error(
            "0A000",
            &format!("database '{name}' is not a mirror database"),
        ));
    }

    // Tear down the cross-cluster observer link BEFORE the descriptor
    // mutation lands. This ensures there is no window where the database
    // is Promoted (writes accepted) but the observer is still streaming
    // entries from the source.
    //
    // If the teardown fails — e.g. the source is unreachable and the link
    // was never established, or it already dropped due to a disconnect —
    // we log and continue. The operator's intent is to stop following; the
    // link will be garbage-collected when the Arc reference count reaches
    // zero. The database must become writable regardless.
    state.mirror_link_registry.teardown_link(db_id);

    // Flip status to Promoted in the mirror_origin record and set
    // the database status to Active so writes are accepted.
    if let Some(ref mut origin) = descriptor.mirror_origin {
        origin.status = MirrorStatus::Promoted;
    }
    descriptor.status = DatabaseStatus::Active;

    // Persist atomically through Raft. On restart the descriptor is reloaded
    // with status=Active + origin.status=Promoted, so the database remains
    // writable without any further intervention.
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

    // The database is now writable. Clear the mirror-only catalog state so
    // it does not linger as stale data:
    //   - mirror_collection_map: source→local collection name routing used
    //     by the observer-side DDL applier; meaningless once writes are local.
    //   - mirror_lag: replication lag observed against the source; no longer
    //     advancing once the observer link is gone.
    // The descriptor's `mirror_origin` is intentionally retained as historical
    // lineage (origin cluster, mode, last applied LSN at promotion). DROP
    // DATABASE relies on this cleanup having happened — see drop.rs.
    if let Err(e) = catalog.delete_mirror_collection_map(db_id) {
        return Err(sqlstate_error(
            "XX000",
            &format!("PROMOTE: failed to clear mirror_collection_map: {e}"),
        ));
    }
    if let Err(e) = catalog.delete_mirror_lag(db_id) {
        return Err(sqlstate_error(
            "XX000",
            &format!("PROMOTE: failed to clear mirror_lag: {e}"),
        ));
    }

    state.audit_record_with_db(
        crate::control::security::audit::AuditEvent::DatabasePromoted,
        None,
        Some(db_id),
        &identity.username,
        &format!("ALTER DATABASE {name} PROMOTE"),
    );

    Ok(vec![Response::Execution(Tag::new("ALTER DATABASE"))])
}
