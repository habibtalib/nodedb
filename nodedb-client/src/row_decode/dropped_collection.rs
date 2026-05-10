// SPDX-License-Identifier: Apache-2.0

//! Decoder for `_system.dropped_collections` rows.
//!
//! The system catalog row layout is the single source of truth for both
//! the trait default `list_dropped_collections` impl (which goes through
//! `execute_sql`) and the remote client's pgwire override. Keeping the
//! decode here means a future column reorder / addition touches one file.
//!
//! Wire column order (must match the SELECT in `NodeDb::list_dropped_collections`):
//!
//! ```text
//!   0  tenant_id              u64
//!   1  name                   string
//!   2  owner                  string (nullable → "")
//!   3  engine_type            string
//!   4  deactivated_at_ns      u64
//!   5  retention_expires_at_ns u64
//! ```

use nodedb_types::dropped_collection::DroppedCollection;
use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::value::Value;

use super::value::{value_as_string, value_as_u64};

/// Number of columns expected in a `_system.dropped_collections` row.
const EXPECTED_COLUMNS: usize = 6;

/// Decode a single row.
pub(crate) fn parse_dropped_collection_row(row: &[Value]) -> NodeDbResult<DroppedCollection> {
    if row.len() < EXPECTED_COLUMNS {
        return Err(NodeDbError::storage(format!(
            "dropped_collections row has {} columns; expected {EXPECTED_COLUMNS} \
             (tenant_id, name, owner, engine_type, deactivated_at_ns, \
             retention_expires_at_ns)",
            row.len()
        )));
    }
    Ok(DroppedCollection {
        tenant_id: value_as_u64(&row[0])?,
        name: value_as_string(&row[1])?,
        owner: value_as_string(&row[2])?,
        engine_type: value_as_string(&row[3])?,
        deactivated_at_ns: value_as_u64(&row[4])?,
        retention_expires_at_ns: value_as_u64(&row[5])?,
    })
}

/// Decode every row in `rows`. Short-circuits on the first decode error
/// so callers see the precise bad row instead of a half-populated `Vec`.
pub(crate) fn parse_dropped_collection_rows(
    rows: &[Vec<Value>],
) -> NodeDbResult<Vec<DroppedCollection>> {
    rows.iter()
        .map(|row| parse_dropped_collection_row(row))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_row() -> Vec<Value> {
        vec![
            Value::Integer(7),
            Value::String("users".into()),
            Value::String("alice".into()),
            Value::String("document_strict".into()),
            Value::Integer(1_700_000_000_000_000_000),
            Value::Integer(1_700_000_604_800_000_000),
        ]
    }

    #[test]
    fn decodes_well_formed_row() {
        let parsed = parse_dropped_collection_row(&ok_row()).unwrap();
        assert_eq!(parsed.tenant_id, 7);
        assert_eq!(parsed.name, "users");
        assert_eq!(parsed.owner, "alice");
        assert_eq!(parsed.engine_type, "document_strict");
        assert_eq!(parsed.deactivated_at_ns, 1_700_000_000_000_000_000);
        assert_eq!(parsed.retention_expires_at_ns, 1_700_000_604_800_000_000);
    }

    #[test]
    fn accepts_text_encoded_u64_columns() {
        // Simple-query path renders every column as text. The decoder
        // must accept that without forcing the remote client to special-
        // case its row shape.
        let row = vec![
            Value::String("7".into()),
            Value::String("users".into()),
            Value::String("alice".into()),
            Value::String("document_strict".into()),
            Value::String("1700000000000000000".into()),
            Value::String("1700000604800000000".into()),
        ];
        let parsed = parse_dropped_collection_row(&row).unwrap();
        assert_eq!(parsed.tenant_id, 7);
        assert_eq!(parsed.deactivated_at_ns, 1_700_000_000_000_000_000);
    }

    #[test]
    fn null_owner_projects_to_empty_string() {
        let mut row = ok_row();
        row[2] = Value::Null;
        let parsed = parse_dropped_collection_row(&row).unwrap();
        assert!(parsed.owner.is_empty());
    }

    #[test]
    fn short_row_is_rejected_with_column_count() {
        let short = vec![Value::Integer(1), Value::String("x".into())];
        let err = parse_dropped_collection_row(&short).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("2 columns"),
            "msg should cite count; got: {msg}"
        );
        assert!(
            msg.contains(&EXPECTED_COLUMNS.to_string()),
            "msg should cite expected count; got: {msg}"
        );
    }

    #[test]
    fn bulk_decode_short_circuits_on_bad_row() {
        let rows = vec![ok_row(), vec![Value::Integer(0)]];
        assert!(parse_dropped_collection_rows(&rows).is_err());
    }

    #[test]
    fn bulk_decode_returns_all_rows_on_success() {
        let rows = vec![ok_row(), ok_row(), ok_row()];
        let parsed = parse_dropped_collection_rows(&rows).unwrap();
        assert_eq!(parsed.len(), 3);
    }
}
