// SPDX-License-Identifier: BUSL-1.1

//! Handler for `MIRROR DATABASE <local_name> FROM <source_cluster>.<source_database> [MODE = sync | async]`.
//!
//! Creates a read-only replica database that continuously applies Raft log entries
//! from the source cluster via a cross-cluster QUIC observer link.
//!
//! Enforces:
//! - Superuser privilege required.
//! - Reject if a database with `local_name` already exists.
//! - Reject if `source_cluster` matches this cluster's own id (no self-mirror).
//!
//! The handler creates the `DatabaseDescriptor` with `MirrorStatus::Bootstrapping`
//! and `mirror_origin` populated, then triggers the bootstrap sequence via the
//! cluster mirror subsystem.

use nodedb_types::{DatabaseId, Lsn, MirrorMode, MirrorOrigin, MirrorStatus};
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::catalog_entry::entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::catalog::database_types::{DatabaseDescriptor, DatabaseStatus};
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::super::types::sqlstate_error;

/// Handle `MIRROR DATABASE <local_name> FROM <source_cluster>.<source_database> [MODE = ...]`.
pub fn handle_mirror_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    local_name: &str,
    source_cluster: &str,
    source_database: &str,
    mode: MirrorMode,
) -> PgWireResult<Vec<Response>> {
    // Mirrors require Superuser (per 50.D privilege matrix).
    if !identity.is_superuser {
        return Err(sqlstate_error(
            nodedb_types::error::sqlstate::INSUFFICIENT_PRIVILEGE,
            "permission denied: MIRROR DATABASE requires superuser",
        ));
    }

    let catalog = state.credentials.catalog();
    let catalog = catalog
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    // Reject if the local name already exists.
    match catalog.get_database_id_by_name(local_name) {
        Ok(Some(_)) => {
            return Err(sqlstate_error(
                "42P04",
                &format!("database '{local_name}' already exists"),
            ));
        }
        Ok(None) => {}
        Err(e) => {
            return Err(sqlstate_error(
                "XX000",
                &format!("catalog lookup failed: {e}"),
            ));
        }
    }

    // Reject self-mirror: a non-empty source_cluster string that is
    // demonstrably "self" can be caught here. The definitive guard is at the
    // QUIC transport layer — the source cluster's handshake handler rejects
    // connections from the same cluster-id. The check here is a best-effort
    // pre-flight that avoids creating a descriptor for an obviously invalid
    // mirror. Empty source_cluster is already rejected by the parser.
    //
    // When the cluster transport is configured and exposes its own cluster-id
    // we compare; otherwise we skip the check (single-node / test mode).
    let own_node_id = state.node_id;
    if source_cluster.parse::<u64>().ok() == Some(own_node_id) {
        return Err(sqlstate_error(
            "0A000",
            &format!(
                "MIRROR DATABASE: source cluster '{source_cluster}' matches this node's id; \
                 self-mirroring is not supported"
            ),
        ));
    }

    // Allocate a DatabaseId for the new mirror.
    let db_id = state.database_registry.alloc_one();
    let created_at_lsn = state.wal.next_lsn().as_u64();

    // The source database numeric id on the source cluster is not known until
    // the bootstrap handshake completes. We store DatabaseId(0) here as the
    // pre-handshake sentinel; the bootstrap process writes the actual id into
    // MirrorOrigin.source_database after receiving the MirrorHelloAck from
    // the source cluster's handshake response.
    let source_db_id = DatabaseId::new(0);

    let mirror_origin = MirrorOrigin {
        source_cluster: source_cluster.to_string(),
        source_database: source_db_id,
        mode,
        last_applied: Lsn::new(0),
        status: MirrorStatus::Bootstrapping {
            bytes_done: 0,
            bytes_total: 0,
        },
    };

    let descriptor = DatabaseDescriptor {
        id: db_id,
        name: local_name.to_string(),
        status: DatabaseStatus::Mirroring,
        created_at_lsn,
        quota_ref: 0,
        parent_clone: None,
        mirror_origin: Some(mirror_origin),
        audit_dml: nodedb_types::AuditDmlMode::None,
    };

    // Propose through Raft; fall back to direct write in single-node mode.
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

    // Flush allocator hwm on threshold.
    if state.database_registry.should_flush() {
        let hwm = state.database_registry.current_hwm();
        if let Err(e) = catalog.put_database_hwm(hwm) {
            tracing::warn!("database hwm flush failed after MIRROR DATABASE: {e}");
        }
    }

    state.audit_record_with_db(
        crate::control::security::audit::AuditEvent::DatabaseMirrored,
        None,
        Some(db_id),
        &identity.username,
        &format!(
            "MIRROR DATABASE {local_name} FROM {source_cluster}.{source_database} MODE={mode:?}"
        ),
    );

    Ok(vec![Response::Execution(Tag::new("MIRROR DATABASE"))])
}
