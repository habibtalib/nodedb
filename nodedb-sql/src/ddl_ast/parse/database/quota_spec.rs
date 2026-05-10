// SPDX-License-Identifier: Apache-2.0

//! Shared `(field = value, ...)` parser for database quota specifications.
//!
//! Re-exported through `database::parse_quota_spec` because the tenant DDL
//! parser also uses it (`MOVE TENANT ... WITH QUOTA (...)`).

use nodedb_types::{PriorityClass, QuotaSpec};

use crate::error::SqlError;

/// Parse a `(field = value, ...)` clause from a raw SQL string into a [`QuotaSpec`].
///
/// Finds the first `(` after the `QUOTA` keyword, reads key=value pairs until `)`,
/// and rejects unknown keys or `=>` used instead of `=`.
pub fn parse_quota_spec(sql: &str, context: &str) -> Result<QuotaSpec, SqlError> {
    // Find the opening paren.
    let paren_start = sql.find('(').ok_or_else(|| SqlError::Parse {
        detail: format!("{context}: expected '(' before quota arguments"),
    })?;
    let after = &sql[paren_start + 1..];
    let paren_end = after.find(')').ok_or_else(|| SqlError::Parse {
        detail: format!("{context}: unterminated '(' in quota clause"),
    })?;
    let inner = &after[..paren_end];

    let mut spec = QuotaSpec::default();

    for pair in inner.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        // Reject `=>` (fat arrow used in vector kwargs) — this is `=` only.
        if pair.contains("=>") {
            return Err(SqlError::Parse {
                detail: format!(
                    "{context}: use '=' not '=>' for quota key-value pairs (near '{pair}')"
                ),
            });
        }
        let mut it = pair.splitn(2, '=');
        let key = it.next().unwrap_or("").trim().to_lowercase();
        let val = it
            .next()
            .ok_or_else(|| SqlError::Parse {
                detail: format!("{context}: expected '=' in quota pair '{pair}'"),
            })?
            .trim()
            .trim_matches('\'')
            .trim_matches('"');

        match key.as_str() {
            "max_memory_bytes" => {
                spec.max_memory_bytes = Some(val.parse::<u64>().map_err(|_| SqlError::Parse {
                    detail: format!(
                        "{context}: max_memory_bytes must be a non-negative integer, got '{val}'"
                    ),
                })?);
            }
            "max_storage_bytes" => {
                spec.max_storage_bytes = Some(val.parse::<u64>().map_err(|_| SqlError::Parse {
                    detail: format!(
                        "{context}: max_storage_bytes must be a non-negative integer, got '{val}'"
                    ),
                })?);
            }
            "max_qps" => {
                spec.max_qps = Some(val.parse::<u32>().map_err(|_| SqlError::Parse {
                    detail: format!(
                        "{context}: max_qps must be a non-negative integer, got '{val}'"
                    ),
                })?);
            }
            "max_connections" => {
                spec.max_connections = Some(val.parse::<u32>().map_err(|_| SqlError::Parse {
                    detail: format!(
                        "{context}: max_connections must be a non-negative integer, got '{val}'"
                    ),
                })?);
            }
            "cache_weight" => {
                let w = val.parse::<u32>().map_err(|_| SqlError::Parse {
                    detail: format!(
                        "{context}: cache_weight must be a positive integer, got '{val}'"
                    ),
                })?;
                if w == 0 {
                    return Err(SqlError::Parse {
                        detail: format!(
                            "{context}: cache_weight must be ≥ 1 (zero would mean \
                             no doc-cache capacity at all)"
                        ),
                    });
                }
                spec.cache_weight = Some(w);
            }
            "priority_class" => {
                let pc = val.parse::<PriorityClass>().map_err(|e| SqlError::Parse {
                    detail: format!("{context}: invalid priority_class — {e}"),
                })?;
                spec.priority_class = Some(pc);
            }
            "maintenance_cpu_pct" => {
                let pct = val.parse::<u8>().map_err(|_| SqlError::Parse {
                    detail: format!("{context}: maintenance_cpu_pct must be 0–100, got '{val}'"),
                })?;
                if pct > 100 {
                    return Err(SqlError::Parse {
                        detail: format!("{context}: maintenance_cpu_pct must be ≤ 100, got {pct}"),
                    });
                }
                spec.maintenance_cpu_pct = Some(pct);
            }
            other => {
                return Err(SqlError::Parse {
                    detail: format!(
                        "{context}: unknown quota field '{other}'. \
                         Valid fields: max_memory_bytes, max_storage_bytes, max_qps, \
                         max_connections, cache_weight, priority_class, maintenance_cpu_pct"
                    ),
                });
            }
        }
    }

    Ok(spec)
}
