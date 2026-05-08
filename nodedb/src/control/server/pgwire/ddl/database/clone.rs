// SPDX-License-Identifier: BUSL-1.1

//! Handler for `CLONE DATABASE <new> FROM <source> [AS OF SYSTEM TIME <ms> | LATEST]`.
//!
//! Creates a copy-on-write snapshot of `<source>` at the requested LSN point.
//! The operation is catalog-only and returns in O(1) relative to source size.
//! Writes to the clone go to fresh target storage; reads delegate to the source
//! up to `as_of_lsn` until the background materializer completes.
//!
//! Enforces:
//!   - `MAX_CLONE_DEPTH = 8` — a chain reaching 8 hops is rejected.
//!   - Mirror detection — rejects cloning a mirror database.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use nodedb_sql::ddl_ast::CloneAsOf;
use nodedb_types::{DatabaseId, MAX_CLONE_DEPTH};

use crate::control::catalog_entry::entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::catalog::database_types::{
    DatabaseDescriptor, DatabaseStatus, ParentCloneRef,
};
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_admin, sqlstate_error};

/// Parameters for `handle_clone_database`, extracted from the parsed AST.
pub struct CloneDatabaseParams<'a> {
    pub new_name: &'a str,
    pub source_name: &'a str,
    pub as_of: &'a CloneAsOf,
}

/// Handle `CLONE DATABASE <new_name> FROM <source_name> [AS OF …]`.
pub fn handle_clone_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    params: CloneDatabaseParams<'_>,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "clone databases")?;

    let catalog = state.credentials.catalog();
    let catalog = catalog
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    // ── Resolve source database ───────────────────────────────────────────────
    let source_db_id = catalog
        .get_database_id_by_name(params.source_name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
        .ok_or_else(|| {
            sqlstate_error(
                "42P01",
                &format!("source database '{}' not found", params.source_name),
            )
        })?;

    let source_descriptor = catalog
        .get_database(source_db_id)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog read failed: {e}")))?
        .ok_or_else(|| {
            sqlstate_error(
                "42P01",
                &format!(
                    "source database '{}' descriptor missing",
                    params.source_name
                ),
            )
        })?;

    // ── Reject cloning a mirror ───────────────────────────────────────────────
    //
    // Mirror catalog entries don't exist yet in the current implementation.
    // The check below calls a helper that returns `Ok(false)` until the mirror
    // subsystem is wired; when mirrors land, this helper will inspect the
    // descriptor's status.
    if is_mirror_database(&source_descriptor) {
        return Err(sqlstate_error(
            nodedb_types::error::sqlstate::CANNOT_CLONE_MIRROR,
            &format!(
                "database '{}' is a mirror and cannot be cloned; \
                 promote it with ALTER DATABASE {} PROMOTE first",
                params.source_name, params.source_name,
            ),
        ));
    }

    // ── Enforce MAX_CLONE_DEPTH ────────────────────────────────────────────────
    let depth = clone_chain_depth(state, source_db_id)
        .map_err(|e| sqlstate_error("XX000", &format!("clone depth check failed: {e}")))?;

    if depth >= MAX_CLONE_DEPTH {
        return Err(sqlstate_error(
            nodedb_types::error::sqlstate::CLONE_DEPTH_EXCEEDED,
            &format!(
                "clone chain depth {} equals the maximum of {}; \
                 materialize a clone to flatten the chain before cloning again",
                depth, MAX_CLONE_DEPTH,
            ),
        ));
    }

    // ── Reject duplicate name ─────────────────────────────────────────────────
    match catalog.get_database_id_by_name(params.new_name) {
        Ok(Some(_)) => {
            return Err(sqlstate_error(
                "42P04",
                &format!("database '{}' already exists", params.new_name),
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

    // ── Resolve as_of LSN ─────────────────────────────────────────────────────
    //
    // For `Latest` we use the current WAL frontier as the clone point.
    //
    // For `SystemTimeMs(t)` the proper resolution is ms → LSN via the
    // `LsnMsAnchor` index built from periodically-emitted WAL anchor records
    // (record type 102). The anchor index is a separate subsystem that also
    // serves PITR (`storage::snapshot::resolve_pitr`) and bitemporal
    // `AS OF SYSTEM TIME` reads; its emitter and reader are not yet wired.
    //
    // To avoid silently lying about the snapshot point we accept `SystemTimeMs(t)`
    // only when `t` is within `NOW_TOLERANCE_MS` of wall-clock now — in that
    // window `next_lsn()` is a faithful resolution. Older timestamps are
    // rejected with a typed error pointing the caller at `AS OF LATEST` until
    // the anchor-index subsystem lands. This matches the CLAUDE.md rule that
    // genuine deferrals are surfaced explicitly, never via silent approximation.
    let now_ms = current_wall_ms()
        .map_err(|e| sqlstate_error("XX000", &format!("clock read failed: {e}")))?;
    let (as_of_lsn, as_of_ms) = match params.as_of {
        CloneAsOf::Latest => (state.wal.next_lsn(), now_ms),
        CloneAsOf::SystemTimeMs(ms) => {
            let skew = now_ms.saturating_sub(*ms);
            if skew > NOW_TOLERANCE_MS {
                return Err(sqlstate_error(
                    "0A000", // feature_not_supported
                    &format!(
                        "CLONE DATABASE AS OF SYSTEM TIME {ms} requires the LsnMsAnchor \
                         index (skew {skew}ms exceeds tolerance {NOW_TOLERANCE_MS}ms); \
                         the anchor subsystem is not yet wired — use AS OF LATEST",
                    ),
                ));
            }
            (state.wal.next_lsn(), *ms)
        }
    };

    let clone_created_at = state.wal.next_lsn();

    // ── Allocate target database id ───────────────────────────────────────────
    let target_db_id = state.database_registry.alloc_one();

    // ── Build descriptor ──────────────────────────────────────────────────────
    let target_descriptor = DatabaseDescriptor {
        id: target_db_id,
        name: params.new_name.to_string(),
        status: DatabaseStatus::Cloning,
        created_at_lsn: clone_created_at.as_u64(),
        quota_ref: source_descriptor.quota_ref,
        parent_clone: Some(ParentCloneRef {
            source_db_id,
            as_of_lsn: as_of_lsn.as_u64(),
            as_of_ms: as_of_ms as u64,
        }),
    };

    // ── Propose via Raft ──────────────────────────────────────────────────────
    let entry = CatalogEntry::CloneDatabase {
        target_descriptor: Box::new(target_descriptor.clone()),
        source_db_id: source_db_id.as_u64(),
    };

    let proposed = propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog propose failed: {e}")))?;

    // Single-node fast path (returned 0 means "no Raft, apply directly").
    //
    // Order matters for partial-failure safety: write the lineage edge first,
    // then the descriptor. If lineage succeeds and descriptor fails we roll the
    // lineage entry back — leaving no partial state. If we reversed the order,
    // a descriptor-then-lineage failure would create a clone that DROP DATABASE
    // on the source would not see as a dependent, allowing unsafe drops.
    if proposed == 0 {
        catalog
            .add_clone_child(source_db_id, target_db_id)
            .map_err(|e| sqlstate_error("XX000", &format!("lineage write failed: {e}")))?;

        if let Err(put_err) = catalog.put_database(&target_descriptor) {
            // Compensate: remove the lineage edge we just wrote. A failure here
            // is fatal — surface both errors so on-call can repair the catalog.
            if let Err(rb_err) = catalog.remove_clone_child(source_db_id, target_db_id) {
                return Err(sqlstate_error(
                    "XX000",
                    &format!(
                        "catalog write failed: {put_err}; \
                         lineage rollback ALSO failed: {rb_err} — \
                         catalog left with orphan lineage edge \
                         (source={source_db_id}, target={target_db_id})",
                    ),
                ));
            }
            return Err(sqlstate_error(
                "XX000",
                &format!("catalog write failed: {put_err}"),
            ));
        }
    }

    // Flush the allocator hwm so restarts pick up the correct next-id boundary.
    if state.database_registry.should_flush() {
        let hwm = state.database_registry.current_hwm();
        if let Err(e) = catalog.put_database_hwm(hwm) {
            tracing::warn!("database hwm flush failed after clone: {e}");
        }
    }

    state.audit_record(
        crate::control::security::audit::AuditEvent::DdlChange,
        None,
        &identity.username,
        &format!(
            "CLONE DATABASE {} FROM {} AS OF SYSTEM TIME {}",
            params.new_name, params.source_name, as_of_ms
        ),
    );

    Ok(vec![Response::Execution(Tag::new("CLONE DATABASE"))])
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Returns `true` if `descriptor` represents a mirror database.
///
/// Mirror catalog entries do not exist in the current implementation;
/// this helper will be updated to inspect `DatabaseStatus::Mirroring`
/// when the mirror subsystem is wired.  Until then it returns `false`
/// so all non-mirror paths proceed normally.
fn is_mirror_database(descriptor: &DatabaseDescriptor) -> bool {
    matches!(descriptor.status, DatabaseStatus::Mirroring)
}

/// Walk the `parent_clone` chain upward from `start_db_id`, counting hops.
/// Returns the depth (0 = no clone ancestry, 1 = direct clone, …).
///
/// The chain is bounded by `MAX_CLONE_DEPTH` — if we count more hops than
/// that we short-circuit and return `MAX_CLONE_DEPTH + 1` so the caller's
/// `>= MAX_CLONE_DEPTH` guard fires.
fn clone_chain_depth(state: &SharedState, start_db_id: DatabaseId) -> crate::Result<u32> {
    let catalog = state.credentials.catalog();
    let catalog = catalog.as_ref().ok_or(crate::Error::Storage {
        engine: "catalog".into(),
        detail: "system catalog unavailable for depth check".into(),
    })?;

    let mut current = start_db_id;
    let mut depth: u32 = 0;

    loop {
        if depth > MAX_CLONE_DEPTH {
            return Ok(depth);
        }
        let desc = catalog
            .get_database(current)
            .map_err(|e| crate::Error::Storage {
                engine: "catalog".into(),
                detail: format!("depth walk get_database failed: {e}"),
            })?;
        match desc.and_then(|d| d.parent_clone) {
            None => return Ok(depth),
            Some(parent) => {
                current = parent.source_db_id;
                depth += 1;
            }
        }
    }
}

/// Tolerance window in which `AS OF SYSTEM TIME ms` resolves faithfully to
/// `next_lsn()`. A timestamp older than this requires the LsnMsAnchor index.
const NOW_TOLERANCE_MS: i64 = 1_000;

/// Current wall-clock milliseconds since Unix epoch.
///
/// Returns `Err` if the system clock is set before the Unix epoch — caller
/// must surface the failure rather than silently substituting a sentinel.
fn current_wall_ms() -> crate::Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .map_err(|e| crate::Error::Internal {
            detail: format!("clone_database: system clock predates Unix epoch: {e}"),
        })
}
