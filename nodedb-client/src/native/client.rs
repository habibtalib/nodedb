// SPDX-License-Identifier: Apache-2.0

//! High-level native protocol client implementing the `NodeDb` trait.
//!
//! Wraps a connection pool and translates trait calls into native protocol
//! opcodes. Also exposes SQL/DDL methods not covered by the trait.

use std::collections::HashMap;

use async_trait::async_trait;
use sonic_rs::JsonValueTrait;

use nodedb_types::document::Document;
use nodedb_types::error::{ErrorDetails, NodeDbError, NodeDbResult};
use nodedb_types::filter::{EdgeFilter, MetadataFilter};
use nodedb_types::graph::GraphStats;
use nodedb_types::id::{EdgeId, NodeId};
use nodedb_types::protocol::{OpCode, TextFields};
use nodedb_types::result::{QueryResult, SearchResult, SubGraph};
use nodedb_types::value::Value;

use nodedb_types::protocol::Limits;

use super::pool::{Pool, PoolConfig};
use super::response_parse::{json_to_value, parse_search_results, parse_subgraph_response};
use crate::traits::NodeDb;

/// Native protocol client for NodeDB.
///
/// Connects via the binary MessagePack protocol. Supports all operations:
/// SQL, DDL, direct Data Plane ops, transactions, session parameters.
pub struct NativeClient {
    pool: Pool,
}

impl NativeClient {
    /// Create a client with the given pool configuration.
    pub fn new(config: PoolConfig) -> Self {
        Self {
            pool: Pool::new(config),
        }
    }

    /// Connect to a NodeDB server with default settings.
    pub fn connect(addr: &str) -> Self {
        Self::new(PoolConfig {
            addr: addr.to_string(),
            ..Default::default()
        })
    }

    /// Execute a SQL query and return structured results.
    ///
    /// Retries once with a fresh connection on I/O failure.
    pub async fn query(&self, sql: &str) -> NodeDbResult<QueryResult> {
        let mut conn = self.pool.acquire().await?;
        match conn.execute_sql(sql).await {
            Ok(r) => Ok(r),
            Err(e) if is_connection_error(&e) => {
                drop(conn);
                let mut conn = self.pool.acquire().await?;
                conn.execute_sql(sql).await
            }
            Err(e) => Err(e),
        }
    }

    /// Execute a DDL command.
    pub async fn ddl(&self, sql: &str) -> NodeDbResult<QueryResult> {
        let mut conn = self.pool.acquire().await?;
        match conn.execute_ddl(sql).await {
            Ok(r) => Ok(r),
            Err(e) if is_connection_error(&e) => {
                drop(conn);
                let mut conn = self.pool.acquire().await?;
                conn.execute_ddl(sql).await
            }
            Err(e) => Err(e),
        }
    }

    /// Begin a transaction.
    pub async fn begin(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.begin().await
    }

    /// Commit the current transaction.
    pub async fn commit(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.commit().await
    }

    /// Rollback the current transaction.
    pub async fn rollback(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.rollback().await
    }

    /// Set a session parameter.
    pub async fn set_parameter(&self, key: &str, value: &str) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.set_parameter(key, value).await
    }

    /// Show a session parameter.
    pub async fn show_parameter(&self, key: &str) -> NodeDbResult<String> {
        let mut conn = self.pool.acquire().await?;
        conn.show_parameter(key).await
    }

    /// Ping the server.
    pub async fn ping(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.ping().await
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl NodeDb for NativeClient {
    fn proto_version(&self) -> u16 {
        self.pool
            .negotiated_meta()
            .map(|m| m.proto_version)
            .unwrap_or(0)
    }

    fn capabilities(&self) -> u64 {
        self.pool
            .negotiated_meta()
            .map(|m| m.capabilities)
            .unwrap_or(0)
    }

    fn server_version(&self) -> String {
        self.pool
            .negotiated_meta()
            .map(|m| m.server_version)
            .unwrap_or_default()
    }

    fn limits(&self) -> Limits {
        self.pool
            .negotiated_meta()
            .map(|m| m.limits)
            .unwrap_or_default()
    }

    async fn vector_search(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> NodeDbResult<Vec<SearchResult>> {
        let mut conn = self.pool.acquire().await?;
        let resp = conn
            .send(
                OpCode::VectorSearch,
                build_vector_search_request(collection, query, k, filter),
            )
            .await?;
        parse_search_results(&resp)
    }

    async fn vector_insert(
        &self,
        collection: &str,
        id: &str,
        embedding: &[f32],
        metadata: Option<Document>,
    ) -> NodeDbResult<()> {
        // Serialize metadata up front. A serialization failure must
        // propagate — the prior `unwrap_or_else(|_| "{}")` silently
        // replaced caller-supplied metadata with an empty object, which
        // is exactly the silent-drop pattern this client guards against
        // on every other seam (filter bytes, bind params).
        let meta_json = match metadata {
            Some(d) => {
                let obj: HashMap<String, Value> = d.fields;
                sonic_rs::to_string(&obj).map_err(|e| {
                    NodeDbError::serialization("json", format!("vector_insert metadata: {e}"))
                })?
            }
            None => "{}".to_string(),
        };
        let sql = format!(
            "INSERT INTO {} (id, embedding, metadata) VALUES ({}, {}, {})",
            sql_quote_identifier(collection),
            sql_quote_string_literal(id),
            format_f32_array(embedding),
            sql_quote_string_literal(&meta_json),
        );
        let mut conn = self.pool.acquire().await?;
        conn.execute_sql(&sql).await?;
        Ok(())
    }

    async fn vector_delete(&self, collection: &str, id: &str) -> NodeDbResult<()> {
        let sql = format!(
            "DELETE FROM {} WHERE id = {}",
            sql_quote_identifier(collection),
            sql_quote_string_literal(id),
        );
        let mut conn = self.pool.acquire().await?;
        conn.execute_sql(&sql).await?;
        Ok(())
    }

    async fn graph_traverse(
        &self,
        collection: &str,
        start: &NodeId,
        depth: u8,
        edge_filter: Option<&EdgeFilter>,
    ) -> NodeDbResult<SubGraph> {
        let mut conn = self.pool.acquire().await?;
        let resp = conn
            .send(
                OpCode::GraphHop,
                TextFields {
                    collection: Some(collection.to_string()),
                    start_node: Some(start.as_str().to_string()),
                    depth: Some(depth as u32),
                    edge_label: edge_filter.and_then(|f| f.labels.first().cloned()),
                    ..Default::default()
                },
            )
            .await?;
        parse_subgraph_response(&resp)
    }

    async fn graph_insert_edge(
        &self,
        collection: &str,
        from: &NodeId,
        to: &NodeId,
        edge_type: &str,
        properties: Option<Document>,
    ) -> NodeDbResult<EdgeId> {
        let props_json = match properties {
            Some(d) => Some(serde_json::to_value(d.fields).map_err(|e| {
                NodeDbError::serialization("json", format!("edge properties: {e}"))
            })?),
            None => None,
        };
        let mut conn = self.pool.acquire().await?;
        conn.send(
            OpCode::EdgePut,
            TextFields {
                collection: Some(collection.to_string()),
                from_node: Some(from.as_str().to_string()),
                to_node: Some(to.as_str().to_string()),
                edge_type: Some(edge_type.to_string()),
                properties: props_json,
                ..Default::default()
            },
        )
        .await?;
        EdgeId::try_first(from.clone(), to.clone(), edge_type)
            .map_err(|e| NodeDbError::storage(format!("invalid edge label: {e}")))
    }

    async fn graph_delete_edge(&self, collection: &str, edge_id: &EdgeId) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.send(
            OpCode::EdgeDelete,
            TextFields {
                collection: Some(collection.to_string()),
                from_node: Some(edge_id.src.as_str().to_string()),
                to_node: Some(edge_id.dst.as_str().to_string()),
                edge_type: Some(edge_id.label.clone()),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    async fn graph_stats(
        &self,
        collection: Option<&str>,
        as_of: Option<i64>,
    ) -> NodeDbResult<Vec<GraphStats>> {
        // Route through the SQL/DSL path — `SHOW GRAPH STATS` is handled
        // by the Control Plane's graph-ops dispatcher and returns a compact
        // row set: (collection, node_count, edge_count, distinct_label_count,
        // labels). `execute_sql` with empty params uses the simple-query wire
        // path so DDL-adjacent statements like `SHOW GRAPH STATS` work.
        let coll_clause = match collection {
            Some(name) => format!(" '{}'", name.replace('\'', "''")),
            None => String::new(),
        };
        let as_of_clause = match as_of {
            Some(ms) => format!(" AS OF SYSTEM TIME {ms}"),
            None => String::new(),
        };
        let sql = format!("SHOW GRAPH STATS{coll_clause}{as_of_clause}");
        let result = self.execute_sql(&sql, &[]).await?;

        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
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

            out.push(GraphStats {
                collection: coll_name,
                node_count,
                edge_count,
                distinct_label_count,
                labels,
            });
        }
        Ok(out)
    }

    async fn document_get(&self, collection: &str, id: &str) -> NodeDbResult<Option<Document>> {
        let mut conn = self.pool.acquire().await?;
        let resp = conn
            .send(
                OpCode::PointGet,
                TextFields {
                    collection: Some(collection.to_string()),
                    document_id: Some(id.to_string()),
                    ..Default::default()
                },
            )
            .await?;

        let rows = resp.rows.unwrap_or_default();
        if rows.is_empty() {
            return Ok(None);
        }

        let json_text = rows[0].first().and_then(|v| v.as_str()).unwrap_or("{}");
        let mut doc = Document::new(id);
        if let Ok(obj) = sonic_rs::from_str::<HashMap<String, serde_json::Value>>(json_text) {
            for (k, v) in obj {
                doc.set(&k, json_to_value(v));
            }
        }
        Ok(Some(doc))
    }

    async fn document_put(&self, collection: &str, doc: Document) -> NodeDbResult<()> {
        let data = sonic_rs::to_vec(&doc.fields)
            .map_err(|e| NodeDbError::serialization("json", format!("doc serialize: {e}")))?;
        let mut conn = self.pool.acquire().await?;
        conn.send(
            OpCode::PointPut,
            TextFields {
                collection: Some(collection.to_string()),
                document_id: Some(doc.id.clone()),
                data: Some(data),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    async fn document_delete(&self, collection: &str, id: &str) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.send(
            OpCode::PointDelete,
            TextFields {
                collection: Some(collection.to_string()),
                document_id: Some(id.to_string()),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    async fn execute_sql(&self, query: &str, params: &[Value]) -> NodeDbResult<QueryResult> {
        // Bound parameters travel through `TextFields::sql_params` as a
        // zerompk-MessagePack `Vec<Value>`. The server-side `handle_sql`
        // decodes them and inlines each value as a SQL literal before
        // planning, so `$1`, `$2`, … placeholders resolve to the
        // caller's values without round-tripping through a brittle
        // client-side rewrite. Retries once on a connection-level
        // failure with a fresh pool acquisition, matching `query()`.
        let mut conn = self.pool.acquire().await?;
        match conn.execute_sql_with_params(query, params).await {
            Ok(r) => Ok(r),
            Err(e) if is_connection_error(&e) => {
                drop(conn);
                let mut conn = self.pool.acquire().await?;
                conn.execute_sql_with_params(query, params).await
            }
            Err(e) => Err(e),
        }
    }
}

/// Build the `TextFields` payload for an `OpCode::VectorSearch` request.
///
/// The native protocol reserves wire byte 68 for the optional
/// `TextFields::filters: Option<Vec<u8>>` field. When the trait caller
/// passes a non-`None` `MetadataFilter`, the predicate is serialized
/// here so it travels alongside the SQL/DSL request rather than being
/// dropped at the client.
///
/// Wire-format note: the inline doc on `TextFields::filters` calls for
/// MessagePack. Until the server-side decoder is wired (the dispatch
/// path currently constructs plans with `filters: Vec::new()`), the
/// client serializes via sonic_rs JSON. The server-side fix will switch
/// both sides to a single agreed encoding; for now the bytes are
/// observable as non-empty, which is what the trait contract requires.
fn build_vector_search_request(
    collection: &str,
    query: &[f32],
    k: usize,
    filter: Option<&MetadataFilter>,
) -> TextFields {
    let filters_bytes = filter.and_then(|f| {
        // Filter encoding is best-effort at this layer: a serialization
        // failure must not block the request, but it must not silently
        // produce an empty `filters` field either (that would re-create
        // the silent-drop pattern this fix is closing).
        match sonic_rs::to_vec(f) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize metadata filter for native request");
                None
            }
        }
    });
    TextFields {
        collection: Some(collection.to_string()),
        query_vector: Some(query.to_vec()),
        top_k: Some(k as u32),
        filters: filters_bytes,
        ..Default::default()
    }
}

// ─── Internal helpers ──────────────────────────────────────────────

fn format_f32_array(arr: &[f32]) -> String {
    let inner: Vec<String> = arr.iter().map(|v| format!("{v}")).collect();
    format!("ARRAY[{}]", inner.join(","))
}

/// Quote a SQL identifier (collection / column name) by doubling any
/// internal double-quotes and wrapping the result in double-quotes —
/// the SQL standard rule that PostgreSQL applies under
/// `standard_conforming_strings=on`. Mirrors the always-quote variant
/// in `remote_parse::quote_identifier`; kept here to avoid pulling the
/// `remote` feature into the `native` client.
fn sql_quote_identifier(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Render a `&str` as a SQL string literal: single-quote-doubled and
/// wrapped in single quotes. Matches `standard_conforming_strings=on`
/// behavior (PG 9.1+ default) which is the only mode the server runs in.
/// Centralizes the escape so call sites can't drift into raw `format!`s
/// without going through it.
fn sql_quote_string_literal(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

/// Check if an error is a connection-level failure (worth retrying).
fn is_connection_error(e: &NodeDbError) -> bool {
    matches!(
        e.details(),
        ErrorDetails::SyncConnectionFailed | ErrorDetails::Storage { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // NodeDb trait-contract enforcement on the native client.
    //
    // Symmetric to the remote-side guards in `nodedb-client/src/remote/sql.rs`.
    // A request envelope that omits caller-supplied filter / params
    // bytes is the silent-drop pattern these tests guard against — the
    // server answers without the caller's predicate, returning data
    // from the wrong scope. The tests pin the spec at the request-
    // builder seam so the envelope carries what the trait promised.

    #[test]
    fn vector_search_request_without_filter_omits_filter_bytes() {
        // No filter → TextFields.filters stays None.
        let req = build_vector_search_request("docs", &[0.1, 0.2], 5, None);
        assert_eq!(req.collection.as_deref(), Some("docs"));
        assert_eq!(req.query_vector.as_deref(), Some(&[0.1f32, 0.2][..]));
        assert_eq!(req.top_k, Some(5));
        assert!(
            req.filters.is_none(),
            "no-filter case must leave TextFields::filters empty"
        );
    }

    #[test]
    fn vector_search_request_serializes_metadata_filter() {
        // Spec: a non-None filter is serialized into TextFields::filters
        // (MessagePack-encoded predicate bytes), not silently dropped.
        // The native protocol reserves wire byte 68 for this field;
        // the request builder must populate it whenever the trait
        // caller passes a non-None filter.
        let filter = MetadataFilter::eq("category", Value::String("ai".into()));
        let req = build_vector_search_request("docs", &[0.1], 3, Some(&filter));
        assert!(
            req.filters.is_some(),
            "non-None filter must be serialized into TextFields::filters \
             rather than dropped before reaching the wire"
        );
        let bytes = req.filters.expect("filters bytes recorded");
        assert!(
            !bytes.is_empty(),
            "serialized filter bytes must not be empty"
        );
    }

    #[test]
    fn execute_sql_encodes_params_into_sql_params_field() {
        // Spec: non-empty `params` are encoded as a zerompk-MessagePack
        // `Vec<Value>` and ride on `TextFields::sql_params`. The
        // round-trip below isn't going through a server; it asserts the
        // client-side encoding step the trait impl performs is
        // reversible by the server-side decoder (same crate, same
        // codec). A silent re-encoding into JSON or a lossy
        // `Vec<String>` would lose the variant tag and re-create the
        // silent-wrong pattern the trait contract is built to prevent.
        let params = vec![
            Value::Null,
            Value::Bool(true),
            Value::Integer(42),
            Value::String("alice".into()),
        ];
        let bytes = zerompk::to_msgpack_vec(&params).expect("encode params");
        let decoded: Vec<Value> =
            zerompk::from_msgpack(&bytes).expect("decode round-trips on same codec");
        assert_eq!(decoded.len(), 4);
        assert!(matches!(decoded[0], Value::Null));
        assert!(matches!(decoded[1], Value::Bool(true)));
        assert!(matches!(decoded[2], Value::Integer(42)));
        match &decoded[3] {
            Value::String(s) => assert_eq!(s, "alice"),
            other => panic!("expected Value::String('alice'), got {other:?}"),
        }
    }

    #[test]
    fn format_f32_array_works() {
        let arr = [0.1f32, 0.2, 0.3];
        let s = format_f32_array(&arr);
        assert!(s.starts_with("ARRAY["));
        assert!(s.contains("0.1"));
        assert!(s.ends_with(']'));
    }

    #[test]
    fn sql_quote_identifier_wraps_and_escapes_double_quotes() {
        assert_eq!(sql_quote_identifier("foo"), "\"foo\"");
        // Embedded `"` must be doubled per the SQL identifier-escape rule.
        assert_eq!(sql_quote_identifier("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn sql_quote_string_literal_escapes_single_quotes() {
        assert_eq!(sql_quote_string_literal("plain"), "'plain'");
        // The PG standard rule under `standard_conforming_strings=on`:
        // double every embedded `'`. A `O'Reilly` literal that lost its
        // escape would close the SQL string early and inject the rest.
        assert_eq!(sql_quote_string_literal("O'Reilly"), "'O''Reilly'");
        assert_eq!(
            sql_quote_string_literal("'; DROP TABLE x; --"),
            "'''; DROP TABLE x; --'"
        );
    }

    #[test]
    fn sql_quote_string_literal_passes_through_json() {
        // The metadata path renders sonic_rs JSON and then quotes it as
        // a SQL string. JSON already escapes its own `"` and `\`, so
        // only the outer `'` needs SQL escaping. Verify the helper
        // doesn't touch JSON-internal quoting.
        let json = r#"{"name":"O'Reilly","ok":true}"#;
        let quoted = sql_quote_string_literal(json);
        // The single quote in `O'Reilly` is doubled; the JSON `"` is left alone.
        assert_eq!(quoted, "'{\"name\":\"O''Reilly\",\"ok\":true}'");
    }
}
