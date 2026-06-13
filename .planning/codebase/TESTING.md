# Testing Patterns

**Analysis Date:** 2026-06-13

## Test Framework

**Runner:**
- `cargo-nextest` (primary) — required for all CI runs and recommended locally.
  Config: `.config/nextest.toml`
- `cargo test` — works for quick local runs but NOT recommended for cluster tests (port/fd exhaustion from in-process parallelism).

**Assertion Library:**
- Rust's built-in `assert!`, `assert_eq!`, `assert!(condition, "message with {context}")`.
- `proptest` for property-based / fuzz tests (e.g. `nodedb/tests/rls_fuzz.rs`).

**Benchmarks:**
- `fluxbench` crate — custom benchmark framework (not criterion). Used in `nodedb-bridge/benches/`, `nodedb-wal/benches/`.

**Run Commands:**

```bash
# All tests except cluster tests (fast suite):
cargo nextest run --workspace --exclude nodedb-cluster-tests --all-features

# Cluster tests only (strictly serialized, ~3-node Raft bringup):
cargo nextest run -p nodedb-cluster-tests --all-features

# Full suite matching CI (uses ci cargo profile + ci nextest profile):
cargo nextest run --workspace --all-features --cargo-profile ci --profile ci

# Single test by name:
cargo nextest run -p nodedb -- pgwire_show_server_version_tracks_workspace_version

# With coverage (no native support in nextest; use cargo test + llvm-cov):
cargo test --workspace --all-features

# Benchmarks (nodedb-bridge example):
cargo bench -p nodedb-bridge --bench throughput
```

## nextest Configuration

File: `.config/nextest.toml`

**Default profile:**
- `slow-timeout = { period = "30s", terminate-after = 4 }` — a test hitting 120s total is a bug.
- `test-threads = "num-cpus"` — full parallelism for unit tests.

**Cluster test group:**
- `filter = 'package(nodedb-cluster-tests)'`
- `test-group = 'cluster'` with `[test-groups] cluster = { max-threads = 1 }` — at most ONE cluster test runs at a time.
- `threads-required = 'num-test-threads'` — the running cluster test claims every available slot, starving nothing else alongside it.
- `retries = { backoff = "fixed", count = 2, delay = "1s" }` — catches startup jitter; a real regression fails twice in a row.
- `slow-timeout = { period = "30s", terminate-after = 6 }` — 180s ceiling for cluster tests (3-node Raft bringup + DDL fan-out).

**CI profile:**
- Inherits default profile.
- `retries = { backoff = "fixed", count = 3, delay = "2s" }` — four total attempts.
- `fail-fast = false` — all failures surfaced in one run.
- JUnit XML output: `target/nextest/ci/junit.xml`.

## Test File Organization

**Unit tests:** Co-located in source files as `#[cfg(test)] mod tests { ... }` at the bottom of the file. Use `use super::*;` to import the parent module:

```rust
// nodedb-fts/src/posting.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_query_mode_is_and() {
        assert_eq!(QueryMode::default(), QueryMode::And);
    }
}
```

**Crate-level integration tests:** `{crate}/tests/{test_name}.rs`. Each file is an independent integration test binary with its own module namespace. Test files include a module doc comment explaining their scope.

**Test subdirectory modules:** Some complex test files are split into subdirectory modules using explicit `#[path]` attributes:

```rust
// nodedb-crdt/tests/constraint_resolution.rs
#[path = "constraint_resolution/common.rs"]
mod common;
#[path = "constraint_resolution/cascade_defer.rs"]
mod cascade_defer;
```

**Naming:** Test files are named `{subject}_{what_is_tested}.rs` — e.g. `wire_server_version.rs`, `pgwire_show_dispatch.rs`, `sql_join_correctness.rs`, `rls_fuzz.rs`, `hnsw_layer_cap.rs`.

## Test Directory Structure

```
{crate}/
├── src/
│   └── module.rs          # inline #[cfg(test)] mod tests at bottom
└── tests/
    ├── common/
    │   └── mod.rs          # shared test helpers (re-exports nodedb-test-support)
    ├── {test_name}.rs      # one test file per feature/concern
    └── {test_dir}/         # for multi-file test modules
        ├── common.rs
        └── {scenario}.rs

nodedb-test-support/        # shared harness crate (no version, dev-only dep)
└── src/
    ├── pgwire_harness/     # TestServer + TestClient
    ├── cluster_harness/    # TestCluster + TestClusterNode
    ├── pgwire_auth_helpers.rs
    ├── tx_batch_helpers.rs
    ├── array_sync.rs
    └── lib.rs
```

## Shared Test Harness (`nodedb-test-support`)

The crate `nodedb-test-support` (no version, `path` dep only) is the single source of truth for test infrastructure shared across `nodedb`, `nodedb-cluster-tests`, and `nodedb-client-tests`.

Tests access it via a `common/mod.rs` shim:

```rust
// nodedb/tests/common/mod.rs
pub use nodedb_test_support::{
    array_sync, cluster_harness, make_cdc_event, now_ms,
    pgwire_auth_helpers, pgwire_harness, tx_batch_helpers,
};
```

## pgwire TestServer (Single-Node)

`TestServer` in `nodedb-test-support/src/pgwire_harness/` spins up a full NodeDB server in-process for each test:

```rust
// Spin up with trust-mode auth (default)
let srv = TestServer::start().await;

// Spin up with password (SCRAM-SHA-256) auth + lockout policy
let srv = TestServer::start_password().await;

// Connect to named database (UUID-suffixed for test isolation)
let (srv, db_name) = TestServer::with_database("my_db").await;
```

**Key TestServer methods:**

```rust
// Execute SQL, return first column of each row as String
srv.query_text("SHOW server_version").await   // -> Result<Vec<String>, String>

// Execute SQL, return rows as tab-joined strings
srv.query_text_joined("SELECT *").await       // -> Result<Vec<String>, String>

// Execute SQL, return rows as HashMap<column_name, value>
srv.query_named_rows("SHOW DATABASES").await  // -> Result<Vec<HashMap<String,String>>, String>

// Execute SQL, return rows as Vec<Vec<String>>
srv.query_rows("SELECT *").await              // -> Result<Vec<Vec<String>>, String>

// Execute SQL expecting success, discard result
srv.exec("CREATE DATABASE foo").await         // -> Result<(), String>

// Execute SQL expecting error containing substring (panics if succeeds)
srv.expect_error("BAD SQL", "syntax error").await

// Open second pgwire connection as different user
let (client2, handle) = srv.connect_as("alice", "password").await?;
```

Error messages from `tokio-postgres` are expanded via `pg_error_detail()` to include SQLSTATE code, severity, and actual DB error message — not just `"db error"`.

**Server lifecycle:** `TestServer` implements `Drop` to initiate shutdown and abort all background tasks. `TestServer::take_dir()` extracts the temp directory for WAL-restart tests.

## Cluster TestCluster (3-Node)

`TestCluster` in `nodedb-test-support/src/cluster_harness/` spawns N full NodeDB nodes with QUIC transport, Raft, and pgwire. Used exclusively in `nodedb-cluster-tests/tests/`.

```rust
// From nodedb-cluster-tests
mod common;
use common::cluster_harness::{TestCluster, TestClusterNode};
use common::cluster_harness::{wait_for, wait_for_async};
```

**Async waiting helpers:**

```rust
// Poll a sync predicate until true or deadline:
wait_for("raft leader elected", Duration::from_secs(10), Duration::from_millis(50), || {
    cluster.leader().is_some()
}).await;

// Poll an async predicate:
wait_for_async("DDL replicated", Duration::from_secs(30), Duration::from_millis(100), || async {
    node2.query_text("SHOW COLLECTIONS").await.unwrap().len() > 0
}).await;
```

Both functions panic with a descriptive timeout message if the deadline is exceeded.

## Wire-Protocol Integration Tests (`nodedb/tests/`)

These tests exercise the full pgwire path end-to-end against a running `TestServer`. All tests in `nodedb/tests/` are `#[tokio::test]` async functions.

**Structure of a wire test:**

```rust
// SPDX-License-Identifier: BUSL-1.1
//! One-paragraph description of what this file tests.

mod common;
use common::pgwire_harness::TestServer;

#[tokio::test]
async fn descriptive_snake_case_test_name() {
    let srv = TestServer::start().await;
    srv.exec("CREATE COLLECTION foo (id TEXT PRIMARY KEY) WITH (engine='document_strict')")
        .await
        .expect("setup must succeed");

    let rows = srv.query_text("SELECT id FROM foo").await.expect("query must succeed");
    assert_eq!(rows, vec!["expected_value"]);
}
```

**Key wire test files in `nodedb/tests/`:**
- `wire_server_version.rs` — version literal guard + `SHOW server_version` round-trip
- `pgwire_show_dispatch.rs` — all `SHOW` commands reach correct handlers (not session-parameter fallback)
- `pgwire_orm_conformance.rs` — ORM driver compatibility
- `pgwire_auth_tenants.rs/` — multi-tenant auth flows
- `sql_join_correctness.rs` — JOIN planner correctness
- `sql_backup_restore_wire.rs` — backup/restore over wire
- `http_auth.rs`, `http_cdc.rs`, `http_ws.rs` — HTTP/WebSocket endpoints
- `rls_fuzz.rs` — property-based RLS predicate evaluation

## Static / Structural Tests

Some tests are synchronous `#[test]` (no async) and walk the source tree to enforce invariants:

```rust
// nodedb/tests/wire_server_version.rs
#[test]
fn no_hardcoded_version_literal_in_server_wire_surfaces() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src").join("control").join("server");
    // Walks .rs files and asserts no "NodeDB 0.3" literals exist
}
```

`env!("CARGO_MANIFEST_DIR")` is the idiomatic way to anchor paths to the crate root.

## Property-Based Testing

`proptest` is used for fuzz-style tests. Strategies are defined with `prop_map`:

```rust
// nodedb/tests/rls_fuzz.rs
fn arb_auth_context() -> impl Strategy<Value = AuthContext> {
    (
        "[a-z]{3,8}",                                   // id
        proptest::collection::vec("[a-z]{3,8}".prop_map(String::from), 0..5),
    ).prop_map(|(id, ...)| { ... })
}

proptest! {
    #[test]
    fn deny_predicate_returns_no_rows(ctx in arb_auth_context(), pred in arb_predicate()) {
        // ...
    }
}
```

Files using proptest: `nodedb/tests/rls_fuzz.rs`, `nodedb/tests/document_bitemporal_store.rs`, `nodedb/tests/pgwire_show_dispatch.rs`, `nodedb/tests/sql_order_by.rs`, `nodedb/tests/pgwire_tenant_scoping.rs`, and others.

## Mocking

No mock framework is used. The codebase avoids mocks by:

1. Using the real in-process server (`TestServer`/`TestCluster`) for integration tests.
2. Passing `tempfile::TempDir` for storage paths — real I/O on ephemeral storage.
3. Using real `Arc<WalManager>` opened with `WalManager::open_for_testing(path)`.
4. Using trait objects (`Arc<dyn NodeDb>`) for the client trait — tests swap real local vs. remote implementations.

For unit tests that need isolated behavior, production structs are constructed directly (no fake/stub layers).

## Test Data and Fixtures

No fixture files. Test data is constructed inline:

```rust
// Inline collection setup helper in test file
async fn setup_join_tables(server: &TestServer) {
    server.exec("CREATE COLLECTION j_t1 (id TEXT PRIMARY KEY, name TEXT, x INT) ...").await.unwrap();
    server.exec("INSERT INTO j_t1 (id, name, x) VALUES ('a', 'Alice', 10)").await.unwrap();
}
```

The `make_cdc_event` helper in `nodedb-test-support` constructs `CdcEvent` with sensible defaults:

```rust
pub fn make_cdc_event(seq: u64, partition: u32, collection: &str, op: &str) -> CdcEvent {
    CdcEvent { sequence: seq, partition, collection: collection.into(), ... }
}
```

## Test Isolation

- **Temp directories:** `tempfile::TempDir` (auto-cleaned on drop) for all on-disk state.
- **Ephemeral ports:** `127.0.0.1:0` binds for pgwire, HTTP, and native listeners — OS assigns free ports. Port number retrieved via `listener.local_addr().port()`.
- **Named databases:** `TestServer::with_database(name)` appends a UUID hex suffix to avoid cross-test collision.
- **Process isolation:** nextest runs each test binary in its own process, eliminating in-process state leakage.

## Coverage

No coverage tooling is enforced in CI. Coverage can be measured locally with `cargo llvm-cov` (not configured in the repo). No minimum coverage threshold is enforced.

## Crate-Specific Test Crates

`nodedb-cluster-tests` and `nodedb-client-tests` are dedicated test-only crates in the workspace. They contain no library code, only `tests/` directories. This avoids adding test infrastructure dependencies to the main crates and enables the nextest `cluster` test group to run them separately.

---

*Testing analysis: 2026-06-13*
