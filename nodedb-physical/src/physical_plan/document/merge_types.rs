// SPDX-License-Identifier: Apache-2.0

//! Physical plan types for MERGE operations carried across the SPSC bridge.

use super::types::UpdateValue;

/// Which rows trigger a WHEN arm — physical encoding of `MergeClauseKind`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub enum MergeClauseKind {
    /// `WHEN MATCHED` — source row has a matching target row.
    Matched,
    /// `WHEN NOT MATCHED` — source row has no matching target row.
    NotMatched,
    /// `WHEN NOT MATCHED BY SOURCE` — target row has no matching source row.
    NotMatchedBySource,
}

/// The action performed when a WHEN arm fires — physical encoding.
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub enum MergeActionOp {
    /// `THEN UPDATE SET col = expr, ...`
    Update { updates: Vec<(String, UpdateValue)> },
    /// `THEN DELETE`
    Delete,
    /// `THEN INSERT (cols) VALUES (vals)` — columns and pre-encoded msgpack bytes.
    Insert {
        /// Column names in declaration order.
        columns: Vec<String>,
        /// Pre-encoded msgpack bytes for each value (parallel to `columns`).
        values: Vec<Vec<u8>>,
    },
    /// `THEN DO NOTHING`
    DoNothing,
}

/// One WHEN arm in the physical MERGE plan.
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct MergeClauseOp {
    pub kind: MergeClauseKind,
    /// Extra predicate bytes (msgpack-encoded `Vec<ScanFilter>`).
    /// Empty = unconditional arm.
    pub extra_predicate: Vec<u8>,
    pub action: MergeActionOp,
}
