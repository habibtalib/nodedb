// SPDX-License-Identifier: BUSL-1.1

//! pg_catalog query interception and dispatch.
//!
//! Virtual tables are materialized as typed [`VTable`] values and then passed
//! through the in-process SQL evaluator (`vquery`) which honors WHERE,
//! aggregates, projection, ORDER BY, and LIMIT against the materialized rows.
//! The interception is a substring match on the FROM target — these
//! synthetic relations live entirely in Control-Plane state, so they are
//! never routed through the SPSC bridge to the Data Plane.

use pgwire::api::results::Response;
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::server::pgwire::types::{bool_field, int4_field, int8_field, text_field};
use crate::control::state::SharedState;

use super::audit_log::audit_log;
use super::dropped_collections::dropped_collections;
use super::l2_cleanup_queue::l2_cleanup_queue;
use super::tables;
use super::vquery::value::VType;
use super::vquery::{self, VTable};

/// Try to handle a SQL query as a pg_catalog virtual-table lookup.
///
/// Returns `Some(Ok(response))` if the query targets a known virtual table,
/// `None` if the query should fall through to the normal planner.
pub async fn try_pg_catalog(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    sql: &str,
) -> Option<PgWireResult<Vec<Response>>> {
    try_pg_catalog_with_params(state, identity, sql, &[]).await
}

/// Same as [`try_pg_catalog`] but binds prepared-statement parameters into
/// the SQL before evaluation. The extended-query path uses this so
/// `SELECT ... WHERE col = $1` honors the bound value rather than seeing
/// an unbound placeholder.
pub async fn try_pg_catalog_with_params(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    sql: &str,
    params: &[nodedb_sql::ParamValue],
) -> Option<PgWireResult<Vec<Response>>> {
    let upper = sql.to_ascii_uppercase();
    let table = extract_pg_catalog_table(&upper)?;
    Some(evaluate(state, identity, sql, table, params).await)
}

async fn evaluate(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    sql: &str,
    table: &'static str,
    params: &[nodedb_sql::ParamValue],
) -> PgWireResult<Vec<Response>> {
    let materialized: VTable = match table {
        "pg_database" => tables::pg_database()?,
        "pg_namespace" => tables::pg_namespace()?,
        "pg_type" => tables::pg_type()?,
        "pg_class" => tables::pg_class(state, identity)?,
        "pg_attribute" => tables::pg_attribute(state, identity)?,
        "pg_index" => tables::pg_index(state, identity)?,
        "pg_authid" => tables::pg_authid(state, identity)?,
        "_system.audit_log" => audit_log(state, identity)?,
        "_system.dropped_collections" => dropped_collections(state, identity).await?,
        "_system.l2_cleanup_queue" => l2_cleanup_queue(state, identity)?,
        _ => unreachable!("extract_pg_catalog_table returned an unknown name"),
    };

    let select = vquery::select::parse_select_with_params(sql, params).map_err(|e| {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".to_owned(),
            "0A000".to_owned(), // feature_not_supported
            format!("virtual table query: {e}"),
        )))
    })?;
    let result = vquery::execute(&select, materialized).map_err(|e| {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".to_owned(),
            "0A000".to_owned(),
            format!("virtual table query: {e}"),
        )))
    })?;
    vquery::encode::encode(result)
}

/// Schema reported for `table` at Parse/Describe time when the projected
/// column set cannot be computed from the SQL. Returns the *full* table
/// schema. The Execute path's response will narrow this if the actual SELECT
/// projects a subset — for the projected, parameterised case use
/// [`pg_catalog_projected_schema`] which parses the SQL.
pub fn pg_catalog_schema(table: &str) -> Option<Vec<pgwire::api::results::FieldInfo>> {
    let fields = match table {
        "pg_database" => vec![
            int8_field("oid"),
            text_field("datname"),
            text_field("datdba"),
            text_field("encoding"),
        ],
        "pg_namespace" => vec![
            int8_field("oid"),
            text_field("nspname"),
            int8_field("nspowner"),
        ],
        "pg_type" => vec![
            int8_field("oid"),
            text_field("typname"),
            int8_field("typnamespace"),
            int4_field("typlen"),
            text_field("typtype"),
        ],
        "pg_class" => vec![
            int8_field("oid"),
            text_field("relname"),
            int8_field("relnamespace"),
            text_field("relkind"),
            int8_field("relowner"),
        ],
        "pg_attribute" => vec![
            int8_field("attrelid"),
            text_field("attname"),
            int8_field("atttypid"),
            int4_field("attnum"),
            int4_field("attlen"),
            bool_field("attnotnull"),
        ],
        "pg_index" => vec![
            int8_field("indexrelid"),
            int8_field("indrelid"),
            bool_field("indisunique"),
            bool_field("indisprimary"),
        ],
        "pg_authid" => vec![
            int8_field("oid"),
            text_field("rolname"),
            bool_field("rolsuper"),
            bool_field("rolcanlogin"),
        ],
        "_system.audit_log" => vec![
            int8_field("seq"),
            int8_field("timestamp_us"),
            text_field("event"),
            int8_field("tenant_id"),
            text_field("source"),
            text_field("detail"),
            text_field("prev_hash"),
        ],
        "_system.dropped_collections" => vec![
            int8_field("tenant_id"),
            text_field("name"),
            text_field("owner"),
            text_field("engine_type"),
            int8_field("deactivated_at_ns"),
            int8_field("retention_expires_at_ns"),
            int8_field("size_bytes_estimate"),
        ],
        "_system.l2_cleanup_queue" => vec![
            int8_field("tenant_id"),
            text_field("name"),
            int8_field("purge_lsn"),
            int8_field("enqueued_at_ns"),
            int8_field("bytes_pending"),
            text_field("last_error"),
            int4_field("attempts"),
        ],
        _ => return None,
    };
    Some(fields)
}

/// Extract the first `pg_catalog.<table>` or bare `pg_<table>` reference
/// from a FROM clause. Returns the lowercase table name if found.
///
/// Matches on token boundaries so identifiers that *contain* a virtual
/// table name as a substring (e.g. a user-defined `pg_class_count_a`
/// collection) do not get mis-routed into the virtual catalog dispatcher.
pub fn extract_pg_catalog_table(upper: &str) -> Option<&'static str> {
    if contains_word(upper, "_SYSTEM.AUDIT_LOG") {
        return Some("_system.audit_log");
    }
    if contains_word(upper, "_SYSTEM.DROPPED_COLLECTIONS") {
        return Some("_system.dropped_collections");
    }
    if contains_word(upper, "_SYSTEM.L2_CLEANUP_QUEUE") {
        return Some("_system.l2_cleanup_queue");
    }
    let known = [
        "pg_database",
        "pg_namespace",
        "pg_type",
        "pg_class",
        "pg_attribute",
        "pg_index",
        "pg_authid",
    ];
    for table in &known {
        let qualified = format!("PG_CATALOG.{}", table.to_uppercase());
        let bare = table.to_uppercase();
        if contains_word(upper, &qualified) || contains_word(upper, &bare) {
            return Some(table);
        }
    }
    None
}

/// True if `needle` appears in `haystack` with non-identifier characters on
/// both sides (or at start/end of string). `.` is treated as an identifier
/// separator so qualified names like `PG_CATALOG.PG_CLASS` still split into
/// the trailing word.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(needle) {
        let pos = start + rel;
        let before_ok = pos == 0 || !is_ident_char(bytes[pos - 1]);
        let after = pos + nlen;
        let after_ok = after == bytes.len() || !is_ident_char(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = pos + 1;
        if start >= bytes.len() {
            break;
        }
    }
    false
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Parse `sql` and compute the schema of the response that the Execute
/// path will produce for the virtual table `table`. Used at Parse time so
/// Describe reports the projected columns (not the full table). Falls back
/// to the full-table schema if parsing fails.
pub fn pg_catalog_projected_schema(
    sql: &str,
    table: &str,
) -> Option<Vec<pgwire::api::results::FieldInfo>> {
    let template = schema_only_vtable(table)?;
    let select = vquery::parse_select(sql).ok()?;
    let result = vquery::execute(&select, template).ok()?;
    Some(
        result
            .columns
            .into_iter()
            .map(|c| match c.ty {
                VType::Bool => bool_field(&c.name),
                VType::Int4 => int4_field(&c.name),
                VType::Int8 => int8_field(&c.name),
                VType::Text => text_field(&c.name),
            })
            .collect(),
    )
}

/// Build a row-less [`VTable`] with the static schema of `table`. Used by
/// schema-only inference at Parse time.
fn schema_only_vtable(table: &str) -> Option<VTable> {
    use super::vquery::value::{VColumn, VType};
    fn t(cols: &[(&'static str, VType)]) -> VTable {
        VTable::new(cols.iter().map(|&(n, ty)| VColumn::new(n, ty)).collect())
    }
    Some(match table {
        "pg_database" => t(&[
            ("oid", VType::Int8),
            ("datname", VType::Text),
            ("datdba", VType::Text),
            ("encoding", VType::Text),
        ]),
        "pg_namespace" => t(&[
            ("oid", VType::Int8),
            ("nspname", VType::Text),
            ("nspowner", VType::Int8),
        ]),
        "pg_type" => t(&[
            ("oid", VType::Int8),
            ("typname", VType::Text),
            ("typnamespace", VType::Int8),
            ("typlen", VType::Int4),
            ("typtype", VType::Text),
        ]),
        "pg_class" => t(&[
            ("oid", VType::Int8),
            ("relname", VType::Text),
            ("relnamespace", VType::Int8),
            ("relkind", VType::Text),
            ("relowner", VType::Int8),
        ]),
        "pg_attribute" => t(&[
            ("attrelid", VType::Int8),
            ("attname", VType::Text),
            ("atttypid", VType::Int8),
            ("attnum", VType::Int4),
            ("attlen", VType::Int4),
            ("attnotnull", VType::Bool),
        ]),
        "pg_index" => t(&[
            ("indexrelid", VType::Int8),
            ("indrelid", VType::Int8),
            ("indisunique", VType::Bool),
            ("indisprimary", VType::Bool),
        ]),
        "pg_authid" => t(&[
            ("oid", VType::Int8),
            ("rolname", VType::Text),
            ("rolsuper", VType::Bool),
            ("rolcanlogin", VType::Bool),
        ]),
        "_system.audit_log" => t(&[
            ("seq", VType::Int8),
            ("timestamp_us", VType::Int8),
            ("event", VType::Text),
            ("tenant_id", VType::Int8),
            ("source", VType::Text),
            ("detail", VType::Text),
            ("prev_hash", VType::Text),
        ]),
        "_system.dropped_collections" => t(&[
            ("tenant_id", VType::Int8),
            ("name", VType::Text),
            ("owner", VType::Text),
            ("engine_type", VType::Text),
            ("deactivated_at_ns", VType::Int8),
            ("retention_expires_at_ns", VType::Int8),
            ("size_bytes_estimate", VType::Int8),
        ]),
        "_system.l2_cleanup_queue" => t(&[
            ("tenant_id", VType::Int8),
            ("name", VType::Text),
            ("purge_lsn", VType::Int8),
            ("enqueued_at_ns", VType::Int8),
            ("bytes_pending", VType::Int8),
            ("last_error", VType::Text),
            ("attempts", VType::Int4),
        ]),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_qualified_table() {
        let sql = "SELECT * FROM pg_catalog.pg_class WHERE relkind = 'r'";
        assert_eq!(
            extract_pg_catalog_table(&sql.to_uppercase()),
            Some("pg_class")
        );
    }

    #[test]
    fn extracts_bare_table() {
        let sql = "SELECT oid, typname FROM pg_type";
        assert_eq!(
            extract_pg_catalog_table(&sql.to_uppercase()),
            Some("pg_type")
        );
    }

    #[test]
    fn no_match_for_regular_query() {
        let sql = "SELECT * FROM users WHERE id = 1";
        assert_eq!(extract_pg_catalog_table(&sql.to_uppercase()), None);
    }

    #[test]
    fn handles_join_with_pg_catalog() {
        let sql =
            "SELECT c.oid FROM pg_class c JOIN pg_catalog.pg_namespace n ON c.relnamespace = n.oid";
        assert_eq!(
            extract_pg_catalog_table(&sql.to_uppercase()),
            Some("pg_namespace")
        );
    }
}
