// SPDX-License-Identifier: Apache-2.0

//! Dictionary encoding conversion for `ColumnData`.

use super::types::ColumnData;

impl ColumnData {
    /// Attempt to convert a `String` column to `DictEncoded`.
    pub fn try_dict_encode(col: &ColumnData, max_cardinality: u32) -> Option<ColumnData> {
        let (data, offsets, valid) = match col {
            ColumnData::String {
                data,
                offsets,
                valid,
            } => (data, offsets, valid),
            _ => return None,
        };

        let row_count = col.len();
        let mut dictionary: Vec<String> = Vec::new();
        let mut reverse: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut ids: Vec<u32> = Vec::with_capacity(row_count);

        for i in 0..row_count {
            if valid.as_ref().is_some_and(|v| !v[i]) {
                ids.push(0);
                continue;
            }
            let start = offsets[i] as usize;
            let end = offsets[i + 1] as usize;
            let s = match std::str::from_utf8(&data[start..end]) {
                Ok(s) => s,
                Err(_) => return None,
            };
            let id = if let Some(&existing) = reverse.get(s) {
                existing
            } else {
                if dictionary.len() as u32 >= max_cardinality {
                    return None;
                }
                let new_id = dictionary.len() as u32;
                dictionary.push(s.to_string());
                reverse.insert(s.to_string(), new_id);
                new_id
            };
            ids.push(id);
        }

        Some(ColumnData::DictEncoded {
            ids,
            dictionary,
            reverse,
            valid: valid.clone(),
        })
    }
}
