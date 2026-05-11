# Changelog

All notable changes to NodeDB will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
NodeDB uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.2.0] - 2026-05-11

### Added

- **Database primitive** — `CREATE`, `DROP`, `ALTER`, `USE`, and `SHOW DATABASE`; database context bound at connection handshake and propagated through WAL, catalog, routing, and planner
- **CLONE DATABASE** — copy-on-write clone with per-engine row materializer, surrogate ceiling for snapshot isolation, and `SHOW DATABASE LINEAGE FOR`
- **MOVE TENANT** — relocate a tenant's collections between databases
- **Mirror database** — cross-cluster read-only replica via Raft Observer role; lag monitor and automatic restart recovery
- **OIDC authentication** — bearer token auth with provider DDL (`CREATE OIDC PROVIDER`) and catalog persistence
- **Per-database audit** — DML audit mode (`ALTER DATABASE SET AUDIT_DML`), database lifecycle events, `user_id` / `statement_digest` propagated through Data Plane and WAL
- **Per-database quotas** — resource budgets for databases and tenants (`ALTER DATABASE SET QUOTA`); sum-of-quotas enforcement; live cap updates
- **Weighted-fair queue** — per-database DRR dispatch in the SPSC bridge; per-database and per-tenant QPS buckets; connection admission control
- **Per-database metrics** — dedicated Prometheus series per database; per-database CPU budget tracker for compaction enforcement
- **DocCache sharding** — shard document cache by `database_id` with weighted eviction
- **ClusterAdmin role** — cluster-wide admin identity; `GRANT/REVOKE ON DATABASE`; `ALTER USER SET DEFAULT DATABASE`
- **Session registry** — kill-channel per session, hard-revoke on credential change
- **Credential hardening** — persistent lockout state, per-user credential versioning, pre-authentication login rate limiting
- **Continuous aggregate DDL** — `CREATE CONTINUOUS AGGREGATE` with catalog persistence
- **`SHOW AUDIT WHERE`** — filter clause on audit log queries
- **nodedb-client** — graph DSL, field-aware vector ops, text search, and bound-parameter support (`sql_params`) in the native protocol
- **FTS** — crash-safe LSM compaction with dedicated compaction module
- **Memory governor** — over-release counter on `Budget` and `Governor` for accounting correctness

### Fixed

- `DISTINCT` deduplication now operates on projected output, not raw rows
- `ORDER BY` correctly propagated into aggregate plans; derived-`FROM` subqueries supported
- `DROP COLLECTION IF EXISTS` routed through typed handler so the flag is honoured
- Catalog orphan-row violations self-healed at startup
- `EventPlane` drop no longer silently discards pending `WriteEvent`s
- Consumer-disconnect events misclassified as security violations
- ILP measurement names with `/` now route correctly for database-qualified paths

---

## [0.1.0] - 2026-05-07

> First structured release. Ready for pilot deployments and early adopters.
> We welcome feedback before the 1.0 stable release.
> Versions prior to 0.1.0 were alpha iterations.

### Added

#### Engines

- **Document (schemaless)** — MessagePack blobs with secondary indexes, schemaless writes, predicate scans, CRDT sync variant for offline-first workloads
- **Document (strict)** — Binary Tuple encoding with O(1) field extraction, schema enforcement, multi-version `ALTER ADD COLUMN`, CRDT adapter
- **Key-Value** — Hash-indexed O(1) point lookups, native TTL with expiry wheel, optional secondary indexes on value fields, SQL-queryable
- **Columnar** — Compressed column segments (ALP, FastLanes, FSST, Gorilla, LZ4), 1024-row blocks with block statistics, predicate pushdown, delete bitmaps, crash-safe compaction
- **Timeseries** — Cascading compression (20–40× ratios), sparse primary index with block-level min/max skip, continuous aggregation engine with incremental refresh and watermarks, ILP ingest with adaptive batching, approximate aggregates (HLL, t-digest, topK)
- **Spatial** — R\*-tree index with bulk load and nearest-neighbor, geohash and H3 hexagonal indexes, OGC predicates (`ST_Contains`, `ST_Intersects`, `ST_DWithin`, etc.), WKB/WKT/GeoJSON/GeoParquet interchange, hybrid spatial-vector search
- **Vector** — HNSW (in-memory) and Vamana/DiskANN (SSD-resident, billion-scale); quantization: SQ8, PQ, IVF-PQ, OPQ, Binary, Ternary (BitNet 1.58), RaBitQ, BBQ; NaviX adaptive filtered traversal (VLDB 2025); SIEVE workload-routed subindices; MetaEmbed multi-vector with ColBERT MaxSim/PLAID; Matryoshka adaptive-dim; SPFresh streaming index updates; vector-primary collection mode (Pinecone/Qdrant replacement)
- **Array** — ND sparse multi-dimensional engine with dedicated DDL (`CREATE ARRAY ... DIMS ... TILE_EXTENTS`); coordinate-tuple keying; tile-based compression via `nodedb-codec`; Z-order indexing; per-tile MBR statistics; bitemporal cells with `audit_retain_ms` retention; targets genomics, single-cell, earth observation, climate, and sparse ML workloads
- **Graph** (cross-engine overlay) — CSR adjacency index, 13 native algorithms (PageRank, WCC, LabelPropagation, SSSP, Betweenness, Closeness, Louvain, k-Core, and more), Cypher-subset MATCH pattern engine, GraphRAG vector+graph fusion, distributed BSP
- **Full-Text Search** (cross-engine overlay) — Block-Max WAND BM25 with 128-doc block pruning, 16 Snowball stemmers, 27-language stop words, CJK bigram tokenization, posting compression, LSM storage, fuzzy matching, synonyms, phrase proximity, hybrid vector+text RRF fusion

#### Protocols & APIs

- PostgreSQL wire protocol (pgwire) — SQL over standard Postgres clients and drivers
- HTTP/REST — JSON API for document and query operations
- Native binary protocol — MessagePack over TCP for low-latency clients
- WebSocket — real-time sync endpoint for Lite clients
- SQL dialect — standard DML/DDL plus engine-specific extensions (`CREATE ARRAY`, `AS OF`, `MATCH`, vector distance functions)

#### Distributed

- vShard partitioning — tenant, collection, and partition-key based routing
- Multi-Raft consensus — linearizable writes per shard group, leader election, log replication, snapshots
- QUIC transport — low-latency inter-node communication via nexar/quinn
- CRDT sync — Loro-backed offline-first replication; AP local merges promoted to CP at Raft commit; declarative conflict policies; dead-letter queue for constraint-violating deltas
- Cross-engine identity — stable `u32` surrogate per row enabling zero-translation cross-engine joins via roaring-bitmap intersection

#### Event Plane

- AFTER triggers — async dispatch with configurable retry and dead-letter queue
- CDC change streams — consumer groups with offset tracking, per-collection routing
- Cron scheduler — SQL-dispatched recurring jobs with 1-second evaluation loop

#### Query & SQL

- Bitemporal queries — system time + valid time on Document, Columnar, Timeseries, Graph, and Array; `AS OF SYSTEM TIME` / `AS OF VALID TIME` SQL syntax
- HTAP bridge — CDC-driven materialized views from strict → columnar; `CONVERT` DDL between storage modes
- Cross-engine queries — vector + graph + spatial + FTS + metadata in a single query against a shared snapshot watermark; RRF fusion
- Row-level security — per-collection RLS policies evaluated at query time
- Multi-tenancy — tenant isolation with quotas and purge

#### Storage & WAL

- Write-Ahead Log — O_DIRECT via io_uring, group commit, AES-256-GCM encryption per segment, hash-chained audit trail
- Storage tiering — L0 in-memory memtables; L1 NVMe via mmap with async prefetch; L2 S3 cold storage (Parquet, HTTP range requests)
- Compression codecs — ALP, FastLanes, FSST, Gorilla, Pcodec, rANS, LZ4 (per-column selection in `nodedb-codec`)
- Memory governance — per-core jemalloc arenas with per-engine budgets and backpressure thresholds

#### Infrastructure

- Three-plane execution model — Tokio Control Plane, Thread-per-Core Data Plane (io_uring), async Event Plane; connected via bounded lock-free SPSC bridges
- Bounded backpressure — SPSC bridge (85%/95% thresholds) and Event Bus (WAL catchup on overflow); no unbounded queues in the hot path
- Encryption — AES-256-GCM at rest (WAL + columnar segments), TLS in transit for all protocols
- Audit log — hash-chained WAL-backed audit trail, Typeguard-based change tracking, SIEM export

---

[0.2.0]: https://github.com/NodeDB-Lab/nodedb/releases/tag/v0.2.0
[0.1.0]: https://github.com/NodeDB-Lab/nodedb/releases/tag/v0.1.0
