# Technology Stack

**Analysis Date:** 2026-06-13

## Languages

**Primary:**
- Rust (edition 2024) — entire codebase; all 25 workspace crates
- SQL — user-facing query language (parsed by `sqlparser` v0.61)

**Secondary:**
- TOML — configuration format (`nodedb.toml`)
- Protobuf — Prometheus remote write/read and OTLP wire encoding (`prost` v0.13)

## Runtime

**Environment:**
- Minimum Rust version: 1.94 (set in `[workspace.package] rust-version`)
- Docker build uses `rust:1.95-bookworm`
- Production runtime image: `cgr.dev/chainguard/glibc-dynamic:latest` (Wolfi-based, minimal)
- Linux kernel >= 5.1 required for production (io_uring)
- macOS supported for development (io_uring gracefully degrades)

**Package Manager:**
- Cargo
- Lockfile: `Cargo.lock` present and committed

## Workspace Crates

| Crate | Role |
|-------|------|
| `nodedb` | Main server binary + lib; control plane, all protocol listeners, engine orchestration |
| `nodedb-types` | Portable shared type definitions (shared with NodeDB-Lite) |
| `nodedb-bridge` | Lock-free SPSC ring buffer bridging Tokio (Control) and TPC (Data) planes |
| `nodedb-wal` | Write-ahead log with O_DIRECT + io_uring group commit |
| `nodedb-mem` | NUMA-aware memory governor (jemalloc arenas, per-engine budgets) |
| `nodedb-crdt` | CRDT engine; wraps `loro`, SQL constraint validation, dead-letter queue |
| `nodedb-raft` | Raft consensus (leader election, log replication, snapshots) |
| `nodedb-cluster` | vShard coordination, QUIC transport, Raft group management |
| `nodedb-query` | Shared expression evaluator, filters, aggregations, window functions |
| `nodedb-sql` | SQL parser + planner + optimizer (wraps `sqlparser`) |
| `nodedb-physical` | PhysicalTask IR + SqlPlan-to-PhysicalPlan converter |
| `nodedb-codec` | Compression codecs: LZ4, Zstd, Pcodec (ALP), Gorilla |
| `nodedb-columnar` | Columnar segment format and memtable for OLAP storage |
| `nodedb-fts` | Full-text search: inverted index, BM25, analyzers, fuzzy, CJK via `lindera` |
| `nodedb-vector` | HNSW index + distance functions; `acorn-baseline` feature flag for benchmarking |
| `nodedb-vector-gpu` | GPU-accelerated index build via cuVS/CAGRA; `cuvs` feature gate requires CUDA |
| `nodedb-graph` | CSR adjacency index + traversal (PageRank, Cypher-style MATCH) |
| `nodedb-spatial` | R*-tree spatial index + OGC predicates + H3 hexagonal index |
| `nodedb-array` | N-D sparse array engine (coordinate-tuple indexed, tile-based) |
| `nodedb-strict` | Binary Tuple serialization for strict (schema-pinned) document mode |
| `nodedb-client` | Unified `NodeDb` trait + remote client; features `remote` (pgwire) and `native` (MessagePack) |
| `nodedb-test-support` | Shared integration test harness (not published) |
| `nodedb-cluster-tests` | Cluster-level integration tests |
| `nodedb-client-tests` | Client integration tests |

## Async Runtime

**Core:**
- `tokio` v1 (features = "full") — Control Plane and Event Plane
- Data Plane runs on `std::thread` (thread-per-core, `!Send` types); bridged to Tokio via `nodedb-bridge`

## Key Dependencies (Workspace-Level)

**Storage:**
- `redb` v2 — embedded B-tree store; used for system catalog, KV engine, CRDT state, graph CSR
- `memmap2` v0.9 — mmap for HNSW graphs and L1 warm-tier index files
- `io-uring` v0.7 — O_DIRECT WAL writes and Data Plane batched I/O (feature-gated; Linux only)
- `parquet` v58 — L2 cold-tier columnar format
- `object_store` v0.13 (features = "aws") — S3-compatible cold storage client
- `arrow` v58 (features = "ipc") — columnar Arrow format; used in strict mode and result serialization

**Networking / Protocols:**
- `pgwire` v0.38 (features = "server-api-aws-lc-rs") — PostgreSQL wire protocol server implementation
- `axum` v0.8 (features = "ws") — HTTP API server
- `axum-server` v0.8 (features = "tls-rustls") — TLS-aware HTTP listener
- `tower` v0.5 / `tower-http` v0.6 (features = "cors,trace") — middleware
- `tokio-tungstenite` v0.29 — WebSocket for Sync (CRDT) protocol
- `quinn` v0.11 — QUIC transport for inter-node cluster RPCs
- `tokio-rustls` v0.26 (features = "aws-lc-rs") — TLS for all listeners
- `rustls-pemfile` v2 — PEM certificate parsing
- `tonic` v0.14 — gRPC for OTLP ingest/export
- `prost` v0.13 — Protobuf for Prometheus remote write/read and OTLP
- `rdkafka` v0.36 (features = "cmake-build") — Kafka producer for CDC bridge
- `tokio-postgres` v0.7 — used in test harness and `nodedb-client` (remote feature)

**Serialization:**
- `serde` v1 + `serde_json` v1 — JSON serialization throughout
- `sonic-rs` v0.5 — fast JSON parsing on hot paths
- `zerompk` v0.5 (features = "std,derive") — MessagePack for NDB native protocol (custom derive macros)
- `rmpv` v1 — MessagePack value type
- `rkyv` v0.8 (features = "alloc") — zero-copy serialization for RPC payloads
- `toml` v0.8 — configuration file parsing
- `snap` v1 / `flate2` v1 — Snappy/gzip compression (Prometheus remote write)

**Cryptography:**
- `aes-gcm` v0.10 — AES-256-GCM for WAL, segment, and backup encryption
- `argon2` v0.5 — password hashing
- `hmac` v0.12 + `hkdf` v0.12 — HMAC for Raft RPC auth frames and key derivation
- `sha2` v0.10 — hashing
- `p256` v0.13 + `p384` v0.13 (features = "ecdsa") — ES256/ES384 JWT verification
- `subtle` v2 — constant-time comparisons
- `zeroize` v1 — secure memory zeroing
- `aws-sdk-kms` v1 + `aws-config` v1 — AWS KMS key wrapping for cold storage SSE-KMS
- `reqwest` v0.12 (features = "rustls-tls,json") — JWKS fetch for OIDC providers

**CRDT:**
- `loro` v1.13.0 (pinned exact version) — CRDT engine; provides `LoroDoc`, `LoroValue`

**WASM:**
- `wasmtime` v45 (features = "cranelift,runtime") — UDF/stored function execution sandbox

**Compute / Math:**
- `nalgebra` v0.33 — linear algebra for OPQ Procrustes rotation in vector index
- `half` v2 (features = "bytemuck") — f16/bf16 types for vector dimensions
- `bytemuck` v1 (features = "derive") — safe transmutation for SIMD slices
- `roaring` v0.11 — Roaring Bitmap for surrogate ID intersection (cross-engine fused queries)
- `arc-swap` v1 — atomic pointer swap for lock-free index cutover

**Memory:**
- `tikv-jemallocator` v0.6 — global allocator (jemalloc)
- `tikv-jemalloc-ctl` v0.6 (features = "stats") — runtime memory stats
- `libz-sys` v1 (features = "static") — zlib statically linked to avoid runtime libz.so.1 dep

**IDs:**
- `uuid` v1 (features = "v4,v7,serde,js")
- `ulid` v1
- `nanoid` v0.4

**Text Search:**
- `lindera` v2.3 — CJK tokenization (Japanese morphological analysis)
- `icu_segmenter` v1 — Unicode text segmentation
- `whatlang` v0.18 — language detection
- `rust-stemmers` v1 — Snowball stemmers
- `unicode-normalization` v0.1

**Spatial:**
- `h3o` v0.10 — H3 hexagonal spatial index

**Hashing:**
- `rustc-hash` v2 — fast integer hashing (GROUP BY aggregation)
- `xxhash-rust` v0.8 (features = "xxh3") — xxHash3 for cluster key partitioning
- `crc32c` v0.6 — segment and WAL integrity checksums

**Misc:**
- `nexar` v0.1 — cluster transport TLS utilities
- `sqlparser` v0.61 (features = "visitor") — SQL parse tree (used by `nodedb-sql`)
- `tempfile` v3 — external sort spill files
- `rand` v0.9 — election timeouts, weighted pick
- `fluxbench` v0.1 — custom benchmark harness (WAL and bridge benchmarks)
- `proptest` v1 — property-based testing (dev dependency)

## Build Configuration

**Build Profiles:**
- `dev` — `debug = "line-tables-only"`, deps have `debug = false`
- `ci` — inherits `dev`, `debug = false`, `incremental = false`
- `debugging` — inherits `dev`, `debug = true`
- `release` — Cargo default

**Feature Flags:**
- `nodedb/failpoints` — crash-injection points for recovery tests; off by default
- `nodedb-vector/acorn-baseline` — retains ACORN-1 ANN algorithm as benchmark baseline; off by default
- `nodedb-vector-gpu/cuvs` — enables cuVS/CAGRA GPU index build; requires CUDA toolkit + nvcc
- `nodedb-wal/io-uring` — enables io_uring group commit; optional at crate level, always compiled for production
- `nodedb-client/remote` — pgwire (tokio-postgres) client
- `nodedb-client/native` — MessagePack (zerompk) client with TLS
- `nodedb-codec` (WASM) — uses `ruzstd` v0.7 pure-Rust Zstd decoder instead of C libzstd

**Build Environment Variables (baked in at compile time):**
- `NODEDB_GIT_COMMIT` — short git hash
- `NODEDB_BUILD_DATE` — YYYY-MM-DD format
- `NODEDB_BUILD_PROFILE` — `"debug"` or `"release"`
- `NODEDB_RUST_VERSION` — rustc version string

**Build Requirements:**
- `cmake`, `clang`, `libclang-dev`, `pkg-config`, `protobuf-compiler`, `perl` (for C deps including rdkafka)
- CUDA toolkit + nvcc for `nodedb-vector-gpu` with `cuvs` feature

**Docker build** uses `cargo-chef` for layer caching; multi-stage Dockerfile at `/Users/habib/Git/nodedb/Dockerfile`.

## Platform Requirements

**Development:**
- Rust 1.94+
- Linux or macOS (io_uring gracefully degrades on macOS)
- No shell required in production container

**Production:**
- Linux kernel >= 5.1 (io_uring requirement)
- Chainguard `glibc-dynamic` container (Wolfi-based)
- systemd integration via `sd-notify` v0.4 (Linux only; `nodedb-cluster` dependency)

## Configuration

**Config file:** `nodedb.toml` (path: CLI arg > `NODEDB_CONFIG` env var > defaults)

**Key env var overrides (no config file needed):**
- `NODEDB_HOST`, `NODEDB_DATA_DIR`, `NODEDB_MEMORY_LIMIT`
- `NODEDB_PORT_PGWIRE`, `NODEDB_PORT_NATIVE`, `NODEDB_PORT_HTTP`, `NODEDB_PORT_RESP`, `NODEDB_PORT_ILP`
- `NODEDB_PROMQL_ENABLED`, `NODEDB_OTLP_RECEIVER_ENABLED`, `NODEDB_OTLP_HTTP_LISTEN`, `NODEDB_OTLP_GRPC_LISTEN`
- `NODEDB_OTLP_EXPORT_ENABLED`, `NODEDB_OTLP_EXPORT_ENDPOINT`, `NODEDB_OTLP_EXPORT_INTERVAL`
- `NODEDB_DEBUG_ENDPOINTS_ENABLED`, `NODEDB_CONFIG`

**Config sections in nodedb.toml:**
- `[server]` — host, ports, data_dir, data_plane_cores, memory_limit, max_connections, log_format, tls
- `[server.ports]` — pgwire, native, http, resp (optional), ilp (optional)
- `[server.tls]` — cert_path, key_path, cert_reload_interval_secs, per-protocol bool flags
- `[auth]` — auth mode, JWKS providers, session settings, superuser bootstrap
- `[cluster]` — cluster mode, Raft group config, QUIC transport tuning
- `[encryption]` — WAL key_path (AES-256-GCM)
- `[backup_encryption]` — backup KEK key_path
- `[cold_storage]` — S3 endpoint, bucket, region, access_key, sse_mode, kms_key_id
- `[observability]` — promql.enabled, otlp.receiver, otlp.export
- `[engines]` — per-engine memory budgets

---

*Stack analysis: 2026-06-13*
