// SPDX-License-Identifier: Apache-2.0

//! Single `impl NodeDb for NativeClient` block.
//!
//! Each method is a one-line delegation to an inherent helper in the
//! domain-specific sibling files. The trait impl must remain a single
//! block (Rust forbids splitting it across files).

use async_trait::async_trait;

use nodedb_types::document::Document;
use nodedb_types::error::NodeDbResult;
use nodedb_types::filter::{EdgeFilter, MetadataFilter};
use nodedb_types::graph::GraphStats;
use nodedb_types::id::{EdgeId, NodeId};
use nodedb_types::protocol::Limits;
use nodedb_types::result::{QueryResult, SearchResult, SubGraph};
use nodedb_types::value::Value;

use crate::traits::NodeDb;

use super::core::NativeClient;

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
        self.vector_search_impl(collection, query, k, filter).await
    }

    async fn vector_insert(
        &self,
        collection: &str,
        id: &str,
        embedding: &[f32],
        metadata: Option<Document>,
    ) -> NodeDbResult<()> {
        self.vector_insert_impl(collection, id, embedding, metadata)
            .await
    }

    async fn vector_delete(&self, collection: &str, id: &str) -> NodeDbResult<()> {
        self.vector_delete_impl(collection, id).await
    }

    async fn graph_traverse(
        &self,
        collection: &str,
        start: &NodeId,
        depth: u8,
        edge_filter: Option<&EdgeFilter>,
    ) -> NodeDbResult<SubGraph> {
        self.graph_traverse_impl(collection, start, depth, edge_filter)
            .await
    }

    async fn graph_insert_edge(
        &self,
        collection: &str,
        from: &NodeId,
        to: &NodeId,
        edge_type: &str,
        properties: Option<Document>,
    ) -> NodeDbResult<EdgeId> {
        self.graph_insert_edge_impl(collection, from, to, edge_type, properties)
            .await
    }

    async fn graph_delete_edge(&self, collection: &str, edge_id: &EdgeId) -> NodeDbResult<()> {
        self.graph_delete_edge_impl(collection, edge_id).await
    }

    async fn graph_stats(&self, collection: &str) -> NodeDbResult<GraphStats> {
        self.graph_stats_impl(collection).await
    }

    async fn document_get(&self, collection: &str, id: &str) -> NodeDbResult<Option<Document>> {
        self.document_get_impl(collection, id).await
    }

    async fn document_put(&self, collection: &str, doc: Document) -> NodeDbResult<()> {
        self.document_put_impl(collection, doc).await
    }

    async fn document_delete(&self, collection: &str, id: &str) -> NodeDbResult<()> {
        self.document_delete_impl(collection, id).await
    }

    async fn execute_sql(&self, query: &str, params: &[Value]) -> NodeDbResult<QueryResult> {
        self.execute_sql_impl(query, params).await
    }
}

#[cfg(test)]
mod tests {
    use nodedb_types::value::Value;

    #[test]
    fn execute_sql_encodes_params_into_sql_params_field() {
        // Spec: non-empty `params` are encoded as a zerompk-MessagePack
        // `Vec<Value>` and ride on `TextFields::sql_params`. A silent
        // re-encoding into JSON or a lossy `Vec<String>` would lose the
        // variant tag and re-create the silent-wrong pattern the trait
        // contract is built to prevent. Round-trips on the same codec
        // the server-side decoder uses.
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
}
