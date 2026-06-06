# `nodedb-physical`

> Shared PhysicalTask IR and `SqlPlan` → `PhysicalPlan` converter

The physical-plan intermediate representation shared by NodeDB Origin and
Lite. Holds the `PhysicalPlan` / `PhysicalTask` types and the converter that
lowers a planned `SqlPlan` into engine-dispatchable physical operations, so
the server and the embedded engine execute from one definition.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE.APACHE-2.0)).
