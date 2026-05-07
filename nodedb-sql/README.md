# `nodedb-sql`

> SQL parser, planner, and engine-routing rules

sqlparser-rs frontend plus `EngineRules` — the single source of truth for what each engine supports and how SQL maps to engine-specific `SqlPlan` variants. Originating crate for `EngineType` and DDL ASTs.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE-APACHE-2.0)).
