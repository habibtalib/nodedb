// SPDX-License-Identifier: BUSL-1.1

//! Virtual table materializers for each pg_catalog table. Each function
//! returns a [`VTable`] which the query evaluator (`vquery::execute`)
//! then filters / projects / aggregates / sorts according to the client
//! SELECT.

use nodedb_types::DatabaseId;

use nodedb_types::columnar::ColumnType;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::server::pgwire::pg_catalog::oid::{stable_collection_oid, stable_index_oid};
use crate::control::server::pgwire::pg_catalog::vquery::VTable;
use crate::control::server::pgwire::pg_catalog::vquery::value::{VColumn, VType, VValue};
use crate::control::state::SharedState;
use pgwire::error::PgWireResult;

pub fn pg_database() -> PgWireResult<VTable> {
    let mut t = VTable::new(vec![
        VColumn::new("oid", VType::Int8),
        VColumn::new("datname", VType::Text),
        VColumn::new("datdba", VType::Text),
        VColumn::new("encoding", VType::Text),
    ]);
    t.push(vec![
        VValue::Int8(1),
        VValue::Text("nodedb".into()),
        VValue::Text("nodedb".into()),
        VValue::Text("UTF8".into()),
    ]);
    Ok(t)
}

pub fn pg_namespace() -> PgWireResult<VTable> {
    let mut t = VTable::new(vec![
        VColumn::new("oid", VType::Int8),
        VColumn::new("nspname", VType::Text),
        VColumn::new("nspowner", VType::Int8),
    ]);
    t.push(vec![
        VValue::Int8(11),
        VValue::Text("pg_catalog".into()),
        VValue::Int8(10),
    ]);
    t.push(vec![
        VValue::Int8(2200),
        VValue::Text("public".into()),
        VValue::Int8(10),
    ]);
    Ok(t)
}

pub fn pg_type() -> PgWireResult<VTable> {
    let mut t = VTable::new(vec![
        VColumn::new("oid", VType::Int8),
        VColumn::new("typname", VType::Text),
        VColumn::new("typnamespace", VType::Int8),
        VColumn::new("typlen", VType::Int4),
        VColumn::new("typtype", VType::Text),
    ]);
    let types: &[(i64, &str, i32, &str)] = &[
        (16, "bool", 1, "b"),
        (17, "bytea", -1, "b"),
        (20, "int8", 8, "b"),
        (21, "int2", 2, "b"),
        (23, "int4", 4, "b"),
        (25, "text", -1, "b"),
        (114, "json", -1, "b"),
        (700, "float4", 4, "b"),
        (701, "float8", 8, "b"),
        (1021, "float4[]", -1, "b"),
        (1043, "varchar", -1, "b"),
        (1082, "date", 4, "b"),
        (1114, "timestamp", 8, "b"),
        (1184, "timestamptz", 8, "b"),
        (1186, "interval", 16, "b"),
        (1700, "numeric", -1, "b"),
        (2950, "uuid", 16, "b"),
        (3802, "jsonb", -1, "b"),
    ];
    for &(oid, name, len, typtype) in types {
        t.push(vec![
            VValue::Int8(oid),
            VValue::Text(name.into()),
            VValue::Int8(11),
            VValue::Int4(len),
            VValue::Text(typtype.into()),
        ]);
    }
    Ok(t)
}

pub fn pg_class(state: &SharedState, identity: &AuthenticatedIdentity) -> PgWireResult<VTable> {
    let mut t = VTable::new(vec![
        VColumn::new("oid", VType::Int8),
        VColumn::new("relname", VType::Text),
        VColumn::new("relnamespace", VType::Int8),
        VColumn::new("relkind", VType::Text),
        VColumn::new("relowner", VType::Int8),
    ]);
    for coll in load_collections(state, identity) {
        let oid = stable_collection_oid(coll.tenant_id, &coll.name);
        t.push(vec![
            VValue::Int8(oid),
            VValue::Text(coll.name.clone()),
            VValue::Int8(2200),
            VValue::Text("r".into()),
            VValue::Int8(10),
        ]);
    }
    Ok(t)
}

pub fn pg_attribute(state: &SharedState, identity: &AuthenticatedIdentity) -> PgWireResult<VTable> {
    let mut t = VTable::new(vec![
        VColumn::new("attrelid", VType::Int8),
        VColumn::new("attname", VType::Text),
        VColumn::new("atttypid", VType::Int8),
        VColumn::new("attnum", VType::Int4),
        VColumn::new("attlen", VType::Int4),
        VColumn::new("attnotnull", VType::Bool),
    ]);
    for coll in load_collections(state, identity) {
        let rel_oid = stable_collection_oid(coll.tenant_id, &coll.name);
        for (col_num, (field_name, field_type)) in coll.fields.iter().enumerate() {
            let type_oid = field_type_to_oid(field_type);
            t.push(vec![
                VValue::Int8(rel_oid),
                VValue::Text(field_name.clone()),
                VValue::Int8(type_oid),
                VValue::Int4((col_num + 1) as i32),
                VValue::Int4(-1),
                VValue::Bool(false),
            ]);
        }
    }
    Ok(t)
}

pub fn pg_index(state: &SharedState, identity: &AuthenticatedIdentity) -> PgWireResult<VTable> {
    let mut t = VTable::new(vec![
        VColumn::new("indexrelid", VType::Int8),
        VColumn::new("indrelid", VType::Int8),
        VColumn::new("indisunique", VType::Bool),
        VColumn::new("indisprimary", VType::Bool),
    ]);
    for coll in load_collections(state, identity) {
        let indrelid = stable_collection_oid(coll.tenant_id, &coll.name);
        for index in &coll.indexes {
            let indexrelid = stable_index_oid(coll.tenant_id, &coll.name, &index.name);
            t.push(vec![
                VValue::Int8(indexrelid),
                VValue::Int8(indrelid),
                VValue::Bool(index.unique),
                VValue::Bool(false),
            ]);
        }
    }
    Ok(t)
}

pub fn pg_authid(state: &SharedState, identity: &AuthenticatedIdentity) -> PgWireResult<VTable> {
    let mut t = VTable::new(vec![
        VColumn::new("oid", VType::Int8),
        VColumn::new("rolname", VType::Text),
        VColumn::new("rolsuper", VType::Bool),
        VColumn::new("rolcanlogin", VType::Bool),
    ]);
    let users = state.credentials.list_users();
    for (i, user) in users.iter().enumerate() {
        let oid = 10i64 + i as i64;
        let is_super = identity.is_superuser && user == &identity.username;
        t.push(vec![
            VValue::Int8(oid),
            VValue::Text(user.clone()),
            VValue::Bool(is_super),
            VValue::Bool(true),
        ]);
    }
    Ok(t)
}

fn load_collections(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> Vec<crate::control::security::catalog::types::StoredCollection> {
    let Some(catalog) = state.credentials.catalog() else {
        return Vec::new();
    };
    if identity.is_superuser {
        catalog
            .load_all_collections(DatabaseId::DEFAULT)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.is_active)
            .collect()
    } else {
        catalog
            .load_collections_for_tenant(DatabaseId::DEFAULT, identity.tenant_id.as_u64())
            .unwrap_or_default()
    }
}

fn field_type_to_oid(field_type: &str) -> i64 {
    if let Ok(ct) = field_type.parse::<ColumnType>() {
        return ct.to_pg_oid() as i64;
    }
    match field_type.to_lowercase().as_str() {
        "int" | "integer" | "int4" => 23,
        "smallint" | "int2" => 21,
        "float" | "float4" | "real" => 700,
        "double" | "float8" => 701,
        "varchar" => 1043,
        "date" => 1082,
        "timestamptz" => 1184,
        _ => 25,
    }
}
