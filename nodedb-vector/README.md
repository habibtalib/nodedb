# `nodedb-vector`

> HNSW + Vamana indexes, scalar / SIMD distance, NaviX filtered traversal

Vector search core: HNSW (in-memory hierarchical) and Vamana / DiskANN (SSD-resident, billion-scale). Includes NaviX adaptive-local filtered traversal, SIEVE workload subindices, MetaEmbed multivec + PLAID, and the cost-model planner inputs.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE.APACHE-2.0)).
