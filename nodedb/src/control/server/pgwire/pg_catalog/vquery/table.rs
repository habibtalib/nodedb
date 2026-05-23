// SPDX-License-Identifier: BUSL-1.1

//! Typed virtual-table representation: schema + rows.

use super::value::{VColumn, VValue};

#[derive(Debug, Clone)]
pub struct VTable {
    pub columns: Vec<VColumn>,
    pub rows: Vec<Vec<VValue>>,
}

impl VTable {
    pub fn new(columns: Vec<VColumn>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn push(&mut self, row: Vec<VValue>) {
        debug_assert_eq!(row.len(), self.columns.len());
        self.rows.push(row);
    }
}
