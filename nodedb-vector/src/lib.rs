// SPDX-License-Identifier: BUSL-1.1

//! Vector search primitives shared by Origin, Lite, and WASM: HNSW + Vamana
//! indexes, scalar / SIMD distance kernels, the quantization codec frontier
//! (SQ8, PQ, IVF-PQ, OPQ, RaBitQ, BBQ, Ternary BitNet 1.58, Binary), the
//! VectorCollection runtime with mmap NVMe segments and background builder,
//! and the cost-model planner inputs (target_recall, oversample, ef_search,
//! query_dim, meta_token_budget, quantization).
//!
//! This crate has no platform-required cargo features for v0.1.0 — SIMD
//! kernels are gated by `#[cfg(target_arch)]` and dispatch happens at
//! runtime. The optional `acorn-baseline` feature retains the old ACORN-1
//! filtered-traversal heuristic for benchmarking against NaviX; not for
//! production use.

pub mod batch_distance;
pub mod codec_index;
pub mod delta;
pub mod distance;
pub mod error;
pub mod hnsw;
pub mod hybrid;
pub mod matryoshka;
pub mod multivec;
pub mod quantize;
pub mod vamana;

pub use distance::DistanceMetric;
pub use error::VectorError;
pub use hnsw::{HnswIndex, HnswParams, SearchResult};
pub use nodedb_types::Surrogate;
pub use quantize::Sq8Codec;

// NaviX adaptive-local filtered traversal (VLDB 2025).
pub mod navix;

// SIEVE workload-driven subindex collection for stable predicates (SIEVE 2025).
pub mod sieve;

// Cost-based multidimensional vector query planner.
pub mod planner;

// Origin-only modules (always compiled for native targets).
pub mod adaptive_filter;
pub mod flat;
pub mod index_config;

// IVF-PQ index (large datasets).
pub mod ivf;

// NVMe mmap tier (requires libc).
pub mod mmap_segment;

// Background HNSW builder thread.
pub mod builder;

// Full VectorCollection with segment lifecycle.
pub mod collection;

// Re-exports for unconditionally compiled types.
pub use adaptive_filter::{
    FilterStrategy, FilterThresholds, adaptive_search, estimate_selectivity, select_strategy,
};
pub use builder::{BuildSender, CompleteReceiver};
pub use collection::{BuildComplete, BuildRequest, StorageTier, VectorCollection};
pub use flat::FlatIndex;
pub use index_config::{IndexConfig, IndexType};
pub use ivf::{IvfPqIndex, IvfPqParams};
