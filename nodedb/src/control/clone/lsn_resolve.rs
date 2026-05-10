// SPDX-License-Identifier: BUSL-1.1

//! LSN ↔ wall-clock millisecond resolution for the clone CoW resolver.
//!
//! Converts a user-supplied `AS OF SYSTEM TIME <ms>` value to the closest
//! WAL LSN using the anchor map held by `SharedState`.  When the map is
//! empty (no anchors replayed yet) the WAL frontier is used as a safe
//! approximation — the same behaviour as the original clone handler.

use nodedb_types::Lsn;

use crate::control::state::SharedState;

/// Resolve wall-clock milliseconds to the nearest LSN.
///
/// Delegates to [`SharedState::ms_to_lsn`] which performs binary-search
/// interpolation across the anchor map, falling back to `wal.next_lsn()`
/// when the map is empty.
pub fn wall_ms_to_lsn(state: &SharedState, wall_ms: i64) -> Lsn {
    state.ms_to_lsn(wall_ms)
}
