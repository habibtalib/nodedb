# `nodedb-mem`

> Per-engine memory governor backed by jemalloc arenas

Pins each Data Plane core to a dedicated jemalloc arena, enforces per-engine memory budgets via a `MemoryGovernor::reserve()` API, and exposes pressure metrics. Eliminates allocator lock contention across TPC cores.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Business Source License 1.1 ([`LICENSE`](../LICENSE)).
