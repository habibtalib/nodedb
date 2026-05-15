// SPDX-License-Identifier: Apache-2.0

//! Columnar insert sync messages (client → server / server → client).
//!
//! `ColumnarInsertMsg` carries a batch of typed row values from a Lite
//! client to Origin. Each row is a MessagePack-serialized `Vec<Value>` in
//! schema column order, matching the collection's `ColumnarSchema`.
//!
//! Wire layout mirrors `TimeseriesPushMsg`: typed payload + schema hint +
//! a monotonic `batch_id` for dedup / ACK correlation.

use serde::{Deserialize, Serialize};

/// Columnar batch insert (client → server, 0xA0).
///
/// Carries one or more rows for a columnar collection. Each entry in
/// `rows` is a MessagePack-serialized `Vec<nodedb_types::value::Value>`
/// with entries in schema column order.
///
/// `schema_bytes` is a MessagePack-serialized `ColumnarSchema`. Origin uses
/// it to create the collection if it does not yet exist (definition-sync
/// guarantees it will already exist in most cases, but the schema hint
/// lets Origin validate column count and types rather than guessing).
#[derive(
    Debug, Clone, Serialize, Deserialize, zerompk::ToMessagePack, zerompk::FromMessagePack,
)]
pub struct ColumnarInsertMsg {
    /// Lite instance ID (for routing and dedup).
    pub lite_id: String,
    /// Target collection name.
    pub collection: String,
    /// Batch of rows. Each element is MessagePack `Vec<Value>` (schema column order).
    pub rows: Vec<Vec<u8>>,
    /// Monotonic batch ID (Lite-assigned, per-collection). Used for ACK correlation.
    pub batch_id: u64,
    /// MessagePack-serialized `ColumnarSchema`. May be empty for collections
    /// that were already synced via definition-sync.
    #[serde(default)]
    pub schema_bytes: Vec<u8>,
}

/// Columnar insert acknowledgment (server → client, 0xA1).
#[derive(
    Debug, Clone, Serialize, Deserialize, zerompk::ToMessagePack, zerompk::FromMessagePack,
)]
pub struct ColumnarInsertAckMsg {
    /// Collection acknowledged.
    pub collection: String,
    /// Batch ID from the originating `ColumnarInsertMsg`.
    pub batch_id: u64,
    /// Number of rows successfully inserted.
    pub accepted: u64,
    /// Number of rows rejected (schema mismatch, constraint violation, etc.).
    pub rejected: u64,
    /// Optional rejection detail for the first rejected row.
    #[serde(default)]
    pub reject_reason: Option<String>,
}
