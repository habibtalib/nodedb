# Codebase Concerns

**Analysis Date:** 2026-06-13
**Focus Branch:** codex/fix-issue-142 (pgwire server_version enhancement, already merged to main)

---

## Tech Debt

**EdgeId parallel-edge seq allocator not wired:**
- Issue: `EdgeId::seq` is always `0` via `EdgeId::try_first`. The engine-side allocator for monotonically increasing `seq` values (which enables multiple parallel edges between the same `(src, dst, label)` pair) is explicitly marked as a follow-up TODO.
- Files: `nodedb-types/src/id/edge.rs:19-53`
- Impact: Parallel edges between the same node pair with the same label silently collide; the second write overwrites rather than creates a distinct edge. Graph queries relying on multi-edge cardinality will return incorrect results until wired.
- Fix approach: Implement an atomic sequence allocator in `nodedb/src/engine/graph/` as indicated in the comment at line 53.

**Snapshot GC only runs at startup:**
- Issue: `sweep_orphans` in the install-snapshot GC module is called once at node startup and never again. Orphaned `.partial` snapshot files accumulate at runtime until the next restart.
- Files: `nodedb-cluster/src/install_snapshot/gc.rs:14-17`
- Impact: Under heavy shard migration traffic, stale partial snapshots consume unbounded disk space between restarts.
- Fix approach: Wire `sweep_orphans` into the existing periodic-task infrastructure (every ~60 s), as noted in the file's TODO comment.

**Decommission does not propagate cluster-wide ShutdownWatch:**
- Issue: When the `DecommissionSubsystem` observer fires (node leaving cluster), the cooperative exit signal is sent only to the subsystem-local shutdown channel. The broader cluster `ShutdownWatch` propagation is a `warn!`-level placeholder.
- Files: `nodedb-cluster/src/subsystem/impls/decommission_subsystem.rs:73-86`
- Impact: A decommissioned node may not shut down cleanly — SWIM detector, Raft loops, and transport accept loops may continue running after the node logically leaves.
- Fix approach: Wire `cluster_shutdown_tx.send(true)` at the `// Wiring point` comment when the cluster-wide ShutdownWatch initiative lands.

**SystemTime::now() scattered without centralised helper:**
- Issue: `nodedb-types/src/wire_time.rs` provides `current_wall_ms()` which logs once-per-process when the system clock is pre-epoch, but the TODO at line 37 acknowledges that direct `SystemTime::now()` callers throughout the codebase bypass this helper.
- Files: `nodedb-types/src/wire_time.rs:37`
- Impact: Clock-before-epoch edge cases are silently swallowed at scattered sites rather than alerting operators.
- Fix approach: Audit and funnel direct `SystemTime::now()` callers through `current_wall_ms()`.

---

## Known Bugs

**`_max_rows` ignored in extended-query portal execution:**
- Symptoms: When a PostgreSQL client sends an `Execute` message with a non-zero `max_rows` parameter (requesting a partial portal fetch / portal suspension), NodeDB ignores the parameter and always returns all rows. The `_max_rows: usize` argument in `execute_prepared` is prefixed with `_` and never consulted.
- Files: `nodedb/src/control/server/pgwire/handler/prepared/execute.rs:32`
- Trigger: Any client library that uses portal-based pagination (e.g. JDBC with `fetchSize`, libpq `PQsetSingleRowMode`, pg's `cursor` in asyncpg) will receive full result sets on first Execute rather than paginated batches. The `PortalSuspended` message is never sent.
- Workaround: Use SQL-level `LIMIT`/`OFFSET` pagination or server-side `DECLARE CURSOR` / `FETCH n`.
- Impact for DB Studio: An IDE that pages large result sets via Execute max_rows will get everything at once, which can be a memory concern for large collections.

**`SHOW server_version` and `version()` function use distinct formats:**
- Issue: `SHOW server_version` returns `"NodeDB X.Y.Z"` (via `session_cmds.rs:483` and `factory.rs:158`). The `version.rs` module (introduced in branch `codex/fix-issue-142`) adds `version_banner()` returning `"PostgreSQL 16.0 (NodeDB X.Y.Z / commit <hash>)"` and `server_version_num` = `"160000"`. The `version.rs` file exists on the branch commit but was not yet committed to the working tree (the file is listed as `??` untracked in the git status). The `version.rs` module is declared in `pgwire/mod.rs` but may not be wired if the file is absent.
- Files: `nodedb/src/control/server/pgwire/version.rs` (branch only, not in working tree), `nodedb/src/control/server/pgwire/factory.rs:158`, `nodedb/src/control/server/pgwire/handler/session_cmds.rs:483`, `nodedb/src/control/server/pgwire/mod.rs`
- Trigger: `SELECT version()` via psql or a driver that inspects the server banner (e.g. DBeaver, DataGrip, TablePlus) will see `"NodeDB X.Y.Z"` rather than the PostgreSQL-compatible `"PostgreSQL 16.0 ..."` banner. Many drivers parse the banner to enable/disable features.
- Impact for DB Studio: A VS Code extension that calls `SELECT version()` to show a connection header will get a non-PostgreSQL-compatible string, potentially breaking version-sniffing logic.

---

## Security Considerations

**Trust mode is a footgun with no runtime warning suppression path:**
- Risk: `AuthMode::Trust` bypasses all credential verification on all protocol entry points (pgwire, native, HTTP, RESP). A misconfigured production deployment with `mode = "trust"` allows any connection to authenticate as any user.
- Files: `nodedb/src/config/auth/config.rs:81`, `nodedb/src/bootstrap/credentials.rs:63-93`
- Current mitigation: A banner warning is logged at startup when Trust mode is active (`credentials.rs:69`). The code path is correctly labeled "Development/testing only".
- Recommendations: Consider requiring an explicit `NODEDB_TRUST_MODE=1` environment variable in addition to the config value; refuse Trust mode when `NODEDB_ENV=production` is set.

**HTTP Password mode sends password cleartext:**
- Risk: `AuthMode::Password` is documented as "cleartext over HTTP" at `config.rs:82`. There is no HTTP-layer enforcement that TLS must be active when password auth is used. The `TlsPolicy` struct has `reject_cleartext: false` as its default, meaning cleartext connections are accepted even when `mode = "password"`.
- Files: `nodedb/src/config/auth/config.rs:82`, `nodedb/src/control/security/tls_policy.rs:15-26`
- Current mitigation: HTTP auth uses Bearer tokens (JWT or API keys), not username+password directly. The HTTP session exchange endpoint at `/v1/auth/exchange-key` requires a valid JWT or API key. Argon2 verify happens only on pgwire SCRAM and native protocol paths.
- Recommendations: The comment "cleartext over HTTP" at `config.rs:82` may be misleading — confirm exactly which HTTP path (if any) accepts raw passwords and document or remove that path. Enforce `reject_cleartext = true` in production docs.

**`insecure_transport = true` disables all QUIC peer verification:**
- Risk: Setting `insecure_transport = true` in cluster config removes all certificate validation on the Raft QUIC transport, allowing any network peer to inject Raft RPCs. The config doc says "any network peer reaching the QUIC port can forge Raft RPCs."
- Files: `nodedb/src/config/server/cluster.rs:84-96`
- Current mitigation: Documented as a security escape hatch, defaults to `false`.
- Recommendations: Add a startup hard-fail if `insecure_transport = true` and `NODEDB_ENV=production` (or equivalent flag) to prevent accidental production deployment.

**Password warning on SCRAM path not surfaced to client:**
- Risk: When a user's password is in a grace-period-expired or must-change state, `factory.rs` logs a `tracing::warn!` server-side but does not send a `NoticeResponse` to the pgwire client. The user receives no warning.
- Files: `nodedb/src/control/server/pgwire/factory.rs:128-132`
- Current mitigation: Warning IS surfaced on the native protocol path (`session_auth::authenticate`). The pgwire gap is acknowledged in a code comment.
- Recommendations: Plumb a post-auth notice hook to deliver `SQLSTATE 01006` (privilege_not_granted as warning) or a custom notice after SCRAM succeeds.

---

## Performance Bottlenecks

**Cursors fully materialize result sets in memory:**
- Problem: Server-side cursors (`DECLARE CURSOR FOR SELECT ...`) eagerly execute the full query and store all result rows as `Vec<String>` (JSON strings) in session state at DECLARE time. `FETCH n` then slices from this in-memory buffer.
- Files: `nodedb/src/control/server/pgwire/session/state.rs:33-41`, `nodedb/src/control/server/pgwire/handler/cursor_query.rs`
- Cause: There is no lazy streaming cursor execution. The entire result must fit in per-connection session memory.
- Improvement path: Implement lazy cursor execution backed by a streaming iterator from the Data Plane, materializing rows on demand at `FETCH` time. Short-term mitigation: the `cursor_spill.rs` module enforces a row cap (`enforce_cursor_limit`).

**EventFd wake signaling is Linux-only; macOS falls back to polling:**
- Problem: `nodedb/src/data/eventfd.rs` and `nodedb-bridge/src/eventfd.rs` use `libc::eventfd` which is Linux-only. On macOS (common dev environment), the bridge falls back to a different mechanism. The fallback path may involve a timeout-based poll rather than interrupt-driven wake.
- Files: `nodedb/src/data/eventfd.rs`, `nodedb-bridge/src/eventfd.rs`, `nodedb/src/data/runtime.rs:9`
- Cause: `eventfd` is a Linux kernel primitive. The comment in `runtime.rs` says it "replaces the naive `sleep(50µs)` busy-poll" — on non-Linux the old behaviour may be retained.
- Improvement path: On macOS, kqueue-based waking (using a `kevent` pipe) would provide equivalent interrupt-driven semantics. Track the actual fallback implementation in `nodedb-bridge/src/eventfd.rs` for non-Linux targets.

---

## Fragile Areas

**pg_catalog virtual table dispatch uses substring matching:**
- Files: `nodedb/src/control/server/pgwire/pg_catalog/dispatch.rs:181-208`
- Why fragile: `extract_pg_catalog_table` uses `contains_word` on the uppercased SQL string to detect virtual table references. Complex queries with JOINs, subqueries, or CTEs that reference a real user collection whose name contains a pg_catalog table name fragment could be misrouted. Queries like `SELECT * FROM pg_class_extensions` would potentially match `pg_class`.
- Safe modification: Use the `vquery` SQL parser to extract the FROM target rather than string-matching. The `contains_word` boundary check partially mitigates this but is not a full parser.
- Test coverage: `nodedb/tests/pg_catalog_select_semantics.rs` and `nodedb/tests/pg_catalog_oid_stability.rs` test known cases but do not cover edge cases with ambiguous identifiers.

**Binary parameter format is rejected for NUMERIC/TIMESTAMP/TIMESTAMPTZ but silently accepted for all other types:**
- Files: `nodedb/src/control/server/pgwire/handler/prepared/execute.rs:200-225`
- Why fragile: The binary rejection guard covers only three types. All other types (DATE, TIME, UUID, BYTEA, INTERVAL, OID, etc.) that arrive in binary format are silently passed through as UTF-8 text. If a client sends genuinely binary-encoded bytes for, say, a `DATE` parameter (4-byte big-endian integer), the server would attempt to interpret those bytes as UTF-8 text, producing a nonsensical string value.
- Safe modification: Expand the binary rejection to all types where the binary wire encoding is not identical to the text encoding, or implement proper binary decoders for the commonly used types (INT2/INT4/INT8 big-endian, FLOAT4/FLOAT8 IEEE 754, UUID 16-byte).
- Impact for DB Studio: ORM drivers (e.g. SQLAlchemy, TypeORM, prisma) often send parameters in binary format for performance. Without binary support, they must be forced into text mode, which some drivers do not support as a per-parameter override.

**Cursor BACKWARD fetch materializes entire result set:**
- Files: `nodedb/src/control/server/pgwire/session/state.rs:33-41`, `nodedb/src/control/server/pgwire/handler/cursor_cmds.rs:97-101`
- Why fragile: `DECLARE ... SCROLL CURSOR` is accepted (the `scrollable` flag is set) but backward scrolling works only because all rows were pre-fetched. A SCROLL cursor against a large collection will OOM before the client issues any FETCH.
- Safe modification: Document and enforce a row-count cap for SCROLL cursors; the `cursor_spill.rs` row-limit helper already exists but needs to be clearly applied to SCROLL cursors specifically.

**SQL splitting by top-level semicolons is custom-implemented:**
- Files: `nodedb/src/control/server/pgwire/handler/sql_split.rs`
- Why fragile: Multi-statement queries are split by a custom tokenizer rather than the full sqlparser. String literals containing semicolons, dollar-quoted strings (`$$...;...$$`), and C-style escape strings (`E'...\;...'`) could cause incorrect splits. psql's `\c` heredoc syntax is particularly prone.
- Safe modification: Use sqlparser's tokenizer to identify statement boundaries rather than scanning for `;` characters.

---

## Scaling Limits

**Per-connection session state stored in a single `DashMap`:**
- Current capacity: Unbounded — one entry per TCP connection. Metadata per session includes `tx_buffer` (buffered write tasks), `read_set` (conflict-detection tuples), and `cursors` (pre-fetched rows). All stored in process memory.
- Limit: At high connection counts with long-lived open transactions or large cursors, memory pressure from session state can grow proportionally with both connection count and cursor result size.
- Scaling path: Implement connection-level limits and cursor row caps (partially in place via `cursor_spill.rs`); enforce `max_connections_per_user` at the session store level.

**`CursorState.rows` holds full result set as `Vec<String>` (JSON):**
- Current capacity: No hard cap per cursor beyond `cursor_spill.rs` enforcement.
- Limit: A single cursor on a large collection (millions of rows) will allocate proportionally large heap memory in the pgwire session for every open connection.
- Scaling path: Stream cursor results to disk via the spill mechanism; `cursor_spill.rs` provides the structure but the actual disk-spill implementation needs to be confirmed complete.

---

## Dependencies at Risk

**`pgwire` crate version lock:**
- Risk: The server exposes a full `ExtendedQueryHandler` surface implemented against a specific pgwire crate version. The handler's `max_rows` parameter is silently ignored (see Known Bugs), which means any upstream pgwire version that adds portal-suspension logic would require coordinated changes to `execute_prepared`.
- Impact: Upgrading the pgwire crate version without implementing `max_rows` and `PortalSuspend` could introduce subtle protocol regressions for compliant clients.
- Migration plan: Implement portal suspension before upgrading the pgwire dependency.

**`libc` used directly for eventfd, poll, close syscalls:**
- Risk: `nodedb/src/data/eventfd.rs` and `nodedb-bridge/src/eventfd.rs` use raw `libc` syscalls without the safety abstractions provided by `nix` or `rustix`. RawFd-based ownership is managed manually with a hand-written `Drop` impl. On Linux this works reliably; on other platforms the code does not compile without guards.
- Impact: If the fd is double-freed (e.g. if `EventFdNotifier` outlives the `EventFd` that owns the fd and `EventFd::drop` closes it while a notifier still holds a copy of the fd number), the notifier would write to a reused fd — a silent data corruption.
- Migration plan: Wrap in `OwnedFd` (already done in `nodedb-bridge` version) or use `rustix` for all eventfd operations.

---

## Missing Critical Features (Protocol Completeness for External Clients)

**`pg_catalog` coverage is minimal — missing tables required by most drivers:**
- Problem: Only 7 `pg_catalog` tables are virtualised: `pg_database`, `pg_namespace`, `pg_type`, `pg_class`, `pg_attribute`, `pg_index`, `pg_authid`. Common drivers and IDEs also query:
  - `pg_proc` — function/procedure listing (used by SQLAlchemy, Django ORM introspection)
  - `pg_constraint` — constraint enumeration (used by ORMs to map FKs)
  - `pg_stat_activity` — active queries (used by pgAdmin, DBeaver, monitoring tools)
  - `pg_settings` / `current_setting()` — runtime parameter introspection
  - `information_schema.tables`, `information_schema.columns` — standard SQL metadata
  - `pg_roles` / `pg_user` — user listing
  - `pg_sequences` — sequence metadata
- Files: `nodedb/src/control/server/pgwire/pg_catalog/dispatch.rs:191-207`
- Impact for DB Studio: A VS Code extension using a standard PostgreSQL driver (e.g. `postgres.js`, `pg`, node-postgres) will fail to enumerate collections/tables, show column types, or introspect indexes if it uses the standard pg_catalog / information_schema queries. Connection will succeed but schema panels will be empty or error.
- Fix approach: Add stub virtual tables for `pg_proc` (empty), `pg_constraint` (empty or mapped to collection indexes), `pg_stat_activity` (current-connection row), `pg_settings` (map from `KNOWN_PG_RUNTIME_PARAMETERS`), and `information_schema.tables`/`columns` as aliases over `pg_class`/`pg_attribute`.

**Binary encoding not supported for extended-query result rows:**
- Problem: All pgwire result rows are encoded in text format (`FieldFormat::Text`) regardless of what the client requested in the `Bind` message. PostgreSQL clients that request binary output format (e.g. `tokio-postgres`/`rust-postgres` in binary mode) will receive text-encoded bytes. The driver will attempt to interpret them as binary structs and produce garbage data or panics.
- Files: `nodedb/src/control/server/pgwire/types/field.rs` (all `FieldInfo` builders use `FieldFormat::Text`), `nodedb/src/control/server/pgwire/handler/prepared/execute.rs:181-225`
- Impact for DB Studio: Any driver that negotiates binary format on the Bind message will misread numeric, timestamp, UUID, and bytea columns. The Rust `tokio-postgres` crate defaults to binary format for these types.
- Fix approach: Implement binary encoders for at least INT2/INT4/INT8, FLOAT4/FLOAT8, BOOL, BYTEA, UUID, TIMESTAMP, TIMESTAMPTZ so drivers that request binary format receive correct data.

**`max_rows` portal pagination not implemented (see Known Bugs):**
- Impact for DB Studio: GUI tools that page through large result sets (DBeaver, DataGrip, TablePlus) use the extended-query `Execute` message with `max_rows > 0` to fetch N rows at a time. Without portal suspension, the server returns all rows in a single Execute response, potentially sending megabytes to the client for a simple table browse.
- Files: `nodedb/src/control/server/pgwire/handler/prepared/execute.rs:32`

**`ULID` type shares UUID OID (2950) — ambiguous to clients:**
- Problem: Both `ColumnType::Uuid` and `ColumnType::Ulid` map to PostgreSQL OID `2950` (uuid). A client that receives an OID-2950 column has no way to distinguish a UUID from a ULID. ULIDs are sortable and have a different textual representation (`01ARZ3NDEKTSV4RRFFQ69G5FAV`) which UUID parsers will reject.
- Files: `nodedb-types/src/columnar/column_type.rs:131`
- Impact for DB Studio: Schema panels will display ULID columns as `uuid` type; data cells showing ULID values will fail UUID format validation in the UI.
- Fix approach: Use a custom extension OID range for ULID (similar to how pgvector uses a custom OID), or surface the type name via the `pg_type` virtual table and document the workaround.

**`Geometry` type collapses to TEXT OID (25):**
- Problem: `ColumnType::Geometry` maps to OID `25` (text). Spatial data is returned as WKT/WKB strings. Clients expecting a geometry type (QGIS, PostGIS-aware tools) will not recognise the column as spatial.
- Files: `nodedb-types/src/columnar/column_type.rs:135`
- Impact for DB Studio: Spatial columns display as plain text; no map/geometry visualisations.
- Fix approach: Register a custom OID in the `pg_type` virtual table for `geometry` matching PostGIS's convention (OID `16384` range) so compatible clients recognise the type.

**`Vector(_)` type maps to `FLOAT4_ARRAY` OID (1021) — incompatible with pgvector:**
- Problem: `ColumnType::Vector(n)` maps to PostgreSQL `FLOAT4_ARRAY` OID `1021`. pgvector uses a custom OID; drivers that only recognise pgvector's custom OID (e.g. `pgvector-python`, `pgvector-node`) will not treat the column as a vector type.
- Files: `nodedb-types/src/columnar/column_type.rs:137`
- Impact for DB Studio: Vector columns show as `float4[]`; no vector-specific UI affordances (e.g. nearest-neighbour query builder) can activate.
- Fix approach: Register a custom `vector` OID in `pg_type` and document that NodeDB's vector OID differs from pgvector's. Provide a mapping guide.

**`Array`, `Set`, `Range`, `Record`, `Regex` types all collapse to JSONB OID (3802):**
- Problem: Five structurally different types all share OID `3802`. Clients cannot distinguish them from JSON. The underlying wire format is MessagePack, not JSON, so clients that attempt to parse the data as JSON will fail unless NodeDB has already decoded and re-encoded to JSON in the text layer.
- Files: `nodedb-types/src/columnar/column_type.rs:140`
- Impact for DB Studio: Schema panels show all five types as `jsonb`; no type-specific UI.
- Fix approach: Verify that result rows actually encode these types as valid JSON text (not raw MessagePack bytes) when returned over pgwire text format. If they are already JSON-encoded, the concern is purely visual. If they are raw MessagePack, this is a data corruption risk for external clients.

---

## Test Coverage Gaps

**No test for `max_rows` / `PortalSuspend` behaviour:**
- What's not tested: Whether clients that send `Execute` with `max_rows > 0` receive a `PortalSuspended` message or all rows at once.
- Files: `nodedb/src/control/server/pgwire/handler/prepared/execute.rs:32`, `nodedb/tests/pgwire_extended_query.rs`
- Risk: Drivers relying on portal pagination silently receive all rows; no regression test would catch a future accidental fix breaking the current all-rows behaviour.
- Priority: High (affects all pg drivers using fetch size)

**No test for binary parameter format on non-rejected types:**
- What's not tested: DATE, TIME, UUID, INTERVAL, BYTEA parameters sent in binary format to the extended-query Execute path. The existing tests cover only the explicitly-rejected types (NUMERIC, TIMESTAMP, TIMESTAMPTZ) and text-format passthrough.
- Files: `nodedb/src/control/server/pgwire/handler/prepared/execute.rs:340-534`
- Risk: Silent corruption when a driver sends binary-encoded date/time/uuid values.
- Priority: High (affects tokio-postgres and any binary-mode driver)

**No integration test for `information_schema` or missing `pg_catalog` tables:**
- What's not tested: Queries against `pg_proc`, `pg_constraint`, `pg_stat_activity`, `pg_settings`, `information_schema.tables`, `information_schema.columns`.
- Files: `nodedb/tests/pg_catalog_select_semantics.rs`
- Risk: Every psql `\d` command and ORM schema-inspection query will fail silently (routed to the normal planner and failing with collection-not-found) with no test regression alarm.
- Priority: High (blocks all standard pg tooling including the planned DB Studio)

**No test for LISTEN/NOTIFY notification delivery timing:**
- What's not tested: Whether notifications queued during a query are delivered to idle connections before the next ReadyForQuery, or whether they are only delivered on the connection's next request cycle.
- Files: `nodedb/src/control/server/pgwire/handler/listen_notify.rs`, `nodedb/src/control/server/pgwire/handler/core.rs:401`
- Risk: Clients using `LISTEN` for real-time change feeds (a primary DB Studio use case) may miss notifications or receive them out-of-order.
- Priority: Medium

**No test for `server_version_num` correctness across client libraries:**
- What's not tested: Whether `SHOW server_version_num` returns a value that client libraries can parse as a PostgreSQL version integer (e.g. libpq's `PQserverVersion`, JDBC's `getServerVersion`). The new `version.rs` module returns `"160000"` which is correct for PG 16.0, but there is no test that a real driver parses this and enables/disables expected features.
- Files: `nodedb/tests/wire_server_version.rs`, `nodedb/src/control/server/pgwire/version.rs` (branch-only)
- Risk: Drivers that gate features on `server_version_num < 90000` (e.g. legacy JDBC logic) may misbehave.
- Priority: Low

**Cluster tests have no pgwire extended-query coverage:**
- What's not tested: Prepared statements and extended-query protocol across the gateway migration path in cluster mode.
- Files: `nodedb-cluster-tests/tests/pgwire_gateway_migration.rs`
- Risk: Extended-query session state (prepared statements, open portals) may not survive gateway failover.
- Priority: Medium

---

## Platform-Gating Concerns

**`EventFd` is a Linux-only primitive with no macOS equivalent implemented:**
- The `eventfd` syscall used in both `nodedb/src/data/eventfd.rs` and `nodedb-bridge/src/eventfd.rs` is Linux-specific. The files use `std::os::unix::io` but the actual `libc::eventfd` call is not guarded by `#[cfg(target_os = "linux")]`. The code will fail to compile on macOS unless the `libc` crate provides a stub — or it will compile and silently return `-ENOSYS` at runtime.
- Files: `nodedb/src/data/eventfd.rs:24`, `nodedb-bridge/src/eventfd.rs:38`
- Impact: Development on macOS (common for contributors and the DB Studio developer workflow) requires either cross-compilation or a Linux VM. The recent commit `fix(runtime): add self-pipe EventFd fallback for non-Linux core wake` addresses this but is gated on the branch — verify the fallback actually compiles on macOS in CI.

**Systemd readiness (`sd_notify`) is correctly gated but is a no-op on macOS:**
- The `notify_ready` / `notify_status` / `notify_stopping` functions in `nodedb-cluster/src/readiness.rs` are correctly gated with `#[cfg(target_os = "linux")]` / `#[cfg(not(target_os = "linux"))]`. Non-Linux targets silently no-op.
- Files: `nodedb-cluster/src/readiness.rs`
- Impact: Non-issue for correctness; `Type=notify` systemd unit files will never receive the READY signal on macOS. Documented.

**Keystore file-permission checks are Unix-only:**
- File permission enforcement (mode `0600` checks, `chown`, etc.) in the keystore and TLS modules is guarded by `#[cfg(unix)]`. Windows builds receive no permission enforcement.
- Files: `nodedb/src/control/security/keystore/key_file_security.rs`, `nodedb/src/control/security/keystore/file.rs`, `nodedb/src/control/cluster/tls.rs`
- Impact: Windows deployments (if ever targeted) would have no key-file security enforcement. Current Linux/macOS production targets are not affected.

---

*Concerns audit: 2026-06-13*
