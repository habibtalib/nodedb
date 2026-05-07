# `nodedb-types`

> Shared types: surrogate IDs, errors, wire shapes, value model

The foundational crate every other NodeDB crate depends on. Defines `Surrogate`, `Lsn`, `TenantId`, `VShardId`, the `NodeDbError` struct, `nodedb_types::Value`, and the cross-plane wire shapes serialized via zerompk.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE-APACHE)).
