# Coding Conventions

**Analysis Date:** 2026-06-13

## License Headers

Every `.rs` file begins with an SPDX header on line 1 — no blank line before it:

```rust
// SPDX-License-Identifier: BUSL-1.1
```

or

```rust
// SPDX-License-Identifier: Apache-2.0
```

**Rule:** All 3,538 source files carry one of these two identifiers. BUSL-1.1 is used for `nodedb/` (the server core, test support, cluster tests). Apache-2.0 is used for public-facing library crates (`nodedb-types`, `nodedb-client`, `nodedb-fts`, `nodedb-vector`, etc.). The license in `Cargo.toml` at workspace level is `BUSL-1.1`; individual crate `Cargo.toml` files inherit or override it.

## Module-Level Doc Comments

Every source file includes a `//!` module doc comment immediately after the SPDX line (with one blank line between). The comment describes what the module does, its scope, and any non-obvious design decisions:

```rust
// SPDX-License-Identifier: BUSL-1.1

//! WAL writer with O_DIRECT and group commit.
//!
//! The writer accumulates records into an aligned buffer and flushes to disk
//! when the buffer is full or when an explicit sync is requested.
```

Multi-section doc blocks use `##` headings for sub-topics (e.g. `## I/O path`, `## Future: io_uring`). Public items also carry `///` doc comments; internal helpers may omit them when the function name is self-explanatory.

## Item-Level Doc Comments

Public structs, enums, traits, and functions carry `///` comments. Enum variants carry inline comments when non-obvious:

```rust
/// Typed column definition for strict document and columnar collections.
///
/// `#[non_exhaustive]` — this enum grows with each type system expansion
/// (e.g. future variants may add `Decimal { precision, scale }` or split
/// `Timestamp`/`TimestampTz`). External exhaustive `match` arms must handle
/// future variants via a typed error arm rather than `_ => unreachable!()`.
#[non_exhaustive]
pub enum ColumnType { ... }
```

## Naming Conventions

**Files:** `snake_case.rs`. Multi-file modules use a directory with a `mod.rs`.

**Directories:** `snake_case` always (e.g. `nodedb-types/src/backup_envelope/`, `nodedb/src/control/server/pgwire/handler/`).

**Crates:** kebab-case (e.g. `nodedb-types`, `nodedb-cluster-tests`, `nodedb-test-support`).

**Types (structs, enums, traits):** `PascalCase` — `TestServer`, `ColumnType`, `NodeDbError`, `VectorError`.

**Functions and methods:** `snake_case` — `query_text`, `fixed_size`, `to_pg_oid`, `wait_for_async`.

**Constants:** `SCREAMING_SNAKE_CASE` — `DEFAULT_WRITE_BUFFER_SIZE`, `WIRE_FORMAT_VERSION`, `DEFAULT_FLAT_INDEX_THRESHOLD`.

**Enum variants:** `PascalCase` — `Int64`, `Timestamptz`, `BudgetExhausted`, `DimensionMismatch`.

**Type aliases:** `PascalCase` ending in the category — `NodeDbResult<T>`, `BridgeError`, `ArrayEngineResult<T>`.

**Modules:** `snake_case`. Modules that pattern-match on exhaustive enums must annotate `#![deny(clippy::wildcard_enum_match_arm)]` at the module level.

## `#[non_exhaustive]` Pattern

`#[non_exhaustive]` is applied to both public enums that grow over time and error enums that may gain variants:

- `ColumnType` — `nodedb-types/src/columnar/column_type.rs` — type-system enum, grows with new types
- `Value` — `nodedb-types/src/value/core.rs` — dynamic value enum, grows with new variants
- `VectorError` — `nodedb-vector/src/error.rs` — error enum marked `#[non_exhaustive]`
- `QueryMode` (in `nodedb-fts`) — small behavioral enum, marked `#[non_exhaustive]`

The doc comment on the type always explains WHY the attribute is there and what callers must do (use a typed error arm, not `_ => unreachable!()`).

## Error Handling

**Pattern 1 — `thiserror` enum per crate:**
Each crate defines its own error enum using `#[derive(thiserror::Error)]` in an `error.rs` file. Each variant carries a structured `#[error("...")]` message with named fields:

```rust
// nodedb/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("constraint violation on {collection}: {detail}")]
    RejectedConstraint { collection: String, constraint: String, detail: String },

    #[error("write conflict on {collection}/{document_id}, retry with idempotency key")]
    ConflictRetry { collection: String, document_id: String },
}
pub type Result<T> = std::result::Result<T, Error>;
```

Files: `nodedb/src/error.rs`, `nodedb-bridge/src/error.rs`, `nodedb-vector/src/error.rs`, `nodedb-crdt/src/error.rs`, `nodedb-strict/src/error.rs`, `nodedb-raft/src/error.rs`, `nodedb-columnar/src/error.rs`.

**Pattern 2 — `NodeDbError` struct for public API boundary:**
The public-facing error type in `nodedb-types/src/error/` is a struct (not an enum) combining a numeric `ErrorCode`, a human `message`, machine-matchable `ErrorDetails`, and an optional chained `cause`. Internal `Error` enums convert to `NodeDbError` at the API boundary via `From`.

```rust
// nodedb-types/src/error/types.rs
pub struct NodeDbError {
    pub(super) code: ErrorCode,
    pub(super) message: String,
    pub(super) details: ErrorDetails,
    pub(super) cause: Option<Box<NodeDbError>>,
}
pub type NodeDbResult<T> = std::result::Result<T, NodeDbError>;
```

**Pattern 3 — `?` propagation, never `.expect()` in format paths:**
The CI gate `scripts/ci/check_format_expects.sh` forbids `.expect()` in segment format parser production code (`nodedb-columnar/src/format.rs`, `nodedb-fts/src/lsm/segment`, `nodedb-array/src/segment/format`). These paths must use `?` so the quarantine registry can handle corrupted segments. `.expect()` is permissible inside `#[cfg(test)]` blocks.

**Pattern 4 — `#[from]` for chained errors:**
```rust
#[error("memory budget exhausted: {0}")]
BudgetExhausted(#[from] MemError),
```

## Section Separators

Long source files use `// ── Section Name ──` comment separators (U+2500 box-drawing chars) to visually partition code into logical sections:

```rust
// ── Write helpers ──
// ── SEGV framing constants ─────────────────────────────────────────────────
// ── Assertion helpers ─────────────────────────────────────────────────────
```

Simpler dashes are also used in test files:
```rust
// ── SHOW DATABASES ───────────────────────────────────────────────────
// ────────────────────────────────────────────────────────────────────
```

## Import Organization

Imports are grouped in this order (rustfmt enforces within-group sorting):
1. `std::` — standard library
2. Third-party crates (alphabetical)
3. Internal workspace crates (`nodedb_types`, `nodedb_mem`, etc.)
4. `crate::` — current crate imports

No blank lines between same-group imports; one blank line between groups. Example from `nodedb-vector/src/flat.rs`:

```rust
use roaring::RoaringBitmap;            // third-party

use crate::distance::{DistanceMetric, distance};  // crate::
use crate::hnsw::SearchResult;
```

Example from `nodedb-client/src/graph_dsl.rs`:

```rust
use std::collections::HashMap;         // std

use nodedb_types::error::{NodeDbError, NodeDbResult};  // workspace
use nodedb_types::value::Value;

use crate::sql_escape::quote_string_literal;  // crate
```

## Formatting

`cargo fmt --all` is enforced in CI (`test.yml` → `cargo fmt --all -- --check`). No `rustfmt.toml` exists; default rustfmt settings are used with Rust edition 2024.

## Linting

`cargo clippy --workspace --all-targets --all-features --profile ci -- -D warnings` is enforced in CI. No `clippy.toml` exists; clippy runs with default settings plus `-D warnings` (all warnings become errors).

Selected targeted lint annotations:

- `#![deny(clippy::wildcard_enum_match_arm)]` — applied to every module that exhaustively matches `PermissionTarget`, `Role`, or `PhysicalPlan`. Documented in `nodedb/src/control/security/identity/mod.rs`.
- `#![allow(dead_code)]` — used only in shared test helper modules where not every test file exercises every helper (e.g. `nodedb-test-support/src/pgwire_auth_helpers.rs`, cluster `common/mod.rs`).
- `#![allow(clippy::too_many_arguments)]` — used sparingly in generated/visitor code (`nodedb-sql/src/visitor/plan_visitor/trait_def.rs`).
- `#![allow(unsafe_op_in_unsafe_fn)]` — used in low-level SIMD code (`nodedb-codec/src/vector_quant/ternary/simd.rs`, `nodedb-codec/src/vector_quant/hamming.rs`).

## Derive Macro Order

Derive attributes appear in this conventional order: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, zerompk::ToMessagePack, zerompk::FromMessagePack`. Custom derive macros from workspace crates follow serde:

```rust
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash,
    Serialize, Deserialize,
    zerompk::ToMessagePack, zerompk::FromMessagePack,
)]
```

## Serde Configuration

`#[serde(tag = "type", content = "params")]` — internally-tagged enums, used for `ColumnType`.
`#[serde(untagged)]` — untagged for `Value` (documented as intentionally lossy for JSON at API boundaries; lossless only via MessagePack).
`#[serde(skip_serializing_if = "Option::is_none")]` — for optional fields in wire types (e.g. `NodeDbError.cause`).

## Async / Trait Pattern

For traits that require `async fn` across both native (Tokio) and WASM targets, `async_trait` is used with conditional `?Send`:

```rust
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait NodeDb: NodeDbMarker { ... }
```

Files: `nodedb-client/src/traits/core/trait_def.rs`.

## CI Gate Scripts

Four custom bash scripts in `scripts/ci/` enforce structural invariants beyond clippy:

| Script | Enforces |
|--------|----------|
| `check_format_expects.sh` | No `.expect()` in segment format parser production code |
| `check_plane_separation.sh` | Data Plane has no `tokio::`, Control Plane has no `io_uring`, Bridge has no `Arc<Mutex<>>` |
| `check_calvin_determinism.sh` | Calvin write-path code has no `HashMap::iter`, `SystemTime::now`, `Uuid::new_v4`, etc. |
| `check_warm_storage_gate.sh` | Object store / cold storage access gated correctly |

Opt-out markers for these gates:
- `// no-plane-separation: <reason>` on the same line or the preceding line
- `// no-determinism: <reason>` on the same line or the preceding line

## Tracing / Logging

`tracing` crate macros are used throughout. Fields are passed as named key-value pairs using the `field = value` or `field = %value` syntax:

```rust
tracing::warn!(
    path = %dwb_path.display(),
    error = %e,
    mode = ?mode,
    "failed to open DWB — torn-write protection disabled for this writer"
);
```

`%` is used for `Display`, `?` is used for `Debug`. The message string is always the last argument. No `println!` or `eprintln!` in library code; only in `main.rs` for the startup banner.

## Function and Module Design

- Each module has one clear responsibility. Long files use section separators.
- Public modules re-export their most-used items at the top of `mod.rs` or `lib.rs` via `pub use`:

```rust
// nodedb-types/src/lib.rs
pub use columnar::{ColumnDef, ColumnType, ColumnarProfile, ...};
pub use error::NodeDbError;
pub use value::Value;
```

- Internal implementation details are in private submodules. Cross-module access uses explicit paths, not glob re-imports from within the same crate.

## Workspace Package Defaults

All crate `Cargo.toml` files inherit from `[workspace.package]`:
- `version = "0.3.0"`
- `edition = "2024"`
- `rust-version = "1.94"`
- `license = "BUSL-1.1"` (may be overridden per crate)

---

*Convention analysis: 2026-06-13*
