// SPDX-License-Identifier: Apache-2.0

//! Graph DDL/DML statements.

use crate::ddl_ast::graph_types::{GraphDirection, GraphProperties};

#[derive(Debug, Clone, PartialEq)]
pub enum GraphStmt {
    // ── Graph DSL ────────────────────────────────────────────────
    GraphInsertEdge {
        collection: String,
        src: String,
        dst: String,
        label: String,
        properties: GraphProperties,
    },
    GraphDeleteEdge {
        collection: String,
        src: String,
        dst: String,
        label: String,
    },
    GraphSetLabels {
        node_id: String,
        labels: Vec<String>,
        remove: bool,
    },
    GraphTraverse {
        start: String,
        depth: usize,
        edge_label: Option<String>,
        direction: GraphDirection,
    },
    GraphNeighbors {
        node: String,
        edge_label: Option<String>,
        direction: GraphDirection,
    },
    GraphPath {
        src: String,
        dst: String,
        max_depth: usize,
        edge_label: Option<String>,
    },
    GraphAlgo {
        algorithm: String,
        collection: String,
        edge_label: Option<String>,
        damping: Option<f64>,
        tolerance: Option<f64>,
        resolution: Option<f64>,
        max_iterations: Option<usize>,
        sample_size: Option<usize>,
        source_node: Option<String>,
        direction: Option<String>,
        mode: Option<String>,
    },
    /// `MATCH (x)-[:l]->(y) RETURN x, y` — body forwarded verbatim to the graph pattern compiler.
    MatchQuery { body: String },
    /// `GRAPH RAG FUSION ON <collection> QUERY ARRAY[…] [options…]`
    GraphRagFusion {
        collection: String,
        params: crate::ddl_ast::graph_parse::FusionParams,
    },
    /// `SHOW GRAPH STATS ['<collection>'] [VERBOSE] [AS OF SYSTEM TIME <ms>]`
    ///
    /// Read-only persistence-rooted stats readout. Bypasses the in-memory
    /// CSR cache to report counts derived from the durable edge store.
    /// `collection = None` means tenant-wide aggregate over all graph
    /// collections. `verbose` toggles compact (one row + JSON labels
    /// column) vs per-label (one row per (collection, label)) output.
    /// `as_of` is system-time in ms; `None` selects the live snapshot.
    ShowGraphStats {
        collection: Option<String>,
        verbose: bool,
        as_of: Option<i64>,
    },
}
