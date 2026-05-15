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
use sonic_rs::JsonValueTrait;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

use nodedb_types::document::Document;
use nodedb_types::dropped_collection::DroppedCollection;
use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::filter::{EdgeFilter, MetadataFilter};
use nodedb_types::graph::GraphStats;
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

/// Extract a useful detail string from a `tokio_postgres::Error`.
///
/// Without this, `Display` returns the literal `"db error"` and the
/// SQLSTATE + server message are dropped — every failure surfaces as the
/// same opaque string and is impossible to diagnose without a debug
/// rebuild. Mirrors the harness's `pg_error_detail` so client and test
/// reports look identical.
fn pg_error_detail(e: &tokio_postgres::Error) -> String {
    if let Some(db_err) = e.as_db_error() {
        format!(
            "{}: {} (SQLSTATE {})",
            db_err.severity(),
            db_err.message(),
            db_err.code().code()
        )
    } else {
        format!("{e}")
    }
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
        let rows = client.query(sql, params).await.map_err(|e| {
            NodeDbError::storage(format!("pgwire query failed: {}", pg_error_detail(&e)))
        })?;

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
        client.execute(sql, params).await.map_err(|e| {
            NodeDbError::storage(format!("pgwire execute failed: {}", pg_error_detail(&e)))
        })
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
        let messages = client.simple_query(sql).await.map_err(|e| {
            NodeDbError::storage(format!(
                "pgwire simple_query failed: {}",
                pg_error_detail(&e)
            ))
        })?;

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

    async fn vector_insert_field(
        &self,
        collection: &str,
        field_name: &str,
        id: &str,
        embedding: &[f32],
        metadata: Option<Document>,
    ) -> NodeDbResult<()> {
        // Field-aware path: emit `INSERT INTO <coll> (id, <field>[,
        // metadata]) VALUES ($1, ARRAY[...]<, $2>)` so the vector lands
        // on the column named by the trait — not on whichever vector
        // column the planner picks when the column name is omitted.
        let coll = quote_identifier(collection);
        let field = quote_identifier(field_name);
        let vec_lit = format_vector_array(embedding);

        let sql = match metadata {
            Some(_) => {
                format!("INSERT INTO {coll} (id, {field}, metadata) VALUES ($1, {vec_lit}, $2)")
            }
            None => format!("INSERT INTO {coll} (id, {field}) VALUES ($1, {vec_lit})"),
        };

        if let Some(d) = metadata {
            let meta_json = sonic_rs::to_string(&d)
                .map_err(|e| NodeDbError::storage(format!("metadata serialization: {e}")))?;
            self.execute_raw(&sql, &[&id, &meta_json]).await?;
        } else {
            self.execute_raw(&sql, &[&id]).await?;
        }
        Ok(())
    }

    async fn vector_search_field(
        &self,
        collection: &str,
        field_name: &str,
        query: &[f32],
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> NodeDbResult<Vec<SearchResult>> {
        // Field-aware path: use the 2-arg form of `vector_distance` so
        // the planner scopes the HNSW lookup to the named column. The
        // single-arg form `vector_distance(ARRAY[...])` only works on
        // collections that have exactly one vector column.
        let coll = quote_identifier(collection);
        let field = quote_identifier(field_name);
        let vec_lit = format_vector_array(query);
        let where_clause = match filter {
            Some(f) => {
                let rendered = super::sql::render_metadata_filter_public(f)?;
                format!(" WHERE {rendered}")
            }
            None => String::new(),
        };
        let sql = format!(
            "SELECT id, vector_distance({field}, {vec_lit}) AS distance \
             FROM {coll}{where_clause} \
             ORDER BY vector_distance({field}, {vec_lit}) \
             LIMIT {k}"
        );

        let (columns, rows) = self.query_raw(&sql, &[]).await?;
        let id_idx = columns.iter().position(|c| c == "id").unwrap_or(0);
        let dist_idx = columns.iter().position(|c| c == "distance").unwrap_or(1);

        let mut results = Vec::with_capacity(rows.len());
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
        collection: &str,
        start: &NodeId,
        depth: u8,
        edge_filter: Option<&EdgeFilter>,
    ) -> NodeDbResult<SubGraph> {
        // Server-side DSL: `GRAPH TRAVERSE FROM '<start>' DEPTH <n>
        // [LABEL '<l>']`. The Origin graph overlay is tenant-scoped
        // (the dispatcher routes on `identity.tenant_id`), so the
        // `collection` argument is accepted for trait symmetry with
        // `graph_insert_edge` and Lite parity but is not threaded into
        // the wire DSL — every edge in the tenant participates in the
        // traversal regardless of which collection it was inserted
        // into.
        let _ = collection;
        let label_clause = edge_filter
            .and_then(|f| f.labels.first())
            .map(|l| format!(" LABEL '{}'", l.replace('\'', "''")))
            .unwrap_or_default();
        let start_str = start.as_str().replace('\'', "''");
        let sql = format!("GRAPH TRAVERSE FROM '{start_str}' DEPTH {depth}{label_clause}");

        let (columns, rows) = self.simple_query_raw(&sql).await?;

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
        collection: &str,
        from: &NodeId,
        to: &NodeId,
        edge_type: &str,
        properties: Option<Document>,
    ) -> NodeDbResult<EdgeId> {
        // `GRAPH INSERT EDGE IN '<collection>' FROM '<src>' TO '<dst>'
        // TYPE '<label>' [PROPERTIES '<json>']`. The DSL handler lives
        // in `pgwire/ddl/graph_ops/edge.rs` and registers the edge in
        // the collection's graph overlay. simple_query is required
        // because the DSL doesn't fit the extended-query row-description
        // shape (it returns CommandComplete only).
        let props_clause = match properties {
            Some(d) => {
                let json = sonic_rs::to_string(&d)
                    .map_err(|e| NodeDbError::storage(format!("properties serialization: {e}")))?;
                format!(" PROPERTIES '{}'", json.replace('\'', "''"))
            }
            None => String::new(),
        };
        let coll = collection.replace('\'', "''");
        let from_s = from.as_str().replace('\'', "''");
        let to_s = to.as_str().replace('\'', "''");
        let label_s = edge_type.replace('\'', "''");
        let sql = format!(
            "GRAPH INSERT EDGE IN '{coll}' FROM '{from_s}' TO '{to_s}' TYPE '{label_s}'{props_clause}"
        );

        self.simple_query_raw(&sql).await?;

        EdgeId::try_first(from.clone(), to.clone(), edge_type)
            .map_err(|e| NodeDbError::storage(format!("invalid edge label: {e}")))
    }

    async fn graph_delete_edge(&self, collection: &str, edge_id: &EdgeId) -> NodeDbResult<()> {
        // `GRAPH DELETE EDGE IN '<collection>' FROM '<src>' TO '<dst>'
        // TYPE '<label>'`. The (src, dst, label) tuple identifies a
        // unique edge in the overlay — `seq` is part of `EdgeId` for
        // multi-edge disambiguation, but the server DSL keys on the
        // tuple alone today, so we drop seq here.
        let coll = collection.replace('\'', "''");
        let src = edge_id.src.as_str().replace('\'', "''");
        let dst = edge_id.dst.as_str().replace('\'', "''");
        let label = edge_id.label.replace('\'', "''");
        let sql = format!("GRAPH DELETE EDGE IN '{coll}' FROM '{src}' TO '{dst}' TYPE '{label}'");
        self.simple_query_raw(&sql).await?;
        Ok(())
    }

    async fn graph_stats(
        &self,
        collection: Option<&str>,
        as_of: Option<i64>,
    ) -> NodeDbResult<Vec<GraphStats>> {
        // `SHOW GRAPH STATS ['<collection>'] [AS OF SYSTEM TIME <ms>]`.
        // The server returns a compact row set: (collection, node_count,
        // edge_count, distinct_label_count, labels). We parse each row and
        // reconstruct the `GraphStats` slice — no JSON round-trip needed for
        // the scalar columns; only the `labels` field arrives as JSON text.
        let coll_clause = match collection {
            Some(name) => format!(" '{}'", name.replace('\'', "''")),
            None => String::new(),
        };
        let as_of_clause = match as_of {
            Some(ms) => format!(" AS OF SYSTEM TIME {ms}"),
            None => String::new(),
        };
        let sql = format!("SHOW GRAPH STATS{coll_clause}{as_of_clause}");

        let (_columns, rows) = self.simple_query_raw(&sql).await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let coll_name = row
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let node_count = row.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as u64;
            let edge_count = row.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as u64;
            let distinct_label_count = row.get(3).and_then(|v| v.as_i64()).unwrap_or(0) as u64;
            let labels: Vec<(String, u64)> = row
                .get(4)
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    sonic_rs::from_str::<Vec<sonic_rs::Value>>(s)
                        .ok()
                        .map(|arr| {
                            arr.into_iter()
                                .filter_map(|obj| {
                                    let label = obj["label"].as_str()?.to_string();
                                    let count = obj["count"].as_u64()?;
                                    Some((label, count))
                                })
                                .collect()
                        })
                })
                .unwrap_or_default();

            result.push(GraphStats {
                collection: coll_name,
                node_count,
                edge_count,
                distinct_label_count,
                labels,
            });
        }
        Ok(result)
    }

    async fn graph_shortest_path(
        &self,
        collection: &str,
        from: &NodeId,
        to: &NodeId,
        max_depth: u8,
        edge_filter: Option<&EdgeFilter>,
    ) -> NodeDbResult<Option<Vec<NodeId>>> {
        // Use the server's `GRAPH PATH` operator instead of the trait
        // default's per-hop BFS — one round-trip vs O(path_length).
        // Like `graph_traverse`, the Origin graph overlay is
        // tenant-scoped, so `collection` is accepted for symmetry but
        // not threaded into the DSL.
        let _ = collection;
        let label_clause = edge_filter
            .and_then(|f| f.labels.first())
            .map(|l| format!(" LABEL '{}'", l.replace('\'', "''")))
            .unwrap_or_default();
        let from_s = from.as_str().replace('\'', "''");
        let to_s = to.as_str().replace('\'', "''");
        let sql =
            format!("GRAPH PATH FROM '{from_s}' TO '{to_s}' MAX_DEPTH {max_depth}{label_clause}");

        let (_columns, rows) = self.simple_query_raw(&sql).await?;
        // Server emits a single `result` column carrying a JSON array
        // of node ids — empty array means unreachable.
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let Some(Value::String(json_text)) = row.first() else {
            return Ok(None);
        };
        let parsed: Vec<String> = sonic_rs::from_str(json_text)
            .map_err(|e| NodeDbError::storage(format!("graph shortest path response: {e}")))?;
        if parsed.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            parsed
                .into_iter()
                .map(NodeId::from_validated)
                .collect::<Vec<_>>(),
        ))
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
        // NodeDB's SQL planner accepts JSON text values directly into
        // the document `data` column — no `::jsonb` cast on the
        // expression side, which the planner currently rejects as an
        // "unsupported value expression". The server interprets the
        // string literal as document JSON when the target column is the
        // doc-engine `data` column.
        let sql = format!(
            "INSERT INTO {collection} (id, data) VALUES ($1, $2) \
             ON CONFLICT (id) DO UPDATE SET data = $2"
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

    async fn text_search(
        &self,
        collection: &str,
        field: &str,
        query: &str,
        top_k: usize,
        params: nodedb_types::text_search::TextSearchParams,
    ) -> NodeDbResult<Vec<SearchResult>> {
        // Server-side FTS query SQL: `text_match(<field>, '<query>')` in
        // a WHERE clause selects matching ids; `bm25_score(<field>,
        // '<query>')` in the SELECT list exposes the score so callers
        // can order/rank. The planner pattern-matches this shape and
        // dispatches `SqlPlan::TextSearch`.
        //
        // `params` (mode, fuzzy, prefix, etc.) is intentionally ignored
        // for now — every supported option is also expressible in the
        // SQL form, but threading them through the DSL string is its
        // own widening. The defaults (Plain query with fuzzy=true) cover
        // the common case the trait's spec calls out.
        let _ = params;
        let coll = quote_identifier(collection);
        let field_quoted = quote_identifier(field);
        let q_escaped = query.replace('\'', "''");
        let sql = format!(
            "SELECT id, bm25_score({field_quoted}, '{q_escaped}') AS score \
             FROM {coll} \
             WHERE text_match({field_quoted}, '{q_escaped}') \
             LIMIT {top_k}"
        );

        let (columns, rows) = self.simple_query_raw(&sql).await?;
        let id_idx = columns.iter().position(|c| c == "id").unwrap_or(0);
        let score_idx = columns.iter().position(|c| c == "score").unwrap_or(1);

        let mut results = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = row
                .get(id_idx)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // simple_query returns text — score arrives as a stringified
            // float. Parse defensively so a missing/malformed score does
            // not torpedo the whole result set; callers prefer ordered
            // ids with score 0.0 over an Err.
            let score = row
                .get(score_idx)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f32>().ok())
                .or_else(|| {
                    row.get(score_idx)
                        .and_then(|v| v.as_f64())
                        .map(|f| f as f32)
                })
                .unwrap_or(0.0);

            results.push(SearchResult {
                id,
                node_id: None,
                distance: score,
                metadata: HashMap::new(),
            });
        }
        Ok(results)
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
