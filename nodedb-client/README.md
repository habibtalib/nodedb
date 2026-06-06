# `nodedb-client`

> NodeDB client SDK over MessagePack and pgwire

`NodeDb` trait + `NodeDbRemote` client. Speaks the native MessagePack protocol for low-latency native access and falls back to pgwire for compatibility. The recommended way to talk to a NodeDB server from Rust.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE.APACHE-2.0)).
