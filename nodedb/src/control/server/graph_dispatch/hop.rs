// SPDX-License-Identifier: BUSL-1.1

//! One BFS hop: build `NeighborsMulti`, broadcast to all Data Plane
//! cores, decode the `{src,label,node}` JSON array, and (in cluster
//! mode) scatter cross-shard destinations and merge.
//!
//! Both `bfs::cross_core_bfs_with_options` and
//! `traverse_subgraph::cross_core_traverse_subgraph` execute the same
//! hop. They differ only in what they retain from each hop:
//!
//! * BFS keeps the merged destination set (flat reachable nodes).
//! * Subgraph traversal keeps the fully-attributed local edge triples
//!   *plus* the merged destination set (for next-frontier expansion).
//!
//! Cross-shard edge attribution is intentionally out of scope here:
//! `scatter_gather::coordinate_cross_shard_hop` returns only a merged
//! destination list (see `CrossShardHopParams`/return type), with no
//! channel to surface remote `(src,label,dst)` triples. Surfacing them
//! requires extending the scatter response shape, which is tracked as a
//! separate workstream. Today every single-node deployment, and the
//! local-shard portion of every cluster deployment, is fully attributed.

use sonic_rs;

use crate::bridge::envelope::PhysicalPlan;
use crate::bridge::physical_plan::GraphOp;
use crate::control::scatter_gather;
use crate::control::state::SharedState;
use crate::engine::graph::edge_store::Direction;
use crate::engine::graph::traversal_options::GraphTraversalOptions;
use crate::types::{TenantId, TraceId};

/// A fully-attributed edge crossed by the local hop: `(src, label, dst)`.
pub(super) type NeighborTriple = (String, String, String);

/// Result of one BFS hop.
pub(super) struct HopOutput {
    /// Local `(src,label,dst)` edges crossed this hop. Always
    /// fully-attributed (no cross-shard remotes).
    pub local_triples: Vec<NeighborTriple>,
    /// Merged destination node IDs after cluster scatter-gather. In
    /// single-node mode this is exactly `local_triples.iter().map(|(_,_,d)| d)`.
    pub merged_destinations: Vec<String>,
}

/// Parameters for one BFS hop. Grouped to keep the call site readable
/// and to mirror the shape of [`scatter_gather::CrossShardHopParams`].
pub(super) struct NeighborHopParams<'a> {
    pub frontier: &'a [String],
    pub edge_label: Option<&'a str>,
    pub direction: Direction,
    pub options: &'a GraphTraversalOptions,
    /// Count of nodes already in the global visited set. Bounds the
    /// Data-Plane-side allocation under `options.max_visited` via
    /// `NeighborsMulti.max_results`.
    pub discovered_so_far: usize,
    /// Hops left after this one. Passed through to the cross-shard
    /// coordinator so remote shards know how much further to recurse.
    pub remaining_depth: usize,
}

/// Execute one hop of BFS from `params.frontier`.
pub(super) async fn execute_neighbor_hop(
    shared: &SharedState,
    tenant_id: TenantId,
    params: NeighborHopParams<'_>,
) -> crate::Result<HopOutput> {
    let NeighborHopParams {
        frontier,
        edge_label,
        direction,
        options,
        discovered_so_far,
        remaining_depth,
    } = params;
    let cluster_mode = shared.cluster_routing.is_some();

    // Cap this hop's handler-side allocation to the remaining budget
    // under `max_visited` so a single wide hop cannot blow past the
    // cap on the Data Plane side. `saturating_sub` plus the `u32::MAX`
    // clamp keeps the cast lossless.
    let remaining_budget = options
        .max_visited
        .saturating_sub(discovered_so_far)
        .min(u32::MAX as usize) as u32;

    let plan = PhysicalPlan::Graph(GraphOp::NeighborsMulti {
        node_ids: frontier.to_vec(),
        edge_label: edge_label.map(str::to_string),
        direction,
        max_results: remaining_budget,
        rls_filters: Vec::new(),
    });

    let resp = crate::control::server::broadcast::broadcast_to_all_cores(
        shared,
        tenant_id,
        plan,
        TraceId::ZERO,
    )
    .await?;

    let local_triples = decode_local_neighbor_triples(&resp.payload);

    let merged_destinations = if cluster_mode {
        let local_dst_only: Vec<String> = local_triples.iter().map(|(_, _, d)| d.clone()).collect();
        let (local_nodes, cross_shard_envelope) = {
            let routing = shared
                .cluster_routing
                .as_ref()
                .expect("cluster_routing checked above");
            let rt = routing.read().unwrap_or_else(|p| p.into_inner());
            scatter_gather::partition_local_remote(&local_dst_only, shared.node_id, &rt)
        };

        if cross_shard_envelope.is_empty() {
            local_nodes
        } else {
            let (merged, _meta) = scatter_gather::coordinate_cross_shard_hop(
                shared,
                tenant_id,
                scatter_gather::CrossShardHopParams {
                    local_nodes,
                    envelope: cross_shard_envelope,
                    options,
                    edge_label,
                    direction,
                    remaining_depth,
                },
            )
            .await?;
            merged
        }
    } else {
        local_triples.iter().map(|(_, _, d)| d.clone()).collect()
    };

    Ok(HopOutput {
        local_triples,
        merged_destinations,
    })
}

/// Decode the msgpack-encoded `{src,label,node}` array a broadcast
/// returns into fully-typed triples. Malformed entries (missing or
/// non-string `src`/`node`) are skipped; `label` defaults to "" since
/// label-less edges are a valid graph shape.
fn decode_local_neighbor_triples(
    payload: &crate::bridge::envelope::Payload,
) -> Vec<NeighborTriple> {
    if payload.is_empty() {
        return Vec::new();
    }
    let json_text = crate::data::executor::response_codec::decode_payload_to_json(payload);
    let arr = match sonic_rs::from_str::<Vec<serde_json::Value>>(&json_text) {
        Ok(arr) => arr,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let src = item.get("src").and_then(|v| v.as_str());
        let node = item.get("node").and_then(|v| v.as_str());
        let (src, node) = match (src, node) {
            (Some(s), Some(n)) if !s.is_empty() && !n.is_empty() => (s, n),
            _ => continue,
        };
        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or("");
        out.push((src.to_string(), label.to_string(), node.to_string()));
    }
    out
}
