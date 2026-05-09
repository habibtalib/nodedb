// SPDX-License-Identifier: BUSL-1.1

//! Background materializer walker.
//!
//! For each cloned collection in `Shadowed | Materializing { .. }` state, the
//! walker is responsible for copying source rows into target storage so the
//! clone becomes self-contained, then flipping `clone_status` to
//! `Materialized` (which terminates source delegation in `clone::resolver`).
//!
//! ## Current status — gated
//!
//! Real source-to-target row copy is not yet implemented. Per-engine bulk-copy
//! plans need to be added (KV, Document, Columnar, Timeseries) and dispatched
//! through the SPSC bridge from this control-plane walker. Until that lands,
//! flipping a clone to `Materialized` would silently delete every source row
//! that was never copy-up'd into the target — a data-loss bug.
//!
//! To prevent that, this module **refuses** to advance any clone past
//! `Shadowed | Materializing` and returns a typed
//! [`crate::Error::BadRequest`] tagged with SQLSTATE `0A000`
//! (`feature_not_supported`) when called for a database that has materializable
//! collections. The CoW shadow read/write path (`clone::resolver`,
//! `clone_write_dispatch`) keeps working unchanged — only the destructive
//! status flip is gated.
//!
//! Background sweep (`run_scheduled_sweep`) treats the gating error as a
//! no-op and logs at `info` level so it does not spam the logs each tick.
//!
//! Once the real implementation lands, this module will:
//!   1. Walk the source surrogate range chunk-by-chunk.
//!   2. For each surrogate that has neither a copyup nor a tombstone, dispatch
//!      a per-engine bulk insert into target with the same surrogate.
//!   3. Persist `progress_lsn` after each chunk via Raft so a restart resumes.
//!   4. Once the full source range is copied, call the reaper to flip status
//!      and clear `cloned_from`.

use std::sync::atomic::{AtomicBool, Ordering};

use nodedb_types::{CloneStatus, DatabaseId};

use crate::control::maintenance::wrapper::{MaintenanceOutcome, with_budget};
use crate::control::security::catalog::SystemCatalog;
use crate::control::state::SharedState;

use super::progress::CloneMaterializerHandle;

/// Result of a single `materialize_database` call.
#[derive(Debug)]
pub enum MaterializeOutcome {
    /// No clone collections needed materialization (database had no live clones,
    /// or all clones were already `Materialized`).
    AllComplete,
    /// `n` collections still need materialization but the gating implementation
    /// could not advance any of them. The caller decides whether this is fatal
    /// (DDL handlers) or a logged no-op (background sweep).
    Incomplete { collections_remaining: usize },
    /// The maintenance budget was exhausted before any work could be done.
    BudgetDeferred,
    /// Cooperative shutdown signal received.
    Cancelled,
}

/// Parameters for a single materialization sweep of one database.
pub struct MaterializeParams<'a> {
    pub db_id: DatabaseId,
    pub state: &'a SharedState,
    pub catalog: &'a SystemCatalog,
    /// Cooperative cancellation flag; set to `true` by the shutdown handler.
    pub cancel: &'a AtomicBool,
    /// Optional completion handle; if `Some`, `notify_collection_done()` is
    /// called for each collection that transitions to `Materialized`.
    pub handle: Option<&'a CloneMaterializerHandle>,
    /// Estimated seconds this sweep will consume (passed to `with_budget`).
    pub estimated_secs: f64,
}

/// Perform one materialization sweep for all clone collections in `db_id`.
///
/// Called by the maintenance scheduler on each tick and by the synchronous
/// blocking wrapper. See module-level docs for the gating contract.
pub fn materialize_database(params: MaterializeParams<'_>) -> crate::Result<MaterializeOutcome> {
    if params.cancel.load(Ordering::Relaxed) {
        return Ok(MaterializeOutcome::Cancelled);
    }

    let outcome = with_budget(
        &params.state.maintenance_budget,
        params.db_id,
        params.estimated_secs,
        || do_materialize_database(&params),
    );

    match outcome {
        MaintenanceOutcome::Deferred => Ok(MaterializeOutcome::BudgetDeferred),
        MaintenanceOutcome::Ran(inner) => inner,
    }
}

/// Inner implementation, runs inside the maintenance budget window.
fn do_materialize_database(params: &MaterializeParams<'_>) -> crate::Result<MaterializeOutcome> {
    if params.cancel.load(Ordering::Relaxed) {
        return Ok(MaterializeOutcome::Cancelled);
    }

    // Load all collections in the database and filter to those needing work.
    let all_colls = params.catalog.load_all_collections(params.db_id)?;

    let to_materialize: Vec<_> = all_colls
        .into_iter()
        .filter(|c| {
            c.cloned_from.is_some()
                && matches!(
                    c.clone_status,
                    CloneStatus::Shadowed | CloneStatus::Materializing { .. }
                )
        })
        .collect();

    let pending = to_materialize.len();

    if let Some(h) = params.handle {
        h.notify_start(pending);
    }

    if pending == 0 {
        // Nothing to do; either there are no clones or every clone is already
        // `Materialized`. Both are success cases.
        return Ok(MaterializeOutcome::AllComplete);
    }

    // Real source-to-target row copy is not yet implemented (see module docs).
    // Refuse to advance status; the caller decides how to surface this.
    Ok(MaterializeOutcome::Incomplete {
        collections_remaining: pending,
    })
}

/// Drive materialization of `db_id` to completion synchronously.
///
/// Returns `Err(Error::BadRequest)` (mapped to SQLSTATE `0A000`
/// `feature_not_supported` by DDL handlers) if any clone collections still
/// need real source-to-target row copy and that path is not yet implemented.
///
/// Safe to call from sync DDL handlers that already run on a blocking thread
/// (pgwire handlers call via `spawn_blocking`).
pub fn force_materialize_blocking(
    db_id: DatabaseId,
    state: &SharedState,
    catalog: &SystemCatalog,
    handle: Option<&CloneMaterializerHandle>,
) -> crate::Result<()> {
    let cancel = AtomicBool::new(false);
    // estimated_secs=0.0 always passes the budget check (consumed + 0.0 <= cap).
    let params = MaterializeParams {
        db_id,
        state,
        catalog,
        cancel: &cancel,
        handle,
        estimated_secs: 0.0,
    };

    match do_materialize_database(&params)? {
        MaterializeOutcome::AllComplete => Ok(()),
        MaterializeOutcome::Incomplete {
            collections_remaining,
        } => Err(crate::Error::BadRequest {
            detail: format!(
                "clone materialization is not yet implemented: {collections_remaining} \
                 collection(s) in database {} still require source-to-target row copy. \
                 The CoW shadow read/write path remains functional; only MATERIALIZE \
                 and DROP DATABASE FORCE are gated until per-engine bulk copy lands.",
                db_id.as_u64()
            ),
        }),
        MaterializeOutcome::BudgetDeferred => Ok(()),
        MaterializeOutcome::Cancelled => Ok(()),
    }
}

/// Entry point called by the maintenance scheduler on each tick.
///
/// Iterates over all known databases and sweeps each that has pending clone
/// collections. Logs and skips databases where materialization is gated so
/// the sweep does not spam errors every tick.
pub fn run_scheduled_sweep(
    state: &SharedState,
    catalog: &SystemCatalog,
    cancel: &AtomicBool,
) -> crate::Result<()> {
    let database_ids: Vec<DatabaseId> = catalog
        .list_databases()?
        .into_iter()
        .map(|d| d.id)
        .collect();

    for db_id in database_ids {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        let params = MaterializeParams {
            db_id,
            state,
            catalog,
            cancel,
            handle: None,
            estimated_secs: 5.0,
        };

        match materialize_database(params)? {
            MaterializeOutcome::AllComplete => {}
            MaterializeOutcome::Incomplete {
                collections_remaining,
            } => {
                tracing::info!(
                    db_id = db_id.as_u64(),
                    collections_remaining,
                    "clone sweep skipped: per-engine row copy not yet implemented",
                );
            }
            MaterializeOutcome::BudgetDeferred => {}
            MaterializeOutcome::Cancelled => break,
        }
    }

    Ok(())
}
