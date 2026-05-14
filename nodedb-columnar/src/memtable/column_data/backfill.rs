// SPDX-License-Identifier: Apache-2.0

//! Backfill operation on `ColumnData`: fill existing rows with null/default values.

use super::types::ColumnData;

impl ColumnData {
    /// Backfill a column with null/default values for existing rows.
    pub(crate) fn backfill_nulls(&mut self, count: usize) {
        match self {
            Self::Int64 { values, valid } => {
                values.extend(std::iter::repeat_n(0i64, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Float64 { values, valid } => {
                values.extend(std::iter::repeat_n(f64::NAN, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Bool { values, valid } => {
                values.extend(std::iter::repeat_n(false, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Timestamp { values, valid } => {
                values.extend(std::iter::repeat_n(0i64, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Decimal { values, valid } => {
                values.extend(std::iter::repeat_n([0u8; 16], count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Uuid { values, valid } => {
                values.extend(std::iter::repeat_n([0u8; 16], count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::String { offsets, valid, .. } => {
                let last = *offsets.last().unwrap_or(&0);
                offsets.extend(std::iter::repeat_n(last, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Bytes { offsets, valid, .. } => {
                let last = *offsets.last().unwrap_or(&0);
                offsets.extend(std::iter::repeat_n(last, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Json { offsets, valid, .. } => {
                let last = *offsets.last().unwrap_or(&0);
                offsets.extend(std::iter::repeat_n(last, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Geometry { offsets, valid, .. } => {
                let last = *offsets.last().unwrap_or(&0);
                offsets.extend(std::iter::repeat_n(last, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::Vector { data, dim, valid } => {
                data.extend(std::iter::repeat_n(0.0f32, *dim as usize * count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
            Self::DictEncoded { ids, valid, .. } => {
                ids.extend(std::iter::repeat_n(0u32, count));
                if let Some(v) = valid {
                    v.extend(std::iter::repeat_n(false, count));
                }
            }
        }
    }
}
