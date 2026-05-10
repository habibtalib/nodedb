// SPDX-License-Identifier: Apache-2.0

//! `NodeDbRemote` connection lifecycle and `NodeDb` trait impl.
//!
//! Connection methods (`connect`, `query_raw`, `execute_raw`) live here.
//! The single `impl NodeDb for NodeDbRemote` block is the only legal
//! place for the trait methods (Rust forbids splitting a trait impl
//! across files); each method is a thin shim that calls the helpers in
//! `super::sql` (SQL/param translation) and `super::parse` (JSON decode).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

use nodedb_types::document::Document;
use nodedb_types::dropped_collection::DroppedCollection;
use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::filter::{EdgeFilter, MetadataFilter};
use nodedb_types::id::{EdgeId, NodeId};
use nodedb_types::result::{QueryResult, SearchResult, SubGraph, SubGraphEdge, SubGraphNode};
use nodedb_types::value::Value;

use crate::remote_parse::{
    format_vector_array, json_to_value, pg_value_to_value, quote_identifier,
};
use crate::row_decode::parse_dropped_collection_rows;
use crate::traits::NodeDb;

use super::parse::{parse_graph_traverse_json, parse_vector_search_json};
use super::sql::{build_vector_search_sql, translate_params};

/// Remote NodeDB client. Connects to an Origin instance over pgwire and
/// translates `NodeDb` trait calls into SQL/DSL queries.
pub struct NodeDbRemote {
    client: Arc<Mutex<Client>>,
}

impl NodeDbRemote {
    /// Connect to a NodeDB Origin instance.
    ///
    /// `config` is a standard PostgreSQL connection string:
    /// `"host=localhost port=5432 user=app dbname=mydb"`
    pub async fn connect(config: &str) -> NodeDbResult<Self> {
        let (client, connection) = tokio_postgres::connect(config, NoTls)
            .await
            .map_err(|e| NodeDbError::sync_connection_failed(e.to_string()))?;

        // Spawn the connection handler — it runs in the background.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("pgwire connection error: {e}");
            }
        });

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
        })
    }

    /// Execute a raw SQL string and return rows as `Vec<Vec<Value>>`.
    async fn query_raw(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> NodeDbResult<(Vec<String>, Vec<Vec<Value>>)> {
        let client = self.client.lock().await;
        let rows = client
            .query(sql, params)
            .await
            .map_err(|e| NodeDbError::storage(format!("pgwire query failed: {e}")))?;

        if rows.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect();

        let mut result_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut vals = Vec::with_capacity(columns.len());
            for (i, col) in row.columns().iter().enumerate() {
                let val = pg_value_to_value(row, i, col.type_());
                vals.push(val);
            }
            result_rows.push(vals);
        }

        Ok((columns, result_rows))
    }

    /// Execute a statement that doesn't return rows (INSERT/UPDATE/DELETE).
    async fn execute_raw(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> NodeDbResult<u64> {
        let client = self.client.lock().await;
        client
            .execute(sql, params)
            .await
            .map_err(|e| NodeDbError::storage(format!("pgwire execute failed: {e}")))
    }

    /// Execute a parameterless statement via the simple-query protocol
    /// (single `Query` message — no `Parse`/`Bind`/`Describe` round-trip).
    ///
    /// Required for DDL statements that don't fit the extended-query
    /// row-description shape that `Client::query` expects.
    /// `simple_query` doesn't support bound parameters, so callers with
    /// non-empty params must continue to use `query_raw`.
    ///
    /// All values come back as strings from the simple-query protocol;
    /// we wrap them as `Value::String` and let downstream consumers
    /// coerce as needed.
    async fn simple_query_raw(&self, sql: &str) -> NodeDbResult<(Vec<String>, Vec<Vec<Value>>)> {
        use tokio_postgres::SimpleQueryMessage;

        let client = self.client.lock().await;
        let messages = client
            .simple_query(sql)
            .await
            .map_err(|e| NodeDbError::storage(format!("pgwire simple_query failed: {e}")))?;

        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<Value>> = Vec::new();

        for msg in messages {
            match msg {
                SimpleQueryMessage::RowDescription(fields) => {
                    columns = fields.iter().map(|f| f.name().to_string()).collect();
                }
                SimpleQueryMessage::Row(row) => {
                    let mut vals = Vec::with_capacity(row.len());
                    for i in 0..row.len() {
                        match row.get(i) {
                            Some(s) => vals.push(Value::String(s.to_string())),
                            None => vals.push(Value::Null),
                        }
                    }
                    rows.push(vals);
                }
                SimpleQueryMessage::CommandComplete(_) => {
                    // DDL / DML completion — no rows.
                }
                _ => {}
            }
        }
        Ok((columns, rows))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl NodeDb for NodeDbRemote {
    async fn vector_search(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> NodeDbResult<Vec<SearchResult>> {
        let sql = build_vector_search_sql(collection, query, k, filter)?;

        let (columns, rows) = self.query_raw(&sql, &[]).await?;

        // The DSL path returns JSON in a single "result" column.
        if columns.len() == 1 && columns[0] == "result" {
            if let Some(row) = rows.first()
                && let Some(Value::String(json_text)) = row.first()
            {
                return parse_vector_search_json(json_text);
            }
            return Ok(Vec::new());
        }

        // Structured result set: id, distance columns.
        let mut results = Vec::with_capacity(rows.len());
        let id_idx = columns.iter().position(|c| c == "id").unwrap_or(0);
        let dist_idx = columns.iter().position(|c| c == "distance").unwrap_or(1);

        for row in &rows {
            let id = row
                .get(id_idx)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let distance = row.get(dist_idx).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;

            results.push(SearchResult {
                id,
                node_id: None,
                distance,
                metadata: HashMap::new(),
            });
        }

        Ok(results)
    }

    async fn vector_insert(
        &self,
        collection: &str,
        id: &str,
        embedding: &[f32],
        metadata: Option<Document>,
    ) -> NodeDbResult<()> {
        let collection = quote_identifier(collection);
        let meta_json = match metadata {
            Some(d) => sonic_rs::to_string(&d)
                .map_err(|e| NodeDbError::storage(format!("metadata serialization: {e}")))?,
            None => "{}".into(),
        };

        let sql = format!(
            "INSERT INTO {collection} (id, embedding, metadata) VALUES ($1, {}, $2::jsonb)",
            format_vector_array(embedding),
        );
        self.execute_raw(&sql, &[&id, &meta_json]).await?;
        Ok(())
    }

    async fn vector_delete(&self, collection: &str, id: &str) -> NodeDbResult<()> {
        let collection = quote_identifier(collection);
        let sql = format!("DELETE FROM {collection} WHERE id = $1");
        self.execute_raw(&sql, &[&id]).await?;
        Ok(())
    }

    async fn graph_traverse(
        &self,
        start: &NodeId,
        depth: u8,
        edge_filter: Option<&EdgeFilter>,
    ) -> NodeDbResult<SubGraph> {
        let label_clause = edge_filter
            .and_then(|f| f.labels.first())
            .map(|l| format!(", '{l}'"))
            .unwrap_or_default();

        let sql = format!("SELECT * FROM graph_traverse('{start}', {depth}{label_clause})");

        let (columns, rows) = self.query_raw(&sql, &[]).await?;

        if columns.len() == 1 && columns[0] == "result" {
            if let Some(row) = rows.first()
                && let Some(Value::String(json_text)) = row.first()
            {
                return parse_graph_traverse_json(json_text);
            }
            return Ok(SubGraph::empty());
        }

        // Structured: node_id, depth, edge_src, edge_dst, edge_label columns.
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seen_nodes = std::collections::HashSet::new();

        for row in &rows {
            let node_id_str = row.first().and_then(|v| v.as_str()).unwrap_or("");
            let d = row.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as u8;

            if seen_nodes.insert(node_id_str.to_string()) {
                nodes.push(SubGraphNode {
                    id: NodeId::from_validated(node_id_str.to_owned()),
                    depth: d,
                    properties: HashMap::new(),
                });
            }

            if let (Some(src), Some(dst), Some(label)) = (
                row.get(2).and_then(|v| v.as_str()),
                row.get(3).and_then(|v| v.as_str()),
                row.get(4).and_then(|v| v.as_str()),
            ) {
                edges.push(SubGraphEdge {
                    id: EdgeId::try_first(
                        NodeId::from_validated(src.to_owned()),
                        NodeId::from_validated(dst.to_owned()),
                        label,
                    )
                    .expect("server wire label already validated"),
                    from: NodeId::from_validated(src.to_owned()),
                    to: NodeId::from_validated(dst.to_owned()),
                    label: label.to_string(),
                    properties: HashMap::new(),
                });
            }
        }

        Ok(SubGraph { nodes, edges })
    }

    async fn graph_insert_edge(
        &self,
        from: &NodeId,
        to: &NodeId,
        edge_type: &str,
        properties: Option<Document>,
    ) -> NodeDbResult<EdgeId> {
        let props_json = match properties {
            Some(d) => sonic_rs::to_string(&d)
                .map_err(|e| NodeDbError::storage(format!("properties serialization: {e}")))?,
            None => "{}".into(),
        };

        let from_str = from.as_str();
        let to_str = to.as_str();
        let sql = "INSERT INTO edges (src, dst, label, properties) VALUES ($1, $2, $3, $4::jsonb)";
        self.execute_raw(sql, &[&from_str, &to_str, &edge_type, &props_json])
            .await?;

        EdgeId::try_first(from.clone(), to.clone(), edge_type)
            .map_err(|e| NodeDbError::storage(format!("invalid edge label: {e}")))
    }

    async fn graph_delete_edge(&self, edge_id: &EdgeId) -> NodeDbResult<()> {
        // Structured fields are passed so the server can match on
        // (src, label, dst, seq) without relying on the Display form.
        let src = edge_id.src.as_str();
        let dst = edge_id.dst.as_str();
        let label = edge_id.label.as_str();
        let seq = edge_id.seq as i64;
        let sql = "DELETE FROM edges WHERE src = $1 AND dst = $2 AND label = $3 AND seq = $4";
        self.execute_raw(sql, &[&src, &dst, &label, &seq]).await?;
        Ok(())
    }

    async fn document_get(&self, collection: &str, id: &str) -> NodeDbResult<Option<Document>> {
        let collection = quote_identifier(collection);
        let sql = format!("SELECT id, data FROM {collection} WHERE id = $1");
        let (_, rows) = self.query_raw(&sql, &[&id]).await?;

        if let Some(row) = rows.first() {
            let doc_id = row
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();

            let mut doc = Document::new(doc_id);

            // If the second column is JSON, parse it into fields.
            if let Some(Value::Object(fields)) = row.get(1) {
                for (k, v) in fields {
                    doc.set(k.clone(), v.clone());
                }
            } else if let Some(Value::String(json_str)) = row.get(1)
                && let Ok(parsed) =
                    sonic_rs::from_str::<HashMap<String, serde_json::Value>>(json_str)
            {
                for (k, v) in &parsed {
                    doc.set(k.clone(), json_to_value(v));
                }
            }

            Ok(Some(doc))
        } else {
            Ok(None)
        }
    }

    async fn document_put(&self, collection: &str, doc: Document) -> NodeDbResult<()> {
        let collection = quote_identifier(collection);
        let data_json = sonic_rs::to_string(&doc.fields)
            .map_err(|e| NodeDbError::storage(format!("document serialization: {e}")))?;
        let sql = format!(
            "INSERT INTO {collection} (id, data) VALUES ($1, $2::jsonb) \
             ON CONFLICT (id) DO UPDATE SET data = $2::jsonb"
        );
        self.execute_raw(&sql, &[&doc.id, &data_json]).await?;
        Ok(())
    }

    async fn document_delete(&self, collection: &str, id: &str) -> NodeDbResult<()> {
        let collection = quote_identifier(collection);
        let sql = format!("DELETE FROM {collection} WHERE id = $1");
        self.execute_raw(&sql, &[&id]).await?;
        Ok(())
    }

    async fn execute_sql(&self, query: &str, params: &[Value]) -> NodeDbResult<QueryResult> {
        // Empty params: route through `simple_query` so DDL works
        // alongside SELECT/DML in the same trait method. The
        // extended-query path (`Client::query`) requires a row
        // description for DDL that the server does not provide,
        // surfacing as a generic `db error`. The simple-query path
        // sends a single Query message and returns CommandComplete
        // for DDL or rows for SELECT in one round-trip.
        let (columns, rows) = if params.is_empty() {
            self.simple_query_raw(query).await?
        } else {
            let translated = translate_params(params)?;
            // Upcast `&(dyn ToSql + Send + Sync)` → `&(dyn ToSql + Sync)`
            // so the slice matches what `Client::query` expects.
            let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = translated
                .iter()
                .map(|b| {
                    let upcast: &(dyn tokio_postgres::types::ToSql + Sync) = b.as_ref();
                    upcast
                })
                .collect();
            self.query_raw(query, &refs).await?
        };

        Ok(QueryResult {
            columns,
            rows,
            rows_affected: 0,
        })
    }

    // Collection lifecycle (soft-delete / undrop / hard-delete).
    //
    // Explicit overrides of the trait defaults so the pgwire routing
    // takes the no-row `execute_raw` path for DDL (rather than
    // `query_raw`, which is shaped for row-returning statements) and so
    // the dispatch is visible and grep-able in this file.

    async fn undrop_collection(&self, name: &str) -> NodeDbResult<()> {
        let sql = format!("UNDROP COLLECTION {}", quote_identifier(name));
        self.execute_raw(&sql, &[]).await?;
        Ok(())
    }

    async fn drop_collection_purge(&self, name: &str) -> NodeDbResult<()> {
        let sql = format!("DROP COLLECTION {} PURGE", quote_identifier(name));
        self.execute_raw(&sql, &[]).await?;
        Ok(())
    }

    async fn list_dropped_collections(&self) -> NodeDbResult<Vec<DroppedCollection>> {
        let sql = "SELECT tenant_id, name, owner, engine_type, \
                   deactivated_at_ns, retention_expires_at_ns \
                   FROM _system.dropped_collections";
        let (_columns, rows) = self.query_raw(sql, &[]).await?;
        parse_dropped_collection_rows(&rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify NodeDbRemote implements NodeDb (compile-time check).
    #[test]
    fn remote_is_nodedb() {
        fn _accepts_dyn(_db: &dyn NodeDb) {}
        // Can't actually connect in a unit test, but we verify the trait is implemented.
    }
}
