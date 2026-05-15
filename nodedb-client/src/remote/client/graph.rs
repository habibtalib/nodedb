// SPDX-License-Identifier: Apache-2.0

//! Graph operation implementations for `NodeDbRemote`.

use std::collections::HashMap;

use nodedb_types::document::Document;
use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::filter::EdgeFilter;
use nodedb_types::graph::GraphStats;
use nodedb_types::id::{EdgeId, NodeId};
use nodedb_types::result::{SubGraph, SubGraphEdge, SubGraphNode};
use nodedb_types::value::Value;

use super::super::parse::parse_graph_traverse_json;
use super::core::NodeDbRemote;

impl NodeDbRemote {
    pub(super) async fn graph_traverse_impl(
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

    pub(super) async fn graph_insert_edge_impl(
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

    pub(super) async fn graph_delete_edge_impl(
        &self,
        collection: &str,
        edge_id: &EdgeId,
    ) -> NodeDbResult<()> {
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

    pub(super) async fn graph_stats_impl(&self, collection: &str) -> NodeDbResult<GraphStats> {
        let coll_escaped = collection.replace('\'', "''");
        let sql = format!("SHOW GRAPH STATS '{coll_escaped}'");
        let (columns, rows) = self.simple_query_raw(&sql).await?;
        GraphStats::parse_show_stats_response(collection, &columns, &rows)
    }

    pub(super) async fn graph_shortest_path_impl(
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
}
