// SPDX-License-Identifier: BUSL-1.1

//! API key management — generation, verification, storage.
//!
//! Key format: `ndb_<key_id>.<secret>`
//! - `ndb_` — literal prefix (4 chars).
//! - `<key_id>` — 8 random bytes encoded as base64url-no-pad (11 chars).
//! - `.` — separator; `.` is not in the base64url alphabet so the split is unambiguous.
//! - `<secret>` — 32 random bytes encoded as base64url-no-pad (43 chars).
//! - Total token length: 59 chars.
//!
//! Storage: SHA-256 hash of secret in system catalog (redb).
//! The full key is only shown once at creation time.
//!
//! Module layout:
//! - `record` — `KeyScope`, `CreateKeyParams`, `ApiKeyRecord` (in-memory shape +
//!   bidirectional mapping to the catalog's `StoredApiKey`).
//! - `store`  — `ApiKeyStore` (in-memory cache, catalog persistence, raft
//!   replication hooks).
//! - `token`  — token generation, hashing, parsing primitives.

pub mod record;
pub mod store;
pub mod token;

pub use record::{ApiKeyRecord, CreateKeyParams, KeyScope};
pub use store::ApiKeyStore;
