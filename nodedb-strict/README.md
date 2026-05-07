# `nodedb-strict`

> Binary Tuple serialization for the strict document engine

O(1) field extraction via byte-offset jumps — 3-4x cache density vs MessagePack / BSON. Backs the strict-schema document engine with multi-version reads for `ALTER ADD COLUMN`.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE-APACHE-2.0)).
