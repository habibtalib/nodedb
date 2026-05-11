// SPDX-License-Identifier: BUSL-1.1

//! `cross_core_bfs` — multi-hop BFS that drives the tree DDL aggregates
//! (`TREE_SUM`, `TREE_CHILDREN`) and any other breadth-first walk that
//! needs a flat reachable-node set across the full cross-core /
//! cross-shard neighborhood of each frontier node.
//!
//! The shared per-hop scatter/decode/merge logic lives in
//! [`super::hop::execute_neighbor_hop`]; this dispatcher only retains
//! the merged destination set. `GRAPH TRAVERSE`, which needs the
//! `{nodes,edges}` subgraph shape the remote client decodes, lives in
//! [`super::traverse_subgraph::cross_core_traverse_subgraph`].

use std::collections::HashSet;

use sonic_rs;

use crate::bridge::envelope::Response;
use crate::control::state::SharedState;
use crate::engine::graph::traversal_options::GraphTraversalOptions;
use crate::types::{Lsn, RequestId, TenantId};

use super::hop::{NeighborHopParams, execute_neighbor_hop};

/// Cross-core BFS with explicit traversal options (fan-out limits, partial mode).
///
/// This is the cluster-aware entry point. Callers pass
/// `&GraphTraversalOptions::default()` for standard traversal.
pub async fn cross_core_bfs_with_options(
    shared: &SharedState,
    tenant_id: TenantId,
    start_nodes: Vec<String>,
    edge_label: Option<String>,
    direction: crate::engine::graph::edge_store::Direction,
    max_depth: usize,
    options: &GraphTraversalOptions,
) -> crate::Result<Response> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut all_discovered: Vec<String> = Vec::new();
    let mut frontier: Vec<String> = start_nodes.clone();

    for node in &start_nodes {
        visited.insert(node.clone());
        all_discovered.push(node.clone());
    }

    for depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }

        let hop = execute_neighbor_hop(
            shared,
            tenant_id,
            NeighborHopParams {
                frontier: &frontier,
                edge_label: edge_label.as_deref(),
                direction,
                options,
                discovered_so_far: all_discovered.len(),
                remaining_depth: max_depth.saturating_sub(depth + 1),
            },
        )
        .await?;

        // Extend global visited set and compute next frontier.
        let mut next_frontier: Vec<String> = Vec::new();
        for node in hop.merged_destinations {
            if visited.insert(node.clone()) {
                next_frontier.push(node.clone());
                all_discovered.push(node);
                if all_discovered.len() >= options.max_visited {
                    break;
                }
            }
        }

        frontier = next_frontier;

        if all_discovered.len() >= options.max_visited {
            break;
        }
    }

    let payload = sonic_rs::to_vec(&all_discovered).map_err(|e| crate::Error::Serialization {
        format: "json".into(),
        detail: e.to_string(),
    })?;

    Ok(Response {
        request_id: RequestId::new(0),
        status: crate::bridge::envelope::Status::Ok,
        attempt: 1,
        partial: false,
        payload: crate::bridge::envelope::Payload::from_vec(payload),
        watermark_lsn: Lsn::ZERO,
        error_code: None,
    })
}
