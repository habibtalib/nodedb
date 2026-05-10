// SPDX-License-Identifier: BUSL-1.1

//! Audit-log SHOW commands: `SHOW AUDIT LOG`, `SHOW AUDIT WHERE`,
//! `SHOW AUDIT IN DATABASE`, and `EXPORT AUDIT`.

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, FieldInfo, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::types::{int8_field, sqlstate_error, text_field};

/// Shared schema for all audit SHOW commands.
pub(super) fn audit_schema() -> Arc<Vec<FieldInfo>> {
    Arc::new(vec![
        int8_field("seq"),
        int8_field("timestamp_us"),
        text_field("event"),
        int8_field("tenant_id"),
        int8_field("database_id"),
        text_field("source"),
        text_field("detail"),
    ])
}

/// SHOW AUDIT LOG [LIMIT <n>]
///
/// Shows recent persisted audit entries. Superuser only.
pub fn show_audit_log(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: only superuser can view audit log",
        ));
    }

    let limit = if parts.len() >= 5 && parts[3].eq_ignore_ascii_case("LIMIT") {
        parts[4].parse::<usize>().unwrap_or(100)
    } else {
        100
    };

    let catalog = match state.credentials.catalog() {
        Some(c) => c,
        None => {
            // No persistent catalog — show in-memory entries only.
            return show_audit_log_memory(state, limit);
        }
    };

    let entries = catalog
        .load_recent_audit_entries(limit)
        .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;

    let schema = audit_schema();
    let mut rows = Vec::with_capacity(entries.len());
    let mut encoder = DataRowEncoder::new(schema.clone());

    for entry in entries.iter().rev() {
        // Most recent first.
        encoder.encode_field(&(entry.seq as i64))?;
        encoder.encode_field(&(entry.timestamp_us as i64))?;
        encoder.encode_field(&entry.event)?;
        encoder.encode_field(&(entry.tenant_id.unwrap_or(0) as i64))?;
        encoder.encode_field(&(entry.database_id.unwrap_or(0) as i64))?;
        encoder.encode_field(&entry.source)?;
        encoder.encode_field(&entry.detail)?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// Show in-memory audit entries (when no persistent catalog).
fn show_audit_log_memory(state: &SharedState, limit: usize) -> PgWireResult<Vec<Response>> {
    let log = match state.audit.lock() {
        Ok(l) => l,
        Err(p) => p.into_inner(),
    };

    let schema = audit_schema();
    let all = log.all();
    let skip = if all.len() > limit {
        all.len() - limit
    } else {
        0
    };

    let mut rows = Vec::new();
    let mut encoder = DataRowEncoder::new(schema.clone());

    for entry in all.iter().skip(skip).rev() {
        encoder.encode_field(&(entry.seq as i64))?;
        encoder.encode_field(&(entry.timestamp_us as i64))?;
        encoder.encode_field(&format!("{:?}", entry.event))?;
        encoder.encode_field(&(entry.tenant_id.map_or(0i64, |t| t.as_u64() as i64)))?;
        encoder.encode_field(&(entry.database_id.map_or(0i64, |d| d.as_u64() as i64)))?;
        encoder.encode_field(&entry.source)?;
        encoder.encode_field(&entry.detail)?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// SHOW AUDIT WHERE event_type = '<snake_name>'
///
/// Filters in-memory audit entries by event type.
/// The filter value must be the snake_case event name, e.g.
/// `'permission_denied'`, `'rls_rejected'`, `'lockout_triggered'`.
pub fn show_audit_where(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: only superuser can view audit log",
        ));
    }

    // Parse: SHOW AUDIT WHERE event_type = '<value>' [LIMIT <n>]
    // parts: ["SHOW", "AUDIT", "WHERE", "event_type", "=", "'permission_denied'", ...]
    let event_filter = if parts.len() >= 6 && parts[3].eq_ignore_ascii_case("event_type") {
        parts[5].trim_matches('\'').to_ascii_lowercase()
    } else {
        return Err(sqlstate_error(
            "42601",
            "syntax: SHOW AUDIT WHERE event_type = '<event_name>' [LIMIT <n>]",
        ));
    };

    let limit = if parts.len() >= 8 && parts[6].eq_ignore_ascii_case("LIMIT") {
        parts[7].parse::<usize>().map_err(|_| {
            sqlstate_error(
                "42601",
                "syntax: SHOW AUDIT WHERE event_type = '<event_name>' [LIMIT <n>] (LIMIT must be a non-negative integer)",
            )
        })?
    } else {
        100
    };

    let log = match state.audit.lock() {
        Ok(l) => l,
        Err(p) => p.into_inner(),
    };

    let schema = audit_schema();
    let all = log.all();
    let mut rows = Vec::new();
    let mut encoder = DataRowEncoder::new(schema.clone());

    for entry in all.iter().rev() {
        if rows.len() >= limit {
            break;
        }
        if entry.event.snake_name() != event_filter {
            continue;
        }
        encoder.encode_field(&(entry.seq as i64))?;
        encoder.encode_field(&(entry.timestamp_us as i64))?;
        encoder.encode_field(&entry.event.snake_name())?;
        encoder.encode_field(&(entry.tenant_id.map_or(0i64, |t| t.as_u64() as i64)))?;
        encoder.encode_field(&(entry.database_id.map_or(0i64, |d| d.as_u64() as i64)))?;
        encoder.encode_field(&entry.source)?;
        encoder.encode_field(&entry.detail)?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// `SHOW AUDIT IN DATABASE <name> [LIMIT <n>]`
///
/// Returns all in-memory audit entries whose `database_id` matches the
/// named database. Falls back to a full-scan of the catalog when the
/// in-memory window is exhausted and a persistent catalog is available.
///
/// Superuser only.
pub fn show_audit_in_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    db_name: &str,
    limit: usize,
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: only superuser can view audit log",
        ));
    }

    let catalog = state.credentials.catalog();
    let catalog = catalog
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog unavailable"))?;

    let db_id = catalog
        .get_database_id_by_name(db_name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
        .ok_or_else(|| sqlstate_error("3D000", &format!("database '{db_name}' does not exist")))?;

    let schema = audit_schema();
    let mut rows = Vec::new();
    let mut encoder = DataRowEncoder::new(schema.clone());

    // Scan the in-memory log first.
    let log = match state.audit.lock() {
        Ok(l) => l,
        Err(p) => p.into_inner(),
    };
    for entry in log.query_by_database(db_id).into_iter().rev() {
        if rows.len() >= limit {
            break;
        }
        encoder.encode_field(&(entry.seq as i64))?;
        encoder.encode_field(&(entry.timestamp_us as i64))?;
        encoder.encode_field(&entry.event.snake_name())?;
        encoder.encode_field(&(entry.tenant_id.map_or(0i64, |t| t.as_u64() as i64)))?;
        encoder.encode_field(&(entry.database_id.map_or(0i64, |d| d.as_u64() as i64)))?;
        encoder.encode_field(&entry.source)?;
        encoder.encode_field(&entry.detail)?;
        rows.push(Ok(encoder.take_row()));
    }
    drop(log);

    // If the in-memory log didn't fill the limit, scan the catalog.
    if rows.len() < limit {
        let remaining = limit - rows.len();
        let all_entries = catalog
            .load_recent_audit_entries(remaining * 10)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        for entry in all_entries.iter().rev() {
            if rows.len() >= limit {
                break;
            }
            if entry.database_id != Some(db_id.as_u64()) {
                continue;
            }
            encoder.encode_field(&(entry.seq as i64))?;
            encoder.encode_field(&(entry.timestamp_us as i64))?;
            encoder.encode_field(&entry.event)?;
            encoder.encode_field(&(entry.tenant_id.unwrap_or(0) as i64))?;
            encoder.encode_field(&(entry.database_id.unwrap_or(0) as i64))?;
            encoder.encode_field(&entry.source)?;
            encoder.encode_field(&entry.detail)?;
            rows.push(Ok(encoder.take_row()));
        }
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// Audit entries are read with a regular `SELECT` query against
/// `system.audit_log`; the client redirects the result.
pub fn export_audit_log(
    _state: &SharedState,
    identity: &AuthenticatedIdentity,
    _parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: only superuser can export audit log",
        ));
    }
    Err(sqlstate_error(
        "0A000",
        "use `SELECT ... FROM system.audit_log` and redirect the query \
         result on the client",
    ))
}
