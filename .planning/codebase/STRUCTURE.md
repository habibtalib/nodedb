# Codebase Structure

**Analysis Date:** 2026-06-13

## Directory Layout

```
nodedb/                          # Workspace root
├── Cargo.toml                   # Workspace manifest, shared dependencies
├── docs/                        # Architecture, protocol, and feature documentation
├── scripts/                     # CI and developer scripts
├── assets/                      # Static assets
├── .github/                     # CI workflows
├── .planning/                   # GSD planning documents
│
├── nodedb/                      # Main server crate (binary + library)
├── nodedb-types/                # Shared type definitions (Apache-2.0)
├── nodedb-physical/             # PhysicalPlan types: Control Plane ↔ Data Plane contract
├── nodedb-bridge/               # SPSC ring buffer, WFQ, backpressure primitives
├── nodedb-wal/                  # Write-ahead log (O_DIRECT + io_uring)
├── nodedb-mem/                  # Memory governor, arena allocator, pressure tracking
├── nodedb-sql/                  # SQL parser, AST, planner, resolver (Apache-2.0)
├── nodedb-query/                # Expression evaluator, scan filters, window functions
├── nodedb-codec/                # Columnar compression codecs (ALP, Gorilla, FastLanes, etc.)
├── nodedb-strict/               # Binary Tuple encoding for strict-schema documents
├── nodedb-columnar/             # Columnar memtable, segment compaction, block statistics
├── nodedb-vector/               # HNSW vector index, quantization, tiered segments
├── nodedb-vector-gpu/           # GPU-accelerated vector operations (optional)
├── nodedb-graph/                # CSR graph index, traversal, sharding
├── nodedb-spatial/              # R*-tree, H3 hex index, OGC geometry predicates
├── nodedb-fts/                  # Full-text search: BM25 analyzer, LSM backend
├── nodedb-array/                # N-dimensional array storage, Hilbert/Z-order codecs
├── nodedb-crdt/                 # CRDT state machines, delta validation, constraint checks
├── nodedb-raft/                 # Raft consensus implementation (leader election, log replication)
├── nodedb-cluster/              # Cluster lifecycle, Calvin sequencer, QUIC transport, catalog
├── nodedb-client/               # Rust SDK client (public API)
├── nodedb-test-support/         # Test fixtures and helpers (dev-only)
├── nodedb-cluster-tests/        # Integration tests: cluster scenarios
└── nodedb-client-tests/         # Integration tests: client SDK
```

## Workspace Crate Ownership

**`nodedb`** (`nodedb/`)
- The server binary (`src/main.rs`) and library
- Owns: bootstrap sequence, all three planes wired together, all six wire protocol handlers, SQL planner integration, Data Plane core loop, all in-process engine state (`SparseEngine`, `VectorCollection`, etc.), Event Plane consumers, security subsystem, cluster coordination glue
- License: BUSL-1.1

**`nodedb-types`** (`nodedb-types/`)
- Portable types shared between Origin (server) and NodeDB-Lite (embedded)
- Owns: `Value`, `ColumnType`, `OrdinalClock`, `Surrogate`, sync wire types, backup envelope, ID types, error codes
- License: Apache-2.0 (shareable across license boundary)

**`nodedb-physical`** (`nodedb-physical/`)
- The typed contract between Control Plane and Data Plane
- Owns: `PhysicalPlan` enum and all per-engine op enums (`DocumentOp`, `VectorOp`, `GraphOp`, `KvOp`, `ColumnarOp`, `TimeseriesOp`, `SpatialOp`, `TextOp`, `CrdtOp`, `QueryOp`, `MetaOp`, `ArrayOp`), `PhysicalTask`, `ConvertContext`
- License: Apache-2.0

**`nodedb-bridge`** (`nodedb-bridge/`)
- Lock-free concurrency primitives for crossing the Tokio/thread-per-core boundary
- Owns: `RingBuffer` (SPSC), `WeightedFairQueue` (Deficit Round-Robin), `BackpressureController`
- Key files: `src/buffer.rs`, `src/wfq.rs`, `src/backpressure.rs`
- License: BUSL-1.1

**`nodedb-wal`** (`nodedb-wal/`)
- Deterministic write-ahead log with O_DIRECT I/O
- Owns: `WalRecord`, `WalWriter`, `GroupCommit` (priority-aware fsync), `MmapReader`, `TombstoneSet`, segment management, encryption, WAL replay
- Key files: `src/group_commit.rs`, `src/uring_writer.rs`, `src/record/wal_record.rs`
- License: BUSL-1.1

**`nodedb-mem`** (`nodedb-mem/`)
- Hierarchical memory governor
- Owns: `MemoryGovernor`, `ReservationToken` (RAII), per-engine `Budget`, pressure states, arena allocator
- Key file: `src/governor.rs`
- License: BUSL-1.1

**`nodedb-sql`** (`nodedb-sql/`)
- SQL parser and logical planner
- Owns: `sqlparser`-based AST, DDL AST types, query planner, resolver, type system for SQL expressions
- Key directories: `src/ddl_ast/`, `src/planner/`, `src/resolver/`
- License: Apache-2.0

**`nodedb-query`** (`nodedb-query/`)
- Shared query execution primitives used by both Control Plane and Data Plane
- Owns: expression evaluator, scan filters, window functions, aggregate keys, cast rules, FTS scoring
- Key directories: `src/expr/`, `src/functions/`, `src/scan_filter.rs`

**`nodedb-codec`** (`nodedb-codec/`)
- Columnar compression codec library
- Owns: ALP, Gorilla (float delta), FastLanes, delta encoding, FSST string compression, LZ4, zstd, pcodec, vector quantization (BBQ, OPQ, RaBitQ, ternary)
- Key directories: `src/vector_quant/`, `src/fastlanes/`

**`nodedb-strict`** (`nodedb-strict/`)
- Binary Tuple serialization for strict-schema document mode
- Owns: `encode`, `decode`, `arrow_extract` (O(1) field extraction without full deserialization)
- Key files: `src/encode.rs`, `src/decode.rs`
- License: Apache-2.0

**`nodedb-columnar`** (`nodedb-columnar/`)
- Columnar engine: memtable, segment files, compaction, block statistics, predicate pushdown
- Owns: `ColumnarMemtable`, segment read/write, per-column compression pipeline, delete bitmaps
- Key directories: `src/memtable/`, `src/compaction/`

**`nodedb-vector`** (`nodedb-vector/`)
- HNSW vector index with tiered segments and quantization
- Owns: `VectorCollection`, HNSW graph building, ANN search, OPQ/scalar quantization, delta index, sidecar persistence
- Key directories: `src/collection/`, `src/codec_index/`, `src/delta/`

**`nodedb-vector-gpu`** (`nodedb-vector-gpu/`)
- GPU-accelerated vector operations (optional feature)
- Owns: CUDA/OpenCL dispatch for distance computations

**`nodedb-graph`** (`nodedb-graph/`)
- CSR (Compressed Sparse Row) graph index
- Owns: `ShardedCsrIndex`, CSR compaction, traversal primitives, edge weights
- Key files: `src/sharded.rs`, `src/csr/mod.rs`, `src/traversal.rs`

**`nodedb-spatial`** (`nodedb-spatial/`)
- Spatial index and OGC geometry operations
- Owns: R*-tree (`src/rtree/`), H3 hexagonal index, geohash index, WKB/WKT parsing, OGC predicates (contains, intersects, distance)

**`nodedb-fts`** (`nodedb-fts/`)
- Full-text search engine
- Owns: BM25 analyzer pipeline, language-specific stemmers/stop words, CJK bigram segmenter, n-gram, synonym support, LSM-backed posting list storage
- Key directories: `src/analyzer/`, `src/backend/`, `src/lsm/`

**`nodedb-array`** (`nodedb-array/`)
- N-dimensional array (tensor) storage
- Owns: tile-based codec (Hilbert and Z-order space-filling curves), coordinate encoding, array compaction
- Key directories: `src/codec/`, `src/coord/`

**`nodedb-crdt`** (`nodedb-crdt/`)
- CRDT state machine and delta validation
- Owns: CRDT state (`src/state/`), delta validator (`src/validator/`), constraint enforcement, dead-letter queue logic, signing
- Uses: `loro` CRDT library (workspace dep)

**`nodedb-raft`** (`nodedb-raft/`)
- Raft consensus protocol implementation
- Owns: `RaftNode`, log storage, RPC (AppendEntries, RequestVote, InstallSnapshot), state machine, snapshot framing
- Key files: `src/node/core.rs`, `src/log.rs`, `src/storage.rs`

**`nodedb-cluster`** (`nodedb-cluster/`)
- Cluster lifecycle and distributed coordination
- Owns: Multi-Raft group management, Calvin sequencer (cross-shard transactions), QUIC transport, cluster catalog, schema replication, bootstrap/join protocol, readiness signaling
- Key directories: `src/calvin/`, `src/bootstrap/`, `src/catalog/`

**`nodedb-client`** (`nodedb-client/`)
- Public Rust SDK
- Owns: NDB protocol client, typed SDK methods (`get`, `put`, `vector_search`, `sql`, etc.), connection pool

**`nodedb-test-support`** (`nodedb-test-support/`)
- Test utilities (dev-dependency only; no version published)

**`nodedb-cluster-tests`** and **`nodedb-client-tests`**
- Integration test suites; in separate crates to allow parallel compilation and isolation

## Directory Purposes: `nodedb/src/`

```
nodedb/src/
├── main.rs              # Binary entry point (bootstrap sequence, listener spawning)
├── lib.rs               # Library entry point (module re-exports)
├── error.rs             # Top-level Error enum
├── error_from.rs        # From impls for external error types
├── version.rs           # VERSION, GIT_COMMIT, BUILD_DATE constants
├── fail_point.rs        # Test fail-point injection (cfg-gated)
├── util.rs              # Misc utilities
│
├── bootstrap/           # Startup orchestration (called from main.rs)
│   ├── mod.rs
│   ├── data_plane.rs    # Spawn Data Plane cores, load array catalog
│   ├── listeners.rs     # Bind and spawn all protocol listeners
│   ├── wal_init.rs      # Open + replay WAL, build TombstoneSet
│   ├── state_wiring.rs  # Wire subsystems into SharedState post-open
│   ├── cluster_ready.rs # Await Raft readiness, fire startup gates
│   ├── background_loops.rs # Spawn response poller + Event Plane
│   ├── credentials.rs   # Superuser bootstrap, surrogate WAL replay
│   ├── tls.rs           # TLS acceptor construction
│   ├── tracing_init.rs  # tracing-subscriber setup
│   └── signal.rs        # SIGTERM / SIGINT handler spawning
│
├── bridge/              # Control Plane side of SPSC bridge
│   ├── dispatch.rs      # Dispatcher: WFQ + SPSC routing to Data Plane cores
│   ├── envelope.rs      # Request / Response / PhysicalPlan / ErrorCode types
│   ├── slab.rs          # (Future) slab-backed zero-copy payload
│   └── quiesce/         # CollectionQuiesce: safe scan drain for PURGE
│
├── config/              # Configuration types
│   ├── mod.rs           # ServerConfig root
│   ├── auth/            # Auth config (mode, session, superuser)
│   ├── server/          # Port, TLS, paths, cluster, scheduler, retention, etc.
│   └── engine.rs        # Per-engine tuning config
│
├── control/             # Control Plane (Tokio): all Send + Sync code
│   ├── state/           # SharedState struct definition + initialization
│   ├── planner/         # Query planner: SQL → PhysicalTask conversion
│   │   ├── sql_plan_convert/  # Core conversion: DataFusion AST → PhysicalPlan
│   │   ├── calvin/            # Cross-shard transaction planner
│   │   ├── procedural/        # Stored procedure executor
│   │   └── wasm/              # WASM UDF planner
│   ├── server/          # Protocol handlers
│   │   ├── pgwire/      # PostgreSQL wire protocol
│   │   │   ├── handler/ # SQL execution, prepared statements, routing, DDL dispatch
│   │   │   ├── ddl/     # All DDL statement handlers (CREATE, ALTER, DROP, ...)
│   │   │   ├── session/ # Session state (transaction, parameters)
│   │   │   └── pg_catalog/ # Information schema / pg_catalog virtual tables
│   │   ├── native/      # NDB binary protocol (opcode dispatcher + plan_builder)
│   │   ├── http/        # Axum-based HTTP API (routes/, auth)
│   │   ├── resp/        # Redis RESP listener
│   │   ├── sync/        # WebSocket CRDT sync listener
│   │   └── admission/   # Per-database/tenant connection semaphore registry
│   ├── security/        # Auth, permissions, RLS, rate limiting, audit
│   │   ├── credential/  # SCRAM / Argon2 credential store
│   │   ├── permission/  # Collection-level ACLs
│   │   ├── rls/         # Row-Level Security policy store
│   │   ├── ratelimit/   # Token bucket rate limiter
│   │   ├── jwks/        # JWKS registry for JWT validation
│   │   └── sessions/    # Active session registry
│   ├── cluster/         # Cluster coordination (wraps nodedb-cluster)
│   │   └── calvin/      # Calvin OLLP executor + scheduler driver
│   ├── backup/          # Backup orchestration + restore
│   ├── maintenance/     # Per-database CPU-budget maintenance scheduler
│   ├── metrics/         # Prometheus + system metrics
│   └── cascade/         # Materialized view CDC, change streams, sequences
│
├── data/                # Data Plane (!Send): physical execution
│   ├── executor/        # Per-core execution engine
│   │   ├── core_loop/   # CoreLoop struct, tick/drain/poll
│   │   ├── dispatch/    # execute() → per-engine sub-dispatchers
│   │   │   ├── document.rs, kv.rs, vector.rs, graph.rs, ...
│   │   │   └── bitmap/  # Roaring bitmap intersection helpers
│   │   ├── handlers/    # Fine-grained operation handlers
│   │   │   ├── document/, kv/, columnar_read/, columnar_write/, ...
│   │   │   └── transaction/  # ROLLBACK / SAVEPOINT undo
│   │   └── task.rs      # ExecutionTask (wraps Request + state)
│   └── io/              # IoMetrics (per-tier wait latency tracking)
│
├── engine/              # In-process engine state (owned by CoreLoop)
│   ├── sparse/          # redb-backed sparse/metadata engine + document cache
│   │   ├── btree_versioned/ # B-tree with MVCC versioning
│   │   ├── fts_redb/    # FTS posting list storage on redb
│   │   └── inverted/    # Inverted index
│   ├── document/        # Document engine (schemaless MessagePack)
│   ├── kv/              # KV engine (hash table + sorted index)
│   ├── vector/          # VectorCollection wrapper (wraps nodedb-vector)
│   ├── graph/           # Graph engine (wraps nodedb-graph, edge store)
│   ├── timeseries/      # Timeseries engine state (columnar memtable, partitions)
│   ├── spatial/         # Spatial engine state (R*-tree instances)
│   ├── crdt/            # CRDT tenant state (wraps nodedb-crdt)
│   ├── array/           # ND-array engine state (wraps nodedb-array)
│   └── bitemporal/      # Bitemporal tracking helpers
│
├── event/               # Event Plane (Tokio): async side effects
│   ├── bus.rs           # EventBus: per-core ring buffers, EventProducer/Consumer
│   ├── types.rs         # WriteEvent, WriteOp, EventSource
│   ├── cdc/             # Change Data Capture (streams, consumer groups, registry)
│   ├── trigger/         # AFTER trigger dispatcher (batch + single), DLQ
│   ├── scheduler/       # Cron job scheduler
│   ├── webhook/         # HTTP webhook delivery + retry
│   ├── kafka/           # Kafka bridge (rdkafka)
│   ├── streaming_mv/    # Streaming materialized view processor
│   ├── topic/           # Durable pub/sub topics
│   └── alert/           # Alert rule executor + hysteresis
│
├── memory/              # Memory governor initialization helpers
├── query/               # Control Plane query context + DataFusion integration
├── storage/             # Storage quarantine registry
├── types/               # Internal type aliases (re-exports from nodedb-types)
└── wal/                 # WAL manager (wraps nodedb-wal for Control Plane use)
```

## Key File Locations

**Entry Points:**
- `nodedb/src/main.rs`: Server binary (bootstrap, listener spawning)
- `nodedb/src/lib.rs`: Library entry point

**Core Contracts:**
- `nodedb-physical/src/physical_plan/mod.rs`: `PhysicalPlan` enum (all engine ops)
- `nodedb/src/bridge/envelope.rs`: `Request` / `Response` / `ErrorCode` bridge types
- `nodedb-types/src/value/core.rs`: `Value` (runtime dynamic value)
- `nodedb-types/src/columnar/column_type.rs`: `ColumnType` (static schema type)

**Control Plane Wiring:**
- `nodedb/src/control/state/fields.rs`: `SharedState` — all CP shared state
- `nodedb/src/bridge/dispatch.rs`: `Dispatcher` — routes to Data Plane cores

**SQL Planning Path:**
- `nodedb/src/control/server/pgwire/handler/sql_exec.rs`: Entry point for SQL execution
- `nodedb/src/control/server/pgwire/handler/routing/planning.rs`: `plan_statement_to_tasks()`
- `nodedb/src/control/planner/sql_plan_convert/convert.rs`: AST → PhysicalTask conversion
- `nodedb/src/control/server/native/dispatch/plan_builder/mod.rs`: Opcode → PhysicalPlan (SDK path)

**Data Plane Core:**
- `nodedb/src/data/executor/core_loop/state.rs`: `CoreLoop` struct
- `nodedb/src/data/executor/core_loop/tick.rs`: `drain_requests()`, `poll_one()`
- `nodedb/src/data/executor/dispatch/mod.rs`: `execute()` — PhysicalPlan dispatch

**Storage:**
- `nodedb-wal/src/group_commit.rs`: Priority-aware WAL fsync
- `nodedb-wal/src/uring_writer.rs`: io_uring writer
- `nodedb-strict/src/encode.rs`, `decode.rs`: Binary Tuple codec
- `nodedb-mem/src/governor.rs`: `MemoryGovernor`

**Security:**
- `nodedb/src/control/security/ratelimit/limiter.rs`: Token-bucket rate limiter
- `nodedb/src/control/server/listener.rs`: Connection semaphore (two-phase admission)
- `nodedb/src/control/planner/rls_injection.rs`: RLS policy injection

**Cluster:**
- `nodedb-raft/src/node/core.rs`: Raft node state machine
- `nodedb-cluster/src/calvin/sequencer/mod.rs`: Calvin sequencer
- `nodedb/src/control/cluster/calvin/executor/ollp/`: OLLP orchestrator

**Testing:**
- `nodedb/tests/`: Integration tests for the main crate
- `nodedb-cluster-tests/tests/`: Cluster integration tests
- `nodedb-client-tests/tests/`: Client SDK integration tests
- `nodedb-test-support/src/`: Shared test fixtures

**Documentation:**
- `docs/architecture.md`: Three-plane model, storage tiers, resource governance
- `docs/protocols.md`: All six wire protocols, ports, TLS

## Naming Conventions

**Files:**
- `mod.rs` for module entry points
- `snake_case.rs` for all source files
- Handlers named after what they handle: `sql_exec.rs`, `copy_handler.rs`, `dispatch.rs`
- Per-engine files consistently named: `document.rs`, `vector.rs`, `kv.rs`, `graph.rs`, etc. within dispatch/handlers directories

**Directories:**
- `snake_case` for all directories
- Engine-specific directories repeat the engine name: `engine/vector/`, `engine/graph/`, `engine/kv/`
- Handler sub-directories mirror the operation domain: `handlers/document/`, `handlers/kv/`, etc.

**Types:**
- Structs and enums: `PascalCase`
- Traits: `PascalCase` (e.g., `DatabasePriorityResolver`)
- Error types: `PascalCase` with `thiserror`-derived `Display`

**Modules:**
- Public API modules re-exported from `lib.rs` or crate root
- Internal impl split across `fields.rs` (struct def), `init.rs` (construction), `methods.rs` (behavior) for large types like `SharedState`

## Where to Add New Code

**New Wire Protocol Feature (DDL statement):**
- DDL parser extension: `nodedb-sql/src/ddl_ast/`
- DDL handler: `nodedb/src/control/server/pgwire/ddl/<category>/`
- Physical op variant: `nodedb-physical/src/physical_plan/<engine>.rs`
- Data Plane handler: `nodedb/src/data/executor/dispatch/<engine>.rs` + `nodedb/src/data/executor/handlers/<engine>/`

**New Engine Operation:**
- Add variant to `PhysicalPlan` in `nodedb-physical/src/physical_plan/mod.rs`
- Add op enum variant in `nodedb-physical/src/physical_plan/<engine>.rs`
- Add SQL → PhysicalPlan conversion in `nodedb/src/control/planner/sql_plan_convert/`
- Add native opcode plan builder in `nodedb/src/control/server/native/dispatch/plan_builder/<engine>.rs`
- Add Data Plane executor in `nodedb/src/data/executor/dispatch/<engine>.rs`

**New Engine (storage model):**
- New crate under workspace (follow `nodedb-*` naming)
- Engine state lives in `nodedb/src/engine/<engine>/`
- Register in `CoreLoop` struct (`nodedb/src/data/executor/core_loop/state.rs`)
- Add `PhysicalPlan` variant + sub-enum in `nodedb-physical`
- Wire checkpoint / compaction into `nodedb/src/data/executor/core_loop/maintenance.rs`
- Register maintenance task in `nodedb/src/control/maintenance/`

**New Event Plane Consumer:**
- Location: `nodedb/src/event/<consumer_name>/`
- Register in `nodedb/src/event/bus.rs`
- Persist recovery state in WAL if crash-safe delivery is needed

**New API Endpoint (HTTP):**
- Location: `nodedb/src/control/server/http/routes/<name>.rs`
- Register in `nodedb/src/control/server/http/routes/mod.rs`

**Utilities and Helpers:**
- Cross-crate types: `nodedb-types/src/`
- Query execution helpers: `nodedb-query/src/`
- Compression codecs: `nodedb-codec/src/`

## Special Directories

**`target/`:**
- Purpose: Cargo build output
- Generated: Yes
- Committed: No

**`.planning/`:**
- Purpose: GSD planning and codebase analysis documents
- Generated: By GSD tooling
- Committed: Yes

**`docs/`:**
- Purpose: User-facing architecture and protocol documentation
- Generated: No (hand-authored)
- Committed: Yes

---

*Structure analysis: 2026-06-13*
