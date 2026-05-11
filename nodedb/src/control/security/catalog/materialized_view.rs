// SPDX-License-Identifier: BUSL-1.1

//! Materialized view metadata persisted in the system catalog.

use nodedb_types::Hlc;

/// A materialized view: strict → columnar CDC bridge.
#[derive(zerompk::ToMessagePack, zerompk::FromMessagePack, Debug, Clone)]
#[msgpack(map, allow_unknown_fields)]
pub struct StoredMaterializedView {
    pub tenant_id: u64,
    pub name: String,
    pub source: String,
    pub query_sql: String,
    #[msgpack(default = "default_refresh_mode")]
    pub refresh_mode: String,
    pub owner: String,
    pub created_at: u64,
    /// Monotonic descriptor version. See `StoredCollection::descriptor_version`.
    #[msgpack(default)]
    pub descriptor_version: u64,
    /// HLC assigned by the metadata applier. See `StoredCollection::modification_hlc`.
    #[msgpack(default)]
    pub modification_hlc: Hlc,
}

fn default_refresh_mode() -> String {
    "auto".into()
}
