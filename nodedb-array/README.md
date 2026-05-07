# `nodedb-array`

> ND-sparse coordinate-tuple array engine (TileDB / Zarr / SciDB replacement)

Tile-based sparse multi-dimensional engine. Coordinate-tuple keying, per-tile compression via `nodedb-codec`, Z-order / row-major cell ordering, per-tile MBR statistics, bitemporal cell history, and `audit_retain_ms` retention.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE-APACHE-2.0)).
