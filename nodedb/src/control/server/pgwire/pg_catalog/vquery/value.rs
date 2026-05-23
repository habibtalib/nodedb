// SPDX-License-Identifier: BUSL-1.1

//! Typed value representation for virtual-table rows and expression results.

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq)]
pub enum VValue {
    Null,
    Bool(bool),
    Int4(i32),
    Int8(i64),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VType {
    Bool,
    Int4,
    Int8,
    Text,
}

impl VValue {
    pub fn is_null(&self) -> bool {
        matches!(self, VValue::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            VValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            VValue::Int4(i) => Some(*i as i64),
            VValue::Int8(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            VValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// SQL three-valued comparison. Returns `None` if either side is NULL.
    pub fn sql_cmp(&self, other: &VValue) -> Option<Ordering> {
        if self.is_null() || other.is_null() {
            return None;
        }
        match (self, other) {
            (VValue::Bool(a), VValue::Bool(b)) => Some(a.cmp(b)),
            (VValue::Text(a), VValue::Text(b)) => Some(a.cmp(b)),
            (a, b) => {
                let (ai, bi) = (a.as_i64(), b.as_i64());
                match (ai, bi) {
                    (Some(x), Some(y)) => Some(x.cmp(&y)),
                    _ => None,
                }
            }
        }
    }
}

/// A single virtual-table column.
#[derive(Debug, Clone)]
pub struct VColumn {
    pub name: &'static str,
    pub ty: VType,
}

impl VColumn {
    pub const fn new(name: &'static str, ty: VType) -> Self {
        Self { name, ty }
    }
}
