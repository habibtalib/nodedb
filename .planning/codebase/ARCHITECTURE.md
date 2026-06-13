# Architecture

**Analysis Date:** 2026-06-13

## Pattern Overview

**Overall:** Three-Plane Execution Model — Control Plane (Tokio async), Data Plane (thread-per-core, `!Send`), Event Plane (Tokio async), connected by lock-free SPSC ring buffers.

**Key Characteristics:**
- No locks on the hot path: Data Plane cores own their state exclusively (`!Send` by design)
- All cross-plane communication is through SPSC bounded ring buffers (`nodedb-bridge`)
- A `PhysicalPlan` enum is the single contract between Control Plane and Data Plane
- Six storage engines share one execution model; engine selection is per-collection, not per-query
- Surrogate IDs (`u64`) provide a unified cross-engine identity for bitmap-fused queries

## Layers

**Control Plane:**
- Purpose: SQL parsing, query planning, connection handling, security, DDL, catalog
- Location: `nodedb/src/control/`
- Contains: Protocol handlers, planner, security middleware, cluster coordination, maintenance scheduling
- Depends on: `nodedb-sql` (parser), `nodedb-physical` (plan types), `nodedb-mem` (governor), `nodedb-cluster` (Raft)
- Used by: Client connections (pgwire, HTTP, native, RESP, ILP, sync WebSocket)
- Runtime: Tokio async; all types are `Send + Sync`

**Data Plane:**
- Purpose: Physical I/O, SIMD math, WAL append, BEFORE triggers, all engine execution
- Location: `nodedb/src/data/`, `nodedb/src/engine/`
- Contains: Per-core `CoreLoop`, engine dispatchers, physical handlers per engine
- Depends on: `nodedb-wal`, `nodedb-columnar`, `nodedb-vector`, `nodedb-graph`, `nodedb-fts`, `nodedb-spatial`, `nodedb-codec`, `nodedb-strict`, `nodedb-crdt`, `nodedb-array`
- Used by: Control Plane via SPSC bridge only
- Runtime: `std::thread` (one thread per CPU core); types are `!Send`, pinned to a single core

**Event Plane:**
- Purpose: AFTER trigger dispatch, CDC change streams, cron scheduler, durable pub/sub, webhook delivery
- Location: `nodedb/src/event/`
- Contains: CDC router, trigger dispatcher, scheduler, streaming materialized views, Kafka bridge, webhook delivery
- Depends on: Control Plane (dispatches trigger SQL back through it), WAL (crash recovery)
- Used by: Data Plane emits `WriteEvent` records via per-core ring buffers to the Event Plane
- Runtime: Tokio async; `Send + Sync`

**SPSC Bridge:**
- Purpose: Lock-free communication between Control and Data Planes
- Location: `nodedb-bridge/src/` (ring buffer primitives), `nodedb/src/bridge/` (wiring)
- Contains: `RingBuffer`, `WeightedFairQueue` (Deficit Round-Robin), backpressure controller
- Key file: `nodedb-bridge/src/wfq.rs` — per-database Weighted Fair Queueing
- Backpressure: 85% utilization → reduce read depth; 95% → suspend new reads per virtual queue

**Storage Layer:**
- Purpose: Persistence primitives shared by engines
- Location: `nodedb/src/storage/`, `nodedb-wal/`, `nodedb-strict/`
- Contains: WAL (O_DIRECT via io_uring), Binary Tuple encoder/decoder (`nodedb-strict`), storage quarantine registry
- Key files: `nodedb-wal/src/group_commit.rs` (priority-aware fsync), `nodedb-wal/src/uring_writer.rs`

## Data Flow

**SQL Query Path (pgwire / HTTP / native SQL mode):**

1. Client connects; TLS negotiation at `nodedb/src/control/server/pgwire/listener.rs` or `nodedb/src/bootstrap/listeners.rs`
2. Authentication (SCRAM-SHA-256 / Argon2) in `nodedb/src/control/server/pgwire/handler/core.rs`
3. SQL text received; DDL statements routed to `nodedb/src/control/server/pgwire/ddl/` handlers
4. DML/query: `NodeDbPgHandler::execute_sql()` in `nodedb/src/control/server/pgwire/handler/sql_exec.rs`
5. Plan cache checked; on miss: `plan_statement_to_tasks()` in `nodedb/src/control/server/pgwire/handler/routing/planning.rs`
6. `nodedb-sql` parses SQL text via `sqlparser` into AST
7. `nodedb/src/control/planner/sql_plan_convert/convert.rs` converts logical AST to `Vec<PhysicalTask>` (from `nodedb-physical`)
8. RLS injection applied (`nodedb/src/control/planner/rls_injection.rs`)
9. `Dispatcher::dispatch()` in `nodedb/src/bridge/dispatch.rs` enqueues `Request { plan: PhysicalPlan, .. }` into per-core WFQ → SPSC ring
10. Core woken via eventfd (`EventFdNotifier`)
11. `CoreLoop::drain_requests()` and `CoreLoop::poll_one()` in `nodedb/src/data/executor/core_loop/tick.rs`
12. `CoreLoop::execute()` in `nodedb/src/data/executor/dispatch/mod.rs` matches `PhysicalPlan` variant, delegates to engine dispatcher
13. Engine handler executes (reads/writes engines, appends WAL); emits `WriteEvent` to Event Plane ring
14. `Response` pushed back through SPSC; Control Plane response poller collects and routes to waiting session
15. pgwire result rows encoded via `nodedb/src/control/server/pgwire/handler/projection.rs`

**Native Opcode Path (SDK / FFI / WASM):**

1. Client sends typed MessagePack opcode (e.g., `VectorSearch 0x80`) via NDB protocol port 6433
2. `nodedb/src/control/server/native/dispatch/mod.rs` decodes opcode
3. `nodedb/src/control/server/native/dispatch/plan_builder/` constructs `PhysicalPlan` directly via `build_plan()` — skips SQL parsing
4. Same path from step 9 onward (Dispatcher → Data Plane)

**Write Event / Event Plane Flow:**

1. Data Plane handler appends WAL entry and emits `WriteEvent` (insert/update/delete) to per-core EventProducer ring
2. Event Plane consumers in `nodedb/src/event/bus.rs` drain rings
3. CDC router (`nodedb/src/event/cdc/router.rs`) fans out to registered change streams
4. Trigger dispatcher (`nodedb/src/event/trigger/dispatcher/`) evaluates AFTER triggers; dispatches trigger SQL back through Control Plane
5. Scheduler (`nodedb/src/event/scheduler/`) evaluates cron expressions; dispatches scheduled SQL through Control Plane
6. Kafka bridge (`nodedb/src/event/kafka/`) forwards events to Kafka topics

**State Management:**
- Control Plane: `SharedState` (`nodedb/src/control/state/fields.rs`) — `Arc<Mutex<Dispatcher>>`, catalog, security stores, all `Send + Sync`
- Data Plane: `CoreLoop` (`nodedb/src/data/executor/core_loop/state.rs`) — per-core, `!Send`, owns all engine state for that shard

## Key Abstractions

**PhysicalPlan:**
- Purpose: The single typed contract between Control Plane and Data Plane
- Location: `nodedb-physical/src/physical_plan/mod.rs`
- Pattern: Top-level enum with one variant per engine (`Vector`, `Graph`, `Document`, `Kv`, `Text`, `Columnar`, `Timeseries`, `Spatial`, `Crdt`, `Query`, `Meta`, `Array`); each variant wraps a per-engine op enum
- Serialization: MessagePack via `zerompk` for SPSC transport; serde for debugging

**ColumnType:**
- Purpose: Atomic value type descriptor for typed (strict/columnar) schemas
- Location: `nodedb-types/src/columnar/column_type.rs`
- Pattern: Non-exhaustive enum; variants include `Int64`, `Float64`, `String`, `Bool`, `Bytes`, `Timestamp`, `Timestamptz`, `Decimal { precision, scale }`, `Geometry`, `Vector(u32)`, `Uuid`, `Json`, `Ulid`, `Duration`, `Array`, `Set`, `Regex`, `Range`, `Record`

**Value:**
- Purpose: Dynamic runtime value for document fields, SQL parameters, and query results
- Location: `nodedb-types/src/value/core.rs`
- Pattern: Non-exhaustive enum; JSON-serialized (lossy) at API boundary only; MessagePack (lossless) for all internal transport. Variants: `Null`, `Bool`, `Integer`, `Float`, `String`, `Bytes`, `Array`, `Object`, `Uuid`, `Ulid`, `DateTime`, `NaiveDateTime`, `Float32Vector`, `Geometry`, `Decimal`, `Duration`, `Regex`, `Range`, `Record`, `ArrayCell`

**Dispatcher:**
- Purpose: Routes PhysicalPlan requests from Control Plane to correct Data Plane core
- Location: `nodedb/src/bridge/dispatch.rs`
- Pattern: One `CoreChannel` per Data Plane core; each channel has a `WeightedFairQueue` (per-database DRR) + SPSC `RingBuffer`; vShard routing via `VShardRouter`

**CoreLoop:**
- Purpose: Per-core Data Plane execution loop
- Location: `nodedb/src/data/executor/core_loop/state.rs`
- Pattern: Owns all engine state for its shard (`SparseEngine` / redb, `VectorCollection` map, CRDT engines, graph edge store, etc.); 3-tier priority queue (8:4:2 drain ratio); eventfd-driven wake

**SharedState:**
- Purpose: All Control Plane shared state, accessible across all Tokio tasks
- Location: `nodedb/src/control/state/fields.rs`
- Pattern: `Arc`-wrapped; holds `Mutex<Dispatcher>`, `RequestTracker`, `WalManager`, all security stores, quota enforcement

## Entry Points

**Server Binary:**
- Location: `nodedb/src/main.rs`
- Triggers: Direct process launch
- Responsibilities: Config loading, jemalloc setup, WAL init, SPSC bridge creation, Data Plane core spawning, Event Plane spawning, cluster init (Raft), `SharedState` construction, listener binding and spawning

**pgwire Listener:**
- Location: `nodedb/src/control/server/pgwire/listener.rs`; handler: `nodedb/src/control/server/pgwire/handler/core.rs`
- Port: 6432 (default)
- Triggers: PostgreSQL-compatible TCP connections

**Native (NDB) Listener:**
- Location: `nodedb/src/control/server/listener.rs`
- Port: 6433 (default)
- Triggers: NDB binary protocol connections (SDK, CLI, FFI)

**HTTP Listener:**
- Location: `nodedb/src/control/server/http/mod.rs`; routes: `nodedb/src/control/server/http/routes/`
- Port: 6480 (default)
- Triggers: REST clients, `/v1/query`, `/metrics`, `/healthz`

**RESP Listener:**
- Location: `nodedb/src/control/server/resp/` (optional, port-activated)
- Triggers: Redis-compatible clients

**ILP Listener:**
- Location: `nodedb/src/control/server/ilp_listener.rs` (optional, port-activated)
- Triggers: InfluxDB Line Protocol ingest (timeseries)

**Sync WebSocket Listener:**
- Location: `nodedb/src/control/server/sync/listener.rs`
- Port: 9090 (default)
- Triggers: NodeDB-Lite CRDT sync clients

**Data Plane Core:**
- Location: `nodedb/src/data/executor/core_loop/tick.rs` (`drain_requests`, `poll_one`)
- Triggers: eventfd notification from `Dispatcher::dispatch()`

## Error Handling

**Strategy:** Typed error enums at every boundary; `thiserror`-derived throughout.

**Patterns:**
- `nodedb/src/error.rs`: Top-level `crate::Error` enum for Control Plane + Data Plane boundary errors
- `nodedb/src/bridge/envelope.rs` `ErrorCode`: Deterministic Data Plane → Control Plane error codes (never opaque strings)
- `nodedb-physical/src/error.rs`: Plan conversion errors
- `nodedb-wal/src/error.rs`: WAL I/O errors
- pgwire errors: Mapped via `error_to_sqlstate()` in `nodedb/src/control/server/pgwire/types/` to SQLSTATE codes before delivery to client

## Cross-Cutting Concerns

**Logging:** `tracing` crate throughout; structured JSON or pretty format (config: `nodedb/src/config/server/log_format.rs`); root span in `main.rs` provides service context on all events.

**Validation:** RLS injection at planner time (`nodedb/src/control/planner/rls_injection.rs`); per-tenant compute caps enforced at plan time (`nodedb/src/control/planner/sql_plan_convert/`); type guard enforcement at Data Plane execute time (`nodedb/src/data/executor/enforcement/`)

**Authentication:** SCRAM-SHA-256 for pgwire; JWT/JWKS for HTTP; OIDC integration in `nodedb/src/control/security/oidc/`; API keys in `nodedb/src/control/security/apikey/`; session handles in `nodedb/src/control/security/session_handle/`

**Memory Governance:** `MemoryGovernor` in `nodedb-mem/src/governor.rs`; RAII `ReservationToken` released across all four levels (global → database → tenant → engine) atomically; consulted at every allocation site before touching memory.

**Concurrency Model:**
- Control Plane: Tokio multi-thread; all state behind `Arc<Mutex<T>>` or `Arc<RwLock<T>>`
- Data Plane: One `std::thread` per CPU core; no locks, no atomics inside a core; all cross-core sharing is read-only (WAL records, config)
- Bridge: SPSC ring buffers (lock-free); backpressure via `BackpressureController`; fairness via `WeightedFairQueue` (Deficit Round-Robin)

---

*Architecture analysis: 2026-06-13*
