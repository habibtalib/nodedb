// SPDX-License-Identifier: Apache-2.0

//! Graph operation implementations for `NativeClient`.

use nodedb_types::document::Document;
use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::filter::EdgeFilter;
use nodedb_types::graph::GraphStats;
use nodedb_types::id::{EdgeId, NodeId};
use nodedb_types::protocol::{OpCode, TextFields};
use nodedb_types::result::SubGraph;

use super::super::response_parse::parse_subgraph_response;
use super::core::{NativeClient, sql_quote_string_literal};

impl NativeClient {
    pub(super) async fn graph_traverse_impl(
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

    pub(super) async fn graph_insert_edge_impl(
        &self,
        collection: &str,
        from: &NodeId,
        to: &NodeId,
        edge_type: &str,
        properties: Option<Document>,
    ) -> NodeDbResult<EdgeId> {
        let props_json = properties.and_then(|d| serde_json::to_value(d.fields).ok());
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

    pub(super) async fn graph_delete_edge_impl(
        &self,
        collection: &str,
        edge_id: &EdgeId,
    ) -> NodeDbResult<()> {
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

    pub(super) async fn graph_stats_impl(&self, collection: &str) -> NodeDbResult<GraphStats> {
        let sql = format!("SHOW GRAPH STATS {}", sql_quote_string_literal(collection));
        let result = self.query(&sql).await?;
        GraphStats::parse_show_stats_response(collection, &result.columns, &result.rows)
    }
}
