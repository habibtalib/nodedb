// SPDX-License-Identifier: BUSL-1.1

use nodedb_sql::types::{EngineType, SqlExpr, SqlValue};
use nodedb_types::Surrogate;

use crate::bridge::envelope::PhysicalPlan;
use crate::bridge::physical_plan::ColumnarInsertIntent;
use crate::bridge::physical_plan::*;
use crate::types::{DatabaseId, TenantId, VShardId};

use super::super::super::physical::{PhysicalTask, PostSetOp};
use super::super::convert::ConvertContext;
use super::super::value::{
    assignments_to_update_values, row_to_msgpack, rows_to_msgpack_array, sql_value_to_string,
};

pub(super) fn assign_for_pk(
    ctx: &ConvertContext,
    collection: &str,
    pk_bytes: &[u8],
) -> crate::Result<Surrogate> {
    match ctx.surrogate_assigner.as_ref() {
        Some(a) => a.assign(collection, pk_bytes),
        None => Ok(Surrogate::ZERO),
    }
}

pub(super) fn columnar_row_surrogates(
    ctx: &ConvertContext,
    collection: &str,
    columnar_rows: &[&Vec<(String, SqlValue)>],
) -> crate::Result<Vec<Surrogate>> {
    let mut out = Vec::with_capacity(columnar_rows.len());
    for row in columnar_rows {
        let pk = row
            .iter()
            .find(|(k, _)| k == "id" || k == "document_id" || k == "key")
            .map(|(_, v)| sql_value_to_string(v))
            .unwrap_or_default();
        if pk.is_empty() {
            out.push(Surrogate::ZERO);
        } else {
            out.push(assign_for_pk(ctx, collection, pk.as_bytes())?);
        }
    }
    Ok(out)
}

pub(in super::super) fn nodedb_value_to_sql(val: nodedb_types::Value) -> SqlValue {
    match val {
        nodedb_types::Value::Integer(n) => SqlValue::Int(n),
        nodedb_types::Value::Float(f) => SqlValue::Float(f),
        nodedb_types::Value::String(s) => SqlValue::String(s),
        nodedb_types::Value::Bool(b) => SqlValue::Bool(b),
        nodedb_types::Value::Null => SqlValue::Null,
        _ => SqlValue::String(format!("{val:?}")),
    }
}

pub(in super::super) fn convert_insert(
    collection: &str,
    engine: &EngineType,
    rows: &[Vec<(String, SqlValue)>],
    column_defaults: &[(String, String)],
    if_absent: bool,
    tenant_id: TenantId,
    ctx: &ConvertContext,
) -> crate::Result<Vec<PhysicalTask>> {
    let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, collection);
    let mut tasks = Vec::new();
    let mut columnar_rows: Vec<&Vec<(String, SqlValue)>> = Vec::new();

    let mut expanded_rows: Vec<Vec<(String, SqlValue)>> = Vec::with_capacity(rows.len());
    for row in rows {
        if column_defaults.is_empty() {
            expanded_rows.push(row.clone());
            continue;
        }
        let mut expanded = row.clone();
        for (col_name, default_expr) in column_defaults {
            if !expanded.iter().any(|(k, _)| k == col_name)
                && let Some(val) = super::super::value::evaluate_default_expr(default_expr)
                    .map_err(|e| crate::Error::PlanError {
                        detail: format!("default for column '{col_name}': {e}"),
                    })?
            {
                expanded.push((col_name.clone(), nodedb_value_to_sql(val)));
            }
        }
        expanded_rows.push(expanded);
    }

    for (i, row) in expanded_rows.iter().enumerate() {
        let doc_id = row
            .iter()
            .find(|(k, _)| k == "id" || k == "document_id" || k == "key")
            .map(|(_, v)| sql_value_to_string(v))
            .unwrap_or_default();

        match engine {
            EngineType::KeyValue => {
                return Err(crate::Error::PlanError {
                    detail: "KV INSERT must use SqlPlan::KvInsert path".into(),
                });
            }
            EngineType::Timeseries => {
                return Err(crate::Error::PlanError {
                    detail: format!(
                        "INSERT into '{collection}': timeseries collections use TimeseriesIngest, not Insert"
                    ),
                });
            }
            EngineType::Columnar | EngineType::Spatial => {
                columnar_rows.push(&rows[i]);
            }
            EngineType::DocumentSchemaless | EngineType::DocumentStrict => {
                let value_bytes = row_to_msgpack(row)?;
                let surrogate = assign_for_pk(ctx, collection, doc_id.as_bytes())?;
                tasks.push(PhysicalTask {
                    tenant_id,
                    vshard_id: vshard,
                    database_id: crate::types::DatabaseId::DEFAULT,
                    plan: PhysicalPlan::Document(DocumentOp::PointInsert {
                        collection: collection.into(),
                        document_id: doc_id,
                        value: value_bytes,
                        if_absent,
                        surrogate,
                    }),
                    post_set_op: PostSetOp::None,
                });
            }
            EngineType::Array => {
                return Err(crate::Error::PlanError {
                    detail: format!(
                        "INSERT into '{collection}': array engine uses INSERT INTO ARRAY syntax"
                    ),
                });
            }
        }
    }

    if !columnar_rows.is_empty() {
        let payload = rows_to_msgpack_array(&columnar_rows, column_defaults)?;
        let intent = if if_absent {
            ColumnarInsertIntent::InsertIfAbsent
        } else {
            ColumnarInsertIntent::Insert
        };
        let surrogates = columnar_row_surrogates(ctx, collection, &columnar_rows)?;
        tasks.push(PhysicalTask {
            tenant_id,
            vshard_id: vshard,
            database_id: crate::types::DatabaseId::DEFAULT,
            plan: PhysicalPlan::Columnar(ColumnarOp::Insert {
                collection: collection.into(),
                payload,
                format: "msgpack".into(),
                intent,
                on_conflict_updates: Vec::new(),
                surrogates,
            }),
            post_set_op: PostSetOp::None,
        });
    }

    Ok(tasks)
}

pub(in super::super) fn convert_upsert(
    collection: &str,
    engine: &EngineType,
    rows: &[Vec<(String, SqlValue)>],
    column_defaults: &[(String, String)],
    on_conflict_updates: &[(String, SqlExpr)],
    tenant_id: TenantId,
    ctx: &ConvertContext,
) -> crate::Result<Vec<PhysicalTask>> {
    let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, collection);
    let mut tasks = Vec::new();

    let on_conflict_values = if on_conflict_updates.is_empty() {
        Vec::new()
    } else {
        assignments_to_update_values(on_conflict_updates)?
    };

    let mut columnar_rows: Vec<&Vec<(String, SqlValue)>> = Vec::new();

    for row in rows {
        let doc_id = row
            .iter()
            .find(|(k, _)| k == "id" || k == "document_id" || k == "key")
            .map(|(_, v)| sql_value_to_string(v))
            .unwrap_or_default();

        match engine {
            EngineType::DocumentSchemaless | EngineType::DocumentStrict => {
                let value_bytes = row_to_msgpack(row)?;
                let surrogate = assign_for_pk(ctx, collection, doc_id.as_bytes())?;
                tasks.push(PhysicalTask {
                    tenant_id,
                    vshard_id: vshard,
                    database_id: crate::types::DatabaseId::DEFAULT,
                    plan: PhysicalPlan::Document(DocumentOp::Upsert {
                        collection: collection.into(),
                        document_id: doc_id,
                        value: value_bytes,
                        on_conflict_updates: on_conflict_values.clone(),
                        surrogate,
                    }),
                    post_set_op: PostSetOp::None,
                });
            }
            EngineType::Columnar | EngineType::Spatial => {
                columnar_rows.push(row);
            }
            EngineType::Timeseries | EngineType::KeyValue | EngineType::Array => {
                return Err(crate::Error::PlanError {
                    detail: format!(
                        "UPSERT into '{collection}': engine type {engine:?} does not support upsert"
                    ),
                });
            }
        }
    }

    if !columnar_rows.is_empty() {
        let payload = rows_to_msgpack_array(&columnar_rows, column_defaults)?;
        let surrogates = columnar_row_surrogates(ctx, collection, &columnar_rows)?;
        tasks.push(PhysicalTask {
            tenant_id,
            vshard_id: vshard,
            database_id: crate::types::DatabaseId::DEFAULT,
            plan: PhysicalPlan::Columnar(ColumnarOp::Insert {
                collection: collection.into(),
                payload,
                format: "msgpack".into(),
                intent: ColumnarInsertIntent::Put,
                on_conflict_updates: on_conflict_values,
                surrogates,
            }),
            post_set_op: PostSetOp::None,
        });
    }

    Ok(tasks)
}
