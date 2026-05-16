// SPDX-License-Identifier: Apache-2.0

//! HNSW graph structure — nodes, parameters, core index operations.
//!
//! Production implementation per Malkov & Yashunin (2018).
//! FP32 construction for structural integrity; heuristic neighbor selection.

pub mod index;
pub mod types;

/// Initial arena capacity used when constructing a new [`index::HnswIndex`].
///
/// Sized to cover `ef_construction = 200` (the default) without needing a
/// reallocation on the first insert or search.
pub(crate) const ARENA_INITIAL_CAPACITY: usize = 256;

/// Hard cap on the layer assigned to any node during insertion.
/// Standard HNSW practice — prevents pathological RNG draws from inflating
/// `max_layer` and slowing every subsequent search.
pub const MAX_LAYER_CAP: usize = 16;

pub use index::HnswIndex;
pub use nodedb_types::hnsw::HnswParams;
pub use types::{Candidate, Node, NodeStorage, SearchResult, Xorshift64};
