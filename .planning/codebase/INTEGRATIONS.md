# External Integrations

**Analysis Date:** 2026-06-13

## Wire Protocols

NodeDB exposes six external wire protocols. All SQL-capable protocols share the same query planner and execution engine — only transport and encoding differ.

| Protocol | Default Port | Default | TLS | Auth |
|----------|-------------|---------|-----|------|
| pgwire (PostgreSQL) | 6432 | On | Per-protocol toggle | SCRAM-SHA-256 only |
| NDB (Native/MessagePack) | 6433 | On | Per-protocol toggle | Password, API key, OIDC bearer |
| HTTP/JSON | 6480 | On | Per-protocol toggle | Bearer (API key or JWT) |
| Sync/WebSocket (CRDT) | 9090 | On | Separate listener | JWT |
| RESP (Redis) | configurable | Off | Per-protocol toggle | None (no auth on RESP) |
| ILP (InfluxDB Line Protocol) | configurable | Off | Per-protocol toggle | None (write-only ingest) |

### pgwire — PostgreSQL Wire Protocol

**Port:** 6432 (default; override with `NODEDB_PORT_PGWIRE` or `[server.ports] pgwire`)

**Implementation:** `pgwire` v0.38 crate (feature `server-api-aws-lc-rs`)

**Key source files:**
- `nodedb/src/control/server/pgwire/factory.rs` — connection factory, SCRAM auth wiring
- `nodedb/src/control/server/pgwire/handler/` — simple query, extended query, COPY handlers
- `nodedb/src/control/server/pgwire/session/state.rs` — per-connection session state
- `nodedb/src/control/server/pgwire/listener.rs` (via `control/server/listener.rs`) — TCP accept loop

**Capabilities:**
- Simple Query and Extended Query (prepared statements)
- `COPY FROM` bulk ingest
- `LISTEN`/`NOTIFY`
- Cursor support (`DECLARE`, `FETCH`, `WITH HOLD`)
- PostgreSQL transaction state machine (`BEGIN`/`COMMIT`/`ROLLBACK`)
- `dbname=` startup parameter — binds session to a database at handshake time

**Auth:** SCRAM-SHA-256 exclusively. JWT/OIDC bearer tokens are NOT accepted on this protocol — the pgwire wire format has no standard bearer framing. Trust mode (`AuthMode::Trust`) uses a no-op startup handler.

**TLS negotiation:** Follows PostgreSQL SSLRequest flow:
1. Client sends 8-byte `SSLRequest` packet
2. Server replies `S` (TLS available) or `N` (plaintext only)
3. Client initiates TLS handshake on the same connection when server returns `S`

Standard libpq-based drivers (psql, JDBC, tokio-postgres, SQLAlchemy, Prisma) handle this automatically.

**Server version string:** `NodeDB <CARGO_PKG_VERSION>` (reported in `DefaultServerParameterProvider`)

### NDB — Native MessagePack Protocol

**Port:** 6433 (default; override with `NODEDB_PORT_NATIVE` or `[server.ports] native`)

**Implementation:** Custom binary framing with MessagePack payload (`zerompk` v0.5 derive macros; `rmpv` v1 value type)

**Key source files:**
- `nodedb/src/control/server/native/handshake.rs` — 16-byte `HelloFrame` version negotiation (5-second timeout)
- `nodedb/src/control/server/native/dispatch/auth.rs` — auth dispatch (Trust, Password, API key, OIDC bearer)
- `nodedb/src/control/server/native/dispatch/mod.rs` — opcode routing
- `nodedb/src/control/server/native/session.rs` — per-connection session state
- `nodedb/src/control/server/listener.rs` — TCP accept loop (shared with NDB)

**Handshake:** Client sends `HelloFrame` (16 bytes, magic + version range + capabilities). Server replies `HelloAckFrame` with negotiated `proto_ver` and `Limits`.

**Two message modes on the same connection:**
- **SQL mode** — `Sql` message type; SQL text parsed by DataFusion exactly as pgwire
- **Native opcode mode** — 18 single-byte opcodes for engine-specific typed operations, bypassing SQL parsing

**Native opcodes (SDK reference):**

| Opcode | Hex | Operation |
|--------|-----|-----------|
| `TimeseriesScan` | `0x1A` | Time-range scan with optional bucket aggregation |
| `TimeseriesIngest` | `0x1B` | Batch ingest into timeseries collection |
| `SpatialScan` | `0x19` | R*-tree lookup with OGC predicate |
| `KvScan` | `0x72` | Full scan over KV collection |
| `KvGet` | `0x73` | Point lookup by key |
| `KvSet` | `0x74` | Set key-value pair with optional TTL |
| `KvDelete` | `0x75` | Delete by key |
| `KvExpire` | `0x76` | Set TTL on existing key |
| `KvMultiGet` | `0x77` | Batch point lookups |
| `KvMultiSet` | `0x78` | Batch set |
| `KvFieldSet` | `0x79` | Set individual fields on a KV value |
| `DocumentUpdate` | `0x7A` | Update fields on a document by ID |
| `DocumentPatch` | `0x7B` | JSON-patch a document by ID |
| `DocumentGet` | `0x7C` | Fetch a document by ID |
| `DocumentBulkInsert` | `0x7D` | Batch insert documents |
| `DocumentBulkDelete` | `0x7E` | Batch delete by predicate |
| `VectorInsert` | `0x7F` | Insert vector with metadata |
| `VectorSearch` | `0x80` | ANN search (HNSW) with optional pre-filter |
| `VectorDelete` | `0x81` | Delete vector by ID |

**Auth:** Supports Trust, Password (Argon2-hashed), API key (`ndb_...` prefix), and OIDC bearer token (`OidcBearer { token, provider }`). OIDC validation dispatches to `control/security/oidc/verify_bearer_token`.

**Consumers:** `nodedb-client` crate (`remote` feature uses `tokio-postgres`; `native` feature uses this protocol), `nodedb-lite-ffi` (iOS/Android FFI), `nodedb-lite-wasm` (WASM/browser), `ndb` CLI.

### HTTP/JSON API

**Port:** 6480 (default; override with `NODEDB_PORT_HTTP` or `[server.ports] http`)

**Implementation:** `axum` v0.8 (features `ws`) + `axum-server` v0.8 (features `tls-rustls`) + `tower-http` v0.6 (features `cors,trace`)

**Key source files:**
- `nodedb/src/control/server/http/server.rs` — router construction, TLS setup, startup gate middleware
- `nodedb/src/control/server/http/auth.rs` — bearer token resolution (JWT → API key → trust fallback)
- `nodedb/src/control/server/http/routes/` — all handler implementations
- `nodedb/src/control/server/http/version.rs` — `Content-Type: application/vnd.nodedb.v1+json; charset=utf-8`

**Response Content-Type:** All `/v1/` JSON routes stamp `application/vnd.nodedb.v1+json; charset=utf-8` via an `axum::middleware::map_response` layer. SSE and WebSocket routes are on a separate sub-router that does not apply this layer.

**Auth order on HTTP:**
1. JWT Bearer (`Authorization: Bearer eyJ...`) — validated via JWKS registry if configured
2. API key Bearer (`Authorization: Bearer ndb_...`) — validated against in-memory key store
3. Trust mode — no header required, only when `AuthMode::Trust`

**Optional header:** `X-On-Deny` sets the `on_deny_override` mode on `AuthContext` for RLS deny-mode control.

#### HTTP Endpoint Surface

**Probe routes (unversioned, always reachable — bypass startup gate):**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/healthz` | GET | k8s readiness probe (503 until `GatewayEnable` startup phase) |
| `/health/live` | GET | Unconditional liveness |
| `/health/ready` | GET | Readiness (WAL recovered) |
| `/health/drain` | POST | Cooperative drain |
| `/metrics` | GET | Prometheus exposition format (requires `monitor` or `superuser` role) |

**Query execution:**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/query` | POST | Execute SQL, return `application/vnd.nodedb.v1+json` |
| `/v1/query/stream` | POST | Stream results as NDJSON |

**Auth:**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/auth/exchange-key` | POST | Exchange JWT → `nda_` API key; returns `{ api_key, auth_user_id, expires_in }` |
| `/v1/auth/session` | POST | Create session |
| `/v1/auth/session` | DELETE | Delete session |

**CDC (Change Data Capture):**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/cdc/{collection}` | GET | SSE stream; supports `Last-Event-ID` reconnection replay |
| `/v1/cdc/{collection}/poll` | GET | Pull-based polling; params: `since_ms`, `since_lsn`, `limit` |

**Streams:**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/streams/{stream}/events` | GET | Named-stream SSE |
| `/v1/streams/{stream}/poll` | GET | Named-stream long-poll |

**Cluster / status:**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/status` | GET | Node status |
| `/v1/cluster/status` | GET | Cluster status |
| `/v1/cluster/debug/raft/{group_id}` | GET | Raft group diagnostics (requires `debug_endpoints_enabled`) |
| `/v1/cluster/debug/transport` | GET | QUIC transport diagnostics |
| `/v1/cluster/debug/catalog/descriptors` | GET | Metadata catalog dump |
| `/v1/cluster/debug/leases` | GET | Descriptor lease dump |
| `/v1/cluster/debug/quarantined-segments` | GET | Segments in CRC quarantine |

**CRDT:**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/collections/{name}/crdt/apply` | POST | CRDT delta application |

**WebSocket:**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/ws` | GET (upgrade) | WebSocket RPC for SQL execution |

**Observability — PromQL (nested under `/v1/obsv/api/v1/`):**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/v1/obsv/api/v1/query` | GET, POST | PromQL instant query |
| `/v1/obsv/api/v1/query_range` | GET, POST | PromQL range queries |
| `/v1/obsv/api/v1/series` | GET | Series metadata |
| `/v1/obsv/api/v1/labels` | GET | Label names |
| `/v1/obsv/api/v1/label/{name}/values` | GET | Label values |
| `/v1/obsv/api/v1/status/buildinfo` | GET | Build info |
| `/v1/obsv/api/v1/metadata` | GET | Metric metadata |
| `/v1/obsv/api/v1/write` | POST | Prometheus remote write (snappy-compressed protobuf `WriteRequest`) |
| `/v1/obsv/api/v1/read` | POST | Prometheus remote read (snappy-compressed protobuf `ReadRequest`/`ReadResponse`) |
| `/v1/obsv/api/v1/annotations` | POST | Annotations |

**Key file:** `nodedb/src/control/server/http/routes/promql/remote.rs` — Prometheus remote write decodes snappy `WriteRequest`, converts `TimeSeries` to ILP lines, dispatches to the Data Plane timeseries engine.

### Sync/WebSocket — CRDT Sync Protocol

**Port:** 9090 (default; separate WebSocket listener, not the HTTP `/v1/ws` upgrade)

**Implementation:** `tokio-tungstenite` v0.29

**Key source files:**
- `nodedb/src/control/server/sync/session_handler.rs` — full session handler with RLS, audit, DLQ
- `nodedb/src/control/server/sync/security.rs` — JWT validation on connect
- `nodedb/src/control/server/sync/presence/` — presence manager
- `nodedb/src/control/server/sync/shape/` — shape subscription and snapshot registry

**Protocol flow:**
1. Client connects and sends `Handshake` with JWT + vector clock
2. Server responds with `HandshakeAck`
3. Server pushes `DeltaPush` messages (CRDT mutations from `loro`)
4. Client acknowledges with `DeltaAck`
5. Conflicts rejected with `DeltaReject` + `CompensationHint`

**Message types:** `Handshake`, `HandshakeAck`, `DeltaPush`, `DeltaAck`, `DeltaReject`, `Throttle`, `PingPong`, `ResyncRequest`, `ShapeSnapshot`, `ShapeSubscribe`, `TimeseriesPush`

**Auth:** JWT only (OIDC/JWKS validator, validated via `JwtValidator` at handshake)

**Consumers:** NodeDB-Lite (mobile via `nodedb-lite-ffi`, WASM via `nodedb-lite-wasm`, desktop offline-first sync)

### RESP — Redis-Compatible Protocol

**Port:** Configurable (disabled by default; default binding port `6381` when enabled)

**Enable:**
```toml
[server.ports]
resp = 6381
```
Or: `NODEDB_PORT_RESP=6381`

**Implementation:** Custom RESP codec and command parser

**Key source files:**
- `nodedb/src/control/server/resp/listener.rs` — TCP accept loop (default port constant `6381`)
- `nodedb/src/control/server/resp/codec.rs` — RESP frame parser
- `nodedb/src/control/server/resp/command.rs` — command type definitions
- `nodedb/src/control/server/resp/handler_kv.rs`, `handler_hash.rs`, `handler_pubsub.rs`, `handler_sorted.rs` — command handlers

**Supported commands:** `GET`, `SET` (with `EX`/`PX`/`NX`/`XX`), `DEL`, `EXISTS`, `MGET`, `MSET`, `EXPIRE`, `PEXPIRE`, `TTL`, `PTTL`, `PERSIST`, `SCAN`, `KEYS`, `HGET`, `HMGET`, `HSET`, `FLUSHDB`, `DBSIZE`, `SUBSCRIBE`, `PSUBSCRIBE`, `PUBLISH`, `PING`, `ECHO`, `SELECT`, `INFO`, `QUIT`

All RESP commands dispatch to the same KV engine as SQL. Data written via RESP is queryable via SQL on any protocol.

**Auth:** No protocol-level auth on RESP. Rely on TLS + network ACLs for security.

### ILP — InfluxDB Line Protocol

**Port:** Configurable (disabled by default; recommended default `8086`)

**Enable:**
```toml
[server.ports]
ilp = 8086
```
Or: `NODEDB_PORT_ILP=8086`

**Format:** `measurement[,tag=val,...] field=value[,...] [timestamp_ns]`

**Field types:** Float (`1.0`), Int (`42i`), UInt (`42u`), String (`"hello"`), Bool (`true`/`false`)

**Behavior:** Write-only; timestamp optional (server-assigned if omitted); schema auto-inferred from first batch; data lands in the timeseries engine columnar memtable with cascading compression. Query ingested data via SQL on any protocol.

**Implementation:** Custom line-protocol parser; output feeds the same timeseries ingest path as `TimeseriesIngest` native opcode.

---

## Authentication

### SCRAM-SHA-256

**Protocols:** pgwire only

**Implementation:** `pgwire::api::auth::sasl::scram::ScramAuth` backed by `NodeDbAuthSource`

**Source:** `nodedb/src/control/server/pgwire/factory.rs`

**Credential storage:** Salted password (Argon2-hashed) in `CredentialStore`

**Security features:**
- Login rate-limiting (`state.rate_limiter.check_login`) per IP and per username — checked before credential lookup
- Account lockout — checked before returning SCRAM credentials; rejection is indistinguishable from a wrong password on the wire
- Constant-time floor (`AUTH_FLOOR`) enforced on all rejection paths to prevent timing attacks
- Lockout counter driven only from the SASL failure arm (no double-counting)

**Auth modes (configured in `[auth]` section):**
- `Trust` — `NoopStartupHandler`; no credential check
- `Password` — SCRAM-SHA-256 via SASL
- `Certificate` — also routes to SCRAM (mTLS handled at TLS layer)

### API Keys

**Protocols:** NDB native, HTTP

**Format:** `ndb_...` prefix (short-lived auth API keys use `nda_...` prefix)

**Source:** `nodedb/src/control/server/session_auth/mod.rs`, `identity.rs`

**HTTP usage:** `Authorization: Bearer ndb_<token>`

**Create via SQL:** `CREATE API KEY 'name' ROLE readwrite;` (returns key once; store securely)

**Database scoping:** Keys can be restricted to specific databases via `WITH DATABASES (db1, db2)`

**Exchange endpoint:** `POST /v1/auth/exchange-key` — exchanges a JWT for a short-lived `nda_...` API key; source: `nodedb/src/control/server/http/routes/auth_key.rs`

### Session Tokens

**Protocols:** HTTP

**Endpoints:** `POST /v1/auth/session` (create), `DELETE /v1/auth/session` (delete)

**Source:** `nodedb/src/control/server/http/routes/auth_session.rs`

Session handle is fingerprint-bound to the peer socket address (requires `ConnectInfo<SocketAddr>` in axum).

### OIDC / JWT Bearer

**Protocols:** NDB native, HTTP (NOT pgwire)

**Source:** `nodedb/src/control/security/oidc/` (provider catalog), `nodedb/src/control/server/http/auth.rs` (HTTP resolution), `nodedb/src/control/server/native/dispatch/auth.rs` (NDB resolution)

**HTTP resolution order:** JWT format detected by 2-dot token structure → `JwksRegistry.validate()` → falls back to API key check

**Supported JWT algorithms:** ES256, ES384, RS256 (via `p256`/`p384` crates)

**Provider registration (runtime, no restart required):**
```sql
CREATE OIDC PROVIDER auth0 WITH (
    issuer = 'https://your-domain.auth0.com/',
    jwks_url = 'https://your-domain.auth0.com/.well-known/jwks.json',
    audience = 'nodedb-api'
);
```

**JWKS caching:** Fetch on provider registration; refresh on `kid` miss; TTL 1 hour; circuit-breaker fallback to cached JWKS for up to 24 hours on provider outage; explicit reload via `ALTER OIDC PROVIDER ... SET RELOAD_JWKS`

**JWKS fetch client:** `reqwest` v0.12 (features `rustls-tls,json`)

**Claim mapping:** JWT claims → `$auth.*` session variables:

| JWT Claim | Session Variable |
|-----------|-----------------|
| `sub` | `$auth.id` |
| `role` | `$auth.role` |
| `org_id` | `$auth.org_id` |
| `scope` | `$auth.scopes` |

**Legacy JWKS config (backward compat):** `[auth.jwks] providers = [...]` in `nodedb.toml`

### mTLS

**Config:**
```toml
[server.tls]
cert = "/path/to/server.crt"
key = "/path/to/server.key"
client_ca = "/path/to/ca.crt"   # enables mTLS
crl = "/path/to/revocation.crl"  # optional
```

**Priority:** Checked first when a client certificate is present.

---

## TLS

**Library:** `tokio-rustls` v0.26 (features `aws-lc-rs`); `rustls-pemfile` v2 for PEM parsing

**TLS versions:** TLS 1.2 and TLS 1.3 (rustls defaults); TLS 1.0/1.1 not offered

**Cipher suites:** Delegated to rustls defaults

**Default behavior:** All five listeners default to plaintext when no `[server.tls]` section is present.

**Enable TLS:**
```toml
[server.tls]
cert_path = "/etc/nodedb/tls/server.crt"
key_path  = "/etc/nodedb/tls/server.key"
```

**Per-protocol toggles:**
```toml
[server.tls]
pgwire = true
native = true
http   = true
resp   = true
ilp    = false  # example: trusted loopback ingest
```

**Certificate hot-reload:**
```toml
[server.tls]
cert_reload_interval_secs = 3600  # default: 1 hour; 0 to disable
```

**Source:** `nodedb/src/control/server/tls_reload.rs` — background task watches cert/key modification times; atomically swaps `Arc<tokio_rustls::rustls::ServerConfig>` via `watch::Sender` without dropping connections. Reload event is recorded in the audit log.

**HTTP TLS:** `axum_server::bind_rustls` with `RustlsConfig::from_pem_file`; 5-second graceful shutdown on shutdown signal.

---

## Observability Integrations

### Prometheus Scrape Target

**Endpoint:** `GET /metrics` on the HTTP port (6480)

**Format:** Prometheus text exposition format 0.0.4 (`text/plain; version=0.0.4; charset=utf-8`)

**Auth required:** `monitor` or `superuser` role

**Source:** `nodedb/src/control/server/http/routes/metrics.rs`

**Metrics exposed (all custom, no external metrics library):**
- `nodedb_wal_next_lsn` — WAL LSN gauge
- `nodedb_node_id` — cluster node ID
- `nodedb_raft_propose_leader_change_retries_total` — Raft leader-change retry counter
- `nodedb_cluster_state{state="..."}` — one-hot cluster lifecycle phase gauge
- `nodedb_cluster_members`, `nodedb_cluster_groups` — cluster topology gauges
- `nodedb_tenant_*` — per-tenant request, connection, memory, storage, QPS gauges/counters
- `nodedb_audit_total_entries` — audit log counter
- `nodedb_users_active` — credential store user count
- `nodedb_database_*` — per-database quota metrics (QPS, memory, storage, connections, WAL)
- Per-vShard QPS / latency histograms (`per_vshard_metrics`)
- Auth method counters and duration histograms (`auth_metrics`)
- Control-loop health metrics (`loop_metrics_registry`)
- Loop-specific gauges: `raft_tick_loop_pending_groups`, `health_loop_suspect_peers`, `descriptor_lease_loop_leases_held`, `gateway_plan_cache_hit_ratio`, `gateway_plan_cache_hits_total`, `gateway_plan_cache_misses_total`
- Segment quarantine active counts

### Prometheus Remote Write / Read

**Endpoint (write):** `POST /v1/obsv/api/v1/write`

**Encoding:** Snappy-compressed protobuf `WriteRequest` (`Content-Encoding: snappy`)

**Flow:** `WriteRequest` → decode `TimeSeries` → convert to ILP lines → dispatch to timeseries Data Plane

**Endpoint (read):** `POST /v1/obsv/api/v1/read`

**Encoding:** Snappy-compressed protobuf `ReadRequest` → `ReadResponse`

**Protobuf library:** `prost` v0.13; snappy via `snap` v1

**Source:** `nodedb/src/control/server/http/routes/promql/remote.rs`

**Auth:** Requires `ResolvedIdentity` (Bearer token)

### PromQL Engine

**Endpoints:** `/v1/obsv/api/v1/{query,query_range,series,labels,label/{name}/values,status/buildinfo,metadata,annotations}`

**Enabled by default** (`[observability.promql] enabled = true`); disable with `NODEDB_PROMQL_ENABLED=false`

**Source:** `nodedb/src/control/server/http/routes/promql/`

### OTLP (OpenTelemetry Protocol) Receiver

**Disabled by default** (`[observability.otlp.receiver] enabled = false`)

**Ports:**
- HTTP: `0.0.0.0:4318` (default; override `NODEDB_OTLP_HTTP_LISTEN`)
- gRPC: `0.0.0.0:4317` (default; override `NODEDB_OTLP_GRPC_LISTEN`)

**gRPC implementation:** `tonic` v0.14; wire encoding via `prost` v0.13

**Config:**
```toml
[observability.otlp.receiver]
enabled       = true
http_listen   = "0.0.0.0:4318"
grpc_listen   = "0.0.0.0:4317"
```

**Env var enable:** `NODEDB_OTLP_RECEIVER_ENABLED=true`

**Source:** `nodedb/src/config/server/observability.rs`

### OTLP Export (NodeDB telemetry outbound)

**Disabled by default** (`[observability.otlp.export] enabled = false`)

**Config:**
```toml
[observability.otlp.export]
enabled               = false
endpoint              = "http://localhost:4318"
metrics_interval_secs = 15
```

**Env var overrides:** `NODEDB_OTLP_EXPORT_ENABLED`, `NODEDB_OTLP_EXPORT_ENDPOINT`, `NODEDB_OTLP_EXPORT_INTERVAL`

---

## Data Storage Integrations

### Cold Storage — S3-Compatible Object Store

**Purpose:** L2 cold-tier; WAL segments and columnar Parquet files tiered after a configurable TTL

**Client library:** `object_store` v0.13 (features `aws`); `aws-sdk-kms` v1 + `aws-config` v1 for SSE-KMS key wrapping

**Source:** `nodedb/src/config/server/cold_storage.rs`, `nodedb/src/wal/archiver.rs`

**Config:**
```toml
[cold_storage]
endpoint         = ""            # empty = local filesystem (dev)
bucket           = "nodedb-cold"
prefix           = "data/"
region           = "us-east-1"
access_key       = ""            # empty = IAM role / instance credentials
secret_key       = ""
compression      = "zstd"        # zstd | snappy | lz4 | none
tier_after_secs  = 3600
sse_mode         = "aes256"      # aes256 | kms | (omit = bucket default)
```

**SSE modes:**
- `"aes256"` — SSE-S3, S3-managed keys
- `"kms"` — SSE-KMS; `kms_key_id` specifies the KMS CMK ARN; key wrapping via `aws-sdk-kms`

**Format:** Parquet v58 (columnar segments); target row group size configurable (`row_group_size`)

---

## CDC Bridge — Kafka

**Purpose:** Forward change stream events to external Kafka topics; configured per `CREATE CHANGE STREAM` statement

**Client library:** `rdkafka` v0.36 (features `cmake-build`; wraps `librdkafka`)

**Key source files:**
- `nodedb/src/event/kafka/config.rs` — `KafkaDeliveryConfig` parsed from `WITH (DELIVERY='kafka', ...)` clause
- `nodedb/src/event/kafka/producer.rs` — Kafka producer
- `nodedb/src/event/kafka/manager.rs` — lifecycle management

**Config via SQL DDL:**
```sql
CREATE CHANGE STREAM orders_cdc ON orders WITH (
    DELIVERY    = 'kafka',
    BROKERS     = 'kafka1:9092,kafka2:9092',
    TOPIC       = 'orders_cdc',
    FORMAT      = 'json',        -- json | avro
    TRANSACTIONAL = 'true'       -- enables idempotence + transactional.id
);
```

**Formats:** JSON (default), Avro

**Exactly-once semantics:** `TRANSACTIONAL='true'` sets `enable.idempotence = true` and assigns a `transactional.id` on the producer

**Max pending publishes:** 50,000 (default; `DEFAULT_KAFKA_MAX_PENDING`)

**Alternative CDC delivery (HTTP):** `GET /v1/cdc/{collection}` (SSE streaming) and `GET /v1/cdc/{collection}/poll` (pull-based, Kafka Connect / Debezium-style) — no Kafka dependency required for these paths

---

## Inter-Node Cluster Transport

**Protocol:** QUIC (`quinn` v0.11)

**Purpose:** Raft RPC, vShard coordination, cluster membership gossip

**Auth:** HMAC frames (`hmac` v0.12 + `hkdf` v0.12); cluster TLS utilities via `nexar` v0.1

**Source:** `nodedb-cluster` crate — `nodedb/src/` cluster modules; `nodedb-raft` crate for Raft consensus

**Cluster debug endpoints (disabled by default; enable via `[observability] debug_endpoints_enabled = true`):**
- `GET /v1/cluster/debug/raft/{group_id}`
- `GET /v1/cluster/debug/transport`
- `GET /v1/cluster/debug/catalog/descriptors`
- `GET /v1/cluster/debug/leases`
- `GET /v1/cluster/debug/quarantined-segments`

---

## Systemd Integration

**Library:** `sd-notify` v0.4 (Linux only; conditionally compiled)

**Crate:** `nodedb-cluster` dependency

**Usage:** Signals service readiness to systemd on `GatewayEnable` startup phase

---

## Environment Configuration

**Required env vars for production (no config file needed):**

| Variable | Purpose |
|----------|---------|
| `NODEDB_PORT_PGWIRE` | pgwire listen port |
| `NODEDB_PORT_NATIVE` | NDB native listen port |
| `NODEDB_PORT_HTTP` | HTTP API listen port |
| `NODEDB_PORT_RESP` | RESP listen port (enables RESP) |
| `NODEDB_PORT_ILP` | ILP listen port (enables ILP) |
| `NODEDB_PROMQL_ENABLED` | Enable/disable PromQL endpoints |
| `NODEDB_OTLP_RECEIVER_ENABLED` | Enable OTLP ingest |
| `NODEDB_OTLP_HTTP_LISTEN` | OTLP/HTTP address |
| `NODEDB_OTLP_GRPC_LISTEN` | OTLP/gRPC address |
| `NODEDB_OTLP_EXPORT_ENABLED` | Enable OTLP export |
| `NODEDB_OTLP_EXPORT_ENDPOINT` | OTLP collector URL |
| `NODEDB_OTLP_EXPORT_INTERVAL` | Export interval (seconds) |
| `NODEDB_DEBUG_ENDPOINTS_ENABLED` | Enable cluster debug HTTP endpoints |

**Secrets:** No env var contains secret values — credentials are read from `nodedb.toml` config sections (`[cold_storage]`, `[auth]`, `[encryption]`).

---

*Integration audit: 2026-06-13*
