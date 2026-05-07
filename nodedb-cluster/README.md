# `nodedb-cluster`

> Distributed layer: vShards, QUIC transport, replication

Cluster orchestration on top of `nodedb-raft`: vShard placement, QUIC transport via nexar / quinn, replication and follower reads, vShard auto-rebalancing. **Public API is unstable for v0.1.0** — most types are `#[doc(hidden)]`.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Business Source License 1.1 ([`LICENSE`](../LICENSE)).
