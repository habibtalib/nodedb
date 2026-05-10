// SPDX-License-Identifier: Apache-2.0

//! JSON response parsing for the remote client.
//!
//! The DSL paths (`SEARCH ... USING VECTOR`, graph traversal) return
//! results as JSON in a single text column. These helpers decode that
//! into typed `SearchResult` / `SubGraph` values for the trait surface.
//! Row-shaped responses (system catalog tables) go through
//! [`crate::row_decode`] instead so the remote and trait-default decoders
//! share one parser.

use std::collections::HashMap;

use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::id::{EdgeId, NodeId};
use nodedb_types::result::{SearchResult, SubGraph, SubGraphEdge, SubGraphNode};

/// Parse a JSON string from the DSL's "result" column into `Vec<SearchResult>`.
pub(super) fn parse_vector_search_json(json_text: &str) -> NodeDbResult<Vec<SearchResult>> {
    let parsed: serde_json::Value = sonic_rs::from_str(json_text)
        .map_err(|e| NodeDbError::serialization("json", e.to_string()))?;

    let mut results = Vec::new();
    if let Some(arr) = parsed.as_array() {
        for item in arr {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let distance = item.get("distance").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            results.push(SearchResult {
                id,
                node_id: None,
                distance,
                metadata: HashMap::new(),
            });
        }
    }

    Ok(results)
}

/// Parse a JSON string from graph_traverse into `SubGraph`.
pub(super) fn parse_graph_traverse_json(json_text: &str) -> NodeDbResult<SubGraph> {
    let parsed: serde_json::Value = sonic_rs::from_str(json_text)
        .map_err(|e| NodeDbError::serialization("json", e.to_string()))?;

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    if let Some(n) = parsed.get("nodes").and_then(|v| v.as_array()) {
        for item in n {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let depth = item.get("depth").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            nodes.push(SubGraphNode {
                id: NodeId::from_validated(id),
                depth,
                properties: HashMap::new(),
            });
        }
    }

    if let Some(e) = parsed.get("edges").and_then(|v| v.as_array()) {
        for item in e {
            let src = item.get("from").and_then(|v| v.as_str()).unwrap_or("");
            let dst = item.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let label = item.get("label").and_then(|v| v.as_str()).unwrap_or("");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vector_search_json_works() {
        let json = r#"[{"id":"v1","distance":0.1},{"id":"v2","distance":0.5}]"#;
        let results = parse_vector_search_json(json).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "v1");
        assert!((results[0].distance - 0.1).abs() < 0.001);
        assert_eq!(results[1].id, "v2");
    }

    #[test]
    fn parse_graph_traverse_json_works() {
        let json = r#"{
            "nodes": [{"id":"a","depth":0},{"id":"b","depth":1}],
            "edges": [{"from":"a","to":"b","label":"KNOWS"}]
        }"#;
        let sg = parse_graph_traverse_json(json).unwrap();
        assert_eq!(sg.node_count(), 2);
        assert_eq!(sg.edge_count(), 1);
        assert_eq!(sg.edges[0].label, "KNOWS");
    }

    #[test]
    fn parse_empty_search_json() {
        let results = parse_vector_search_json("[]").unwrap();
        assert!(results.is_empty());
    }
}
