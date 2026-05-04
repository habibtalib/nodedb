//! Document decoding helpers shared by the read paths.

use crate::data::executor::{doc_format, strict_format};

/// Decode a scanned document to raw msgpack bytes.
pub(in crate::data::executor) fn decode_scanned_document_msgpack(
    value: &[u8],
    strict_schema: Option<&nodedb_types::columnar::StrictSchema>,
) -> Vec<u8> {
    if let Some(schema) = strict_schema
        && let Some(mp) = strict_format::binary_tuple_to_msgpack(value, schema)
    {
        return mp;
    }
    doc_format::json_to_msgpack(value)
}

/// Decode a scanned document to serde_json::Value (for window functions / legacy paths).
pub(in crate::data::executor) fn decode_scanned_document(
    value: &[u8],
    strict_schema: Option<&nodedb_types::columnar::StrictSchema>,
) -> serde_json::Value {
    strict_schema
        .and_then(|schema| strict_format::binary_tuple_to_json(value, schema))
        .or_else(|| doc_format::decode_document(value))
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodedb_types::Value;
    use nodedb_types::columnar::{ColumnDef, ColumnType, StrictSchema};

    #[test]
    fn decode_scanned_document_uses_strict_schema_for_binary_tuple_rows() {
        let schema = StrictSchema {
            columns: vec![
                ColumnDef::required("id", ColumnType::String).with_primary_key(),
                ColumnDef::required("name", ColumnType::String),
                ColumnDef::nullable("age", ColumnType::Int64),
            ],
            version: 1,
            dropped_columns: Vec::new(),
            bitemporal: false,
        };
        let mut map = std::collections::HashMap::new();
        map.insert("id".into(), Value::String("u1".into()));
        map.insert("name".into(), Value::String("Ada".into()));
        map.insert("age".into(), Value::Integer(42));

        let tuple = strict_format::value_to_binary_tuple(&Value::Object(map), &schema)
            .expect("encode strict tuple");

        let decoded = decode_scanned_document(&tuple, Some(&schema));

        assert_eq!(
            decoded,
            serde_json::json!({
                "id": "u1",
                "name": "Ada",
                "age": 42
            })
        );
    }
}
