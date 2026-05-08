// SPDX-License-Identifier: Apache-2.0

//! Standardized error types for the NodeDB public API.
//!
//! [`NodeDbError`] is a **struct** (not an enum) that separates:
//! - `code` — stable numeric code for programmatic handling (`NDB-1000`)
//! - `message` — human-readable explanation
//! - `details` — machine-matchable [`ErrorDetails`] enum with structured data
//! - `cause` — optional chained error for debugging
//!
//! # Wire format
//!
//! Serializes to:
//! ```json
//! {
//!   "code": "NDB-1000",
//!   "message": "constraint violation on users: duplicate email",
//!   "details": { "kind": "constraint_violation", "collection": "users" }
//! }
//! ```
//!
//! # Error code ranges
//!
//! | Range       | Category      |
//! |-------------|---------------|
//! | 1000–1099   | Write path    |
//! | 1100–1199   | Read path     |
//! | 1200–1299   | Query         |
//! | 1300–1399   | Engine ops    |
//! | 1400–1499   | Quota         |
//! | 2000–2099   | Auth/Security |
//! | 3000–3099   | Sync          |
//! | 4000–4099   | Storage       |
//! | 4100–4199   | WAL           |
//! | 4200–4299   | Serialization |
//! | 5000–5099   | Config        |
//! | 6000–6099   | Cluster       |
//! | 7000–7099   | Memory        |
//! | 8000–8099   | Encryption    |
//! | 9000–9099   | Internal      |

pub mod code;
pub mod ctors;
pub mod details;
pub mod msgpack;
pub mod sqlstate;
pub mod types;

pub use code::ErrorCode;
pub use details::ErrorDetails;
pub use types::{NodeDbError, NodeDbResult};
