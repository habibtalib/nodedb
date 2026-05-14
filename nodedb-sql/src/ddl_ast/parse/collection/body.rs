// SPDX-License-Identifier: Apache-2.0

//! Parse the body of `CREATE COLLECTION` / `CREATE TABLE` after the name.

use super::column_list::extract_column_pairs;
use super::with_clause::{extract_balanced_raw, extract_with_options};
use crate::error::SqlError;

/// Parsed body of a `CREATE COLLECTION` / `CREATE TABLE` statement.
///
/// Tuple shape: `(engine, columns, options, flags, balanced_raw)`:
/// - `engine`: value of `engine=` from the WITH clause (lowercased), if present.
/// - `columns`: `(name, type)` pairs from the parenthesised column list.
/// - `options`: remaining WITH clause `key=value` pairs (excluding `engine`).
/// - `flags`: free-standing modifier keywords: `APPEND_ONLY`, `HASH_CHAIN`, `BITEMPORAL`.
/// - `balanced_raw`: raw interior of the `BALANCED ON (...)` clause, or `None`.
pub(super) type CollectionBody = (
    Option<String>,
    Vec<(String, String)>,
    Vec<(String, String)>,
    Vec<String>,
    Option<String>,
);

pub(super) fn parse_collection_body(trimmed: &str, name: &str) -> Result<CollectionBody, SqlError> {
    // Skip past the name to find the body.
    let lower = trimmed.to_lowercase();
    let name_lower = name.to_lowercase();
    let body = if let Some(pos) = lower.find(&name_lower) {
        trimmed[pos + name.len()..].trim()
    } else {
        return Ok((None, Vec::new(), Vec::new(), Vec::new(), None));
    };

    let upper_body = body.to_uppercase();

    let columns = extract_column_pairs(body)?;
    let (engine, options) = extract_with_options(body);

    let mut flags: Vec<String> = Vec::new();
    if upper_body.contains("APPEND_ONLY") {
        flags.push("APPEND_ONLY".to_string());
    }
    if upper_body.contains("HASH_CHAIN") {
        flags.push("HASH_CHAIN".to_string());
    }
    if upper_body.contains("BITEMPORAL") {
        flags.push("BITEMPORAL".to_string());
    }

    let balanced_raw = extract_balanced_raw(&upper_body, body);

    Ok((engine, columns, options, flags, balanced_raw))
}
