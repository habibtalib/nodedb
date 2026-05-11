# `nodedb`

> NodeDB server library and binary — **Linux only**

The single-binary distributed hybrid database. Hosts the Control / Data / Event planes, all eight engines, the WAL, storage tiers, and the pgwire / HTTP / native MessagePack listeners. Most users want this crate or [`nodedb-lite`] (embedded), not the building-block crates.

> **Platform support:** Linux x86-64 and ARM64 only. The Data Plane requires `io_uring` (Linux 5.1+). On macOS or Windows, run the server via the [official Docker image](https://hub.docker.com/r/farhansyah/nodedb), or use [`nodedb-lite`] for embedded use.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Business Source License 1.1 ([`LICENSE`](../LICENSE)).
