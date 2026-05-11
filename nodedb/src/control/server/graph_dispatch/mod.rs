// SPDX-License-Identifier: BUSL-1.1

//! Cross-core BFS and shortest-path orchestration for graph traversal.
//!
//! In single-node mode, BFS is local: the Control Plane broadcasts
//! `GraphNeighbors` to all Data Plane cores hop by hop and collects results.
//!
//! In cluster mode, after each local hop the Control Plane inspects the
//! discovered frontier and identifies nodes that hash to shards owned by
//! remote nodes. Those are batched into a `ScatterEnvelope` and dispatched
//! to the remote shard leaders via
//! `control::scatter_gather::coordinate_cross_shard_hop`. Remote results
//! are merged before the next depth level begins.

pub mod bfs;
pub mod helpers;
pub(crate) mod hop;
pub mod shortest_path;
pub mod traverse_subgraph;

pub use bfs::cross_core_bfs_with_options;
pub use shortest_path::cross_core_shortest_path;
pub use traverse_subgraph::cross_core_traverse_subgraph;
