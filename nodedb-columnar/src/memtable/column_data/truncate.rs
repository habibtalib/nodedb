// SPDX-License-Identifier: Apache-2.0

//! Truncate operation on `ColumnData`: discard rows beyond a given index.

use super::types::ColumnData;

impl ColumnData {
    /// Truncate this column to `n` rows, discarding any rows beyond that point.
    ///
    /// Used by transaction rollback to restore the column to its pre-write state.
    /// Panics in debug builds if `n > self.len()`.
    pub(crate) fn truncate(&mut self, n: usize) {
        debug_assert!(
            n <= self.len(),
            "truncate({n}) exceeds column length {}",
            self.len()
        );
        match self {
            Self::Int64 { values, valid } => {
                values.truncate(n);
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Float64 { values, valid } => {
                values.truncate(n);
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Bool { values, valid } => {
                values.truncate(n);
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Timestamp { values, valid } => {
                values.truncate(n);
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Decimal { values, valid } => {
                values.truncate(n);
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Uuid { values, valid } => {
                values.truncate(n);
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::String {
                data,
                offsets,
                valid,
            } => {
                // offsets has length n_rows + 1; truncate to n+1.
                if n < offsets.len().saturating_sub(1) {
                    let byte_end = offsets[n] as usize;
                    data.truncate(byte_end);
                    offsets.truncate(n + 1);
                }
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Bytes {
                data,
                offsets,
                valid,
            } => {
                if n < offsets.len().saturating_sub(1) {
                    let byte_end = offsets[n] as usize;
                    data.truncate(byte_end);
                    offsets.truncate(n + 1);
                }
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Json {
                data,
                offsets,
                valid,
            } => {
                if n < offsets.len().saturating_sub(1) {
                    let byte_end = offsets[n] as usize;
                    data.truncate(byte_end);
                    offsets.truncate(n + 1);
                }
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Geometry {
                data,
                offsets,
                valid,
            } => {
                if n < offsets.len().saturating_sub(1) {
                    let byte_end = offsets[n] as usize;
                    data.truncate(byte_end);
                    offsets.truncate(n + 1);
                }
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::Vector { data, dim, valid } => {
                let d = *dim as usize;
                if d > 0 {
                    data.truncate(n * d);
                }
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
            Self::DictEncoded { ids, valid, .. } => {
                ids.truncate(n);
                if let Some(v) = valid {
                    v.truncate(n);
                }
            }
        }
    }
}
