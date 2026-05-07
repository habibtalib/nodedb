# `nodedb-wal`

> Write-Ahead Log: O_DIRECT, group commit, AES-256-GCM encryption

Page-aligned WAL with CRC32C, segment rollover, group commit for NVMe IOPS, double-write buffer, and at-rest encryption. Provides the durability primitive every NodeDB engine writes through.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Business Source License 1.1 ([`LICENSE.md`](../LICENSE.md)).
