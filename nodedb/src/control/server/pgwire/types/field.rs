// SPDX-License-Identifier: BUSL-1.1

//! `FieldInfo` builders and NodeDB-type-name to pgwire `Type` mapping used
//! when constructing query result row descriptors.

use nodedb_types::columnar::ColumnType;
use pgwire::api::Type;
use pgwire::api::results::FieldFormat;
use pgwire::api::results::FieldInfo;

/// Build a FieldInfo for a text column in query results.
pub fn text_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::TEXT, FieldFormat::Text)
}

/// Build a FieldInfo for an int8 column.
pub fn int8_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::INT8, FieldFormat::Text)
}

/// Build a FieldInfo for a float8 column.
pub fn float8_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::FLOAT8, FieldFormat::Text)
}

/// Build a FieldInfo for a float4 column.
pub fn float4_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::FLOAT4, FieldFormat::Text)
}

/// Build a FieldInfo for an int4 column.
pub fn int4_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::INT4, FieldFormat::Text)
}

/// Build a FieldInfo for an int2 column.
pub fn int2_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::INT2, FieldFormat::Text)
}

/// Build a FieldInfo for a bool column.
pub fn bool_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::BOOL, FieldFormat::Text)
}

/// Build a FieldInfo for a bytea column.
pub fn bytea_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::BYTEA, FieldFormat::Text)
}

/// Build a FieldInfo for a JSON column.
pub fn json_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::JSON, FieldFormat::Text)
}

/// Build a FieldInfo for a JSONB column.
pub fn jsonb_field(name: &str) -> FieldInfo {
    FieldInfo::new(name.to_owned(), None, None, Type::JSONB, FieldFormat::Text)
}

/// Build a FieldInfo for a timestamptz column.
pub fn timestamptz_field(name: &str) -> FieldInfo {
    FieldInfo::new(
        name.to_owned(),
        None,
        None,
        Type::TIMESTAMPTZ,
        FieldFormat::Text,
    )
}

/// Build a FieldInfo for a timestamp column.
pub fn timestamp_field(name: &str) -> FieldInfo {
    FieldInfo::new(
        name.to_owned(),
        None,
        None,
        Type::TIMESTAMP,
        FieldFormat::Text,
    )
}

/// Build a FieldInfo for a varchar column.
pub fn varchar_field(name: &str) -> FieldInfo {
    FieldInfo::new(
        name.to_owned(),
        None,
        None,
        Type::VARCHAR,
        FieldFormat::Text,
    )
}

/// Build a FieldInfo for a float4 array column (vector embeddings).
pub fn float4_array_field(name: &str) -> FieldInfo {
    FieldInfo::new(
        name.to_owned(),
        None,
        None,
        Type::FLOAT4_ARRAY,
        FieldFormat::Text,
    )
}

/// Build a FieldInfo for a float8 array column.
pub fn float8_array_field(name: &str) -> FieldInfo {
    FieldInfo::new(
        name.to_owned(),
        None,
        None,
        Type::FLOAT8_ARRAY,
        FieldFormat::Text,
    )
}

/// Map a NodeDB field type name to a pgwire `Type`.
///
/// Uses `ColumnType::from_str` + `ColumnType::to_pg_oid` as the single
/// authoritative OID mapping. Falls back to `Type::TEXT` only for names that
/// cannot be parsed as a known `ColumnType` (e.g. DataFusion aliases like
/// `"int4"` or `"float8[]"`).
pub fn type_name_to_pgwire(type_name: &str) -> Type {
    // Try to parse via the canonical ColumnType mapping first.
    if let Ok(ct) = type_name.parse::<ColumnType>() {
        return Type::from_oid(ct.to_pg_oid()).unwrap_or(Type::TEXT);
    }
    // Handle DataFusion / legacy aliases that ColumnType::from_str doesn't cover.
    match type_name.to_lowercase().as_str() {
        "int" | "int4" | "integer" => Type::INT4,
        "int2" | "smallint" => Type::INT2,
        "float4" | "real" => Type::FLOAT4,
        "float8" | "double" | "double precision" => Type::FLOAT8,
        "varchar" => Type::VARCHAR,
        "timestamptz" => Type::TIMESTAMPTZ,
        s if s.starts_with("float4[]") => Type::FLOAT4_ARRAY,
        "float8[]" => Type::FLOAT8_ARRAY,
        _ => Type::TEXT,
    }
}
