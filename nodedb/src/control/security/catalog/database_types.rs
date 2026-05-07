// SPDX-License-Identifier: BUSL-1.1

//! Types stored in the `_system.databases` catalog table.

use nodedb_types::DatabaseId;

/// Lifecycle status of a database.
///
/// Exhaustive matches are required — no `_ =>` arms anywhere.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum DatabaseStatus {
    /// Normal operating state.
    Active,
    /// Database has been dropped but is within the retention window.
    /// Collections remain accessible in read-only mode for un-drop.
    Deactivated,
    /// A `CLONE DATABASE` operation is pending materialization.
    /// Writes are rejected until materialization completes or the
    /// clone is promoted/abandoned.
    Cloning,
    /// A `MIRROR DATABASE` observer: read-only replica of a source.
    /// Promoted to `Active` via `ALTER DATABASE PROMOTE`.
    Mirroring,
}

/// Persisted descriptor for a single database row in `_system.databases`.
#[derive(
    Debug,
    Clone,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
    serde::Serialize,
    serde::Deserialize,
)]
#[msgpack(map)]
pub struct DatabaseDescriptor {
    pub id: DatabaseId,
    /// Human-readable name. Mutable via `ALTER DATABASE RENAME`.
    /// The durable identity is always `id`, never `name`.
    pub name: String,
    pub status: DatabaseStatus,
    /// WAL LSN at which this database was created (or 0 for the
    /// bootstrapped `default` database).
    pub created_at_lsn: u64,
    /// Quota reference id — links to the quota hierarchy (tier 20).
    /// `0` means "no explicit quota; inherits global default".
    #[msgpack(default)]
    pub quota_ref: u64,
    /// For cloned databases: the source `DatabaseId` this was
    /// forked from, and the LSN / system-time boundary.
    /// `None` for non-clones.
    #[msgpack(default)]
    pub parent_clone: Option<ParentCloneRef>,
}

/// Reference to a clone's origin (populated by tier 30 — stored here
/// from tier 10 so the schema is forward-compatible).
#[derive(
    Debug,
    Clone,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
    serde::Serialize,
    serde::Deserialize,
)]
#[msgpack(map)]
pub struct ParentCloneRef {
    pub source_db_id: DatabaseId,
    /// WAL LSN at which the clone was taken (the `AS OF` point).
    pub as_of_lsn: u64,
    /// System-time milliseconds at the `AS OF` point.
    pub as_of_ms: u64,
}

impl DatabaseDescriptor {
    /// Construct a minimal descriptor for the built-in `default` database.
    pub fn default_db() -> Self {
        Self {
            id: DatabaseId::DEFAULT,
            name: "default".to_string(),
            status: DatabaseStatus::Active,
            created_at_lsn: 0,
            quota_ref: 0,
            parent_clone: None,
        }
    }
}
