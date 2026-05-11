// SPDX-License-Identifier: BUSL-1.1

//! Shared propose-and-apply helper for parent-replicated DDL.
//!
//! Every `CREATE` / `ALTER` handler for the eight parent-replicated
//! object types (collection, function, procedure, trigger,
//! materialized_view, sequence, schedule, change_stream) follows the
//! same three-step ritual:
//!
//! 1. Build a `CatalogEntry::Put*(...)` variant.
//! 2. Propose it through the metadata raft group; the applier on each
//!    voter writes the primary row AND the companion `StoredOwner`
//!    row.
//! 3. If `propose_catalog_entry` returns `Ok(0)` — single-node, no
//!    metadata group installed, rolling-upgrade compat mode, or
//!    inside a DDL-transaction buffer — no applier will run on this
//!    node, so the handler must apply the entry locally to land
//!    both the primary AND the OWNERS row in redb.
//!
//! Before this helper existed, every handler open-coded those three
//! steps with slightly different error-wrapping styles. Several
//! handlers silently forgot the step-3 local apply, which is the
//! orphan-row class of bug closed by the `apply_locally_if_needed`
//! routing. Routing every handler through [`propose_and_apply`]
//! makes the step-3 omission unrepresentable.
//!
//! Returns the raft `log_index` the entry committed at (or `0` for
//! the local-apply fallback) so callers can run their own
//! single-node-only registry refresh.

use pgwire::error::PgWireResult;

use crate::control::catalog_entry::CatalogEntry;
use crate::control::catalog_entry::apply::local::apply_locally_if_needed;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::state::SharedState;

use super::super::types::sqlstate_error;

/// Propose `entry` through the metadata raft group and, when the
/// proposer reports `Ok(0)` (single-node / no-applier path), apply
/// the entry locally so the primary row and the companion
/// `StoredOwner` row both land in redb.
///
/// Returns the committed `log_index`. Callers that want to gate a
/// single-node-only side effect (in-memory registry refresh, audit
/// hook) on the local-apply path branch on `log_index == 0`; the
/// remote-apply path (`log_index > 0`) reaches the corresponding
/// applier on the same node, which already handles those side
/// effects cluster-wide.
pub fn propose_and_apply(state: &SharedState, entry: &CatalogEntry) -> PgWireResult<u64> {
    let log_index = propose_catalog_entry(state, entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    apply_locally_if_needed(state, entry, log_index);
    Ok(log_index)
}
