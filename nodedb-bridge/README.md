# `nodedb-bridge`

> SPSC ring-buffer bridge between the Control and Data planes

Bounded lock-free single-producer / single-consumer queues with backpressure thresholds (85% / 95%). Used by NodeDB to send query plans from Tokio (Control Plane) to TPC reactors (Data Plane) without locks or atomics on the hot path.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Business Source License 1.1 ([`LICENSE.md`](../LICENSE.md)).
