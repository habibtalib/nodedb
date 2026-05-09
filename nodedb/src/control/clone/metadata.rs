// SPDX-License-Identifier: BUSL-1.1

//! Metadata note attached to results when a bitemporal query pre-dates the
//! creation of the clone it targets.
//!
//! When `T_lsn < clone_created_at`, the clone did not exist at the queried
//! point in time.  The result set is empty and this note is attached so
//! callers can distinguish "no rows" from "query pre-dates the clone".

use serde::{Deserialize, Serialize};

use nodedb_types::Lsn;

/// Attached to an empty result when an `AS OF` query targets a clone at a
/// time before the clone was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClonePredicatesNote {
    /// Human-readable description, surfaced in query metadata.
    pub message: &'static str,
    /// LSN of the query (`T_lsn`).
    pub query_lsn: Lsn,
    /// LSN at which this clone was created.
    pub clone_created_at: Lsn,
}

impl ClonePredicatesNote {
    pub const MESSAGE: &'static str = "clone_predates_query_time";

    pub fn new(query_lsn: Lsn, clone_created_at: Lsn) -> Self {
        Self {
            message: Self::MESSAGE,
            query_lsn,
            clone_created_at,
        }
    }
}
