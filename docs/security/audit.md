# Audit Log & Change Tracking

NodeDB maintains a tamper-evident, hash-chained audit log. Every entry includes a SHA-256 hash of the previous entry — if any record is modified or deleted, the chain breaks.

## Viewing the Audit Log

```sql
-- Show recent entries (superuser only)
SHOW AUDIT LOG;
SHOW AUDIT LOG LIMIT 50;
```

**Columns:** `seq`, `timestamp`, `event`, `tenant_id`, `source`, `detail`

Each entry also records `auth_user_id`, `auth_user_name`, and `session_id` for correlation.

## Audit Events

| Event                        | Level    | Description                                    |
| ---------------------------- | -------- | ---------------------------------------------- |
| `AuthSuccess`                | Minimal  | Successful authentication                      |
| `AuthFailure`                | Minimal  | Failed login attempt                           |
| `PermissionDenied`           | Minimal  | Permission check failed                        |
| `AuthzDenied`                | Minimal  | Legacy: permission denied                      |
| `PrivilegeChange`            | Standard | GRANT, REVOKE, role changes                    |
| `SessionConnect`             | Standard | New connection                                 |
| `SessionDisconnect`          | Standard | Connection closed                              |
| `SessionRevoked`             | Standard | Admin terminated session                       |
| `LockoutTriggered`           | Standard | Account locked after failed logins             |
| `LoginRateLimited`           | Standard | Too many login attempts                        |
| `RlsRejected`                | Standard | RLS policy blocked row                         |
| `AdminAction`                | Standard | DDL, config changes                            |
| `TenantCreated`              | Standard | New tenant                                     |
| `TenantDeleted`              | Standard | Tenant removed                                 |
| `DatabaseCreated`            | Standard | New database                                   |
| `DatabaseDropped`            | Standard | Database deleted                               |
| `DatabaseRenamed`            | Standard | Database renamed                               |
| `DatabaseQuotaChanged`       | Standard | Quota limit changed                            |
| `DatabaseCloned`             | Standard | Database cloned                                |
| `DatabaseMirrored`           | Standard | Database mirrored                              |
| `DatabasePromoted`           | Standard | Replica promoted to primary                    |
| `DatabaseMaterialized`       | Standard | Materialized view materialized                 |
| `TenantMoved`                | Standard | Tenant reassigned to database                  |
| `DatabaseBackedUp`           | Standard | Backup created                                 |
| `DatabaseRestored`           | Standard | Backup restored                                |
| `DatabaseAuditDmlChanged`    | Standard | DML audit setting changed                      |
| `DatabaseIdleTimeoutChanged` | Standard | Idle timeout changed                           |
| `OidcProviderChanged`        | Standard | OIDC provider created/altered/dropped          |
| `DmlAudit`                   | Forensic | Individual DML operation (opt-in per database) |
| `SnapshotBegin/End`          | Standard | Backup lifecycle                               |
| `RestoreBegin/End`           | Standard | Restore lifecycle                              |
| `CertRotation`               | Standard | TLS cert rotated                               |
| `KeyRotation`                | Standard | Encryption key rotated                         |
| `NodeJoined/Left`            | Standard | Cluster membership                             |
| `QueryExec`                  | Full     | Every query executed                           |
| `RowChange`                  | Forensic | Individual row mutations                       |

## Audit Levels

```toml
# nodedb.toml
[audit]
level = "standard"
```

| Level      | What's recorded                                |
| ---------- | ---------------------------------------------- |
| `minimal`  | Auth events only (login, failure, denial)      |
| `standard` | + admin actions, DDL, sessions, config changes |
| `full`     | + every query, RLS denials                     |
| `forensic` | + row-level mutations, CRDT deltas             |

Higher levels include everything from lower levels.

## Per-Database Filtering

Filter audit events by database to reduce noise when operating a single database:

```sql
-- Show audit events for a specific database
SHOW AUDIT IN DATABASE prod LIMIT 50;

-- Combine with event type filter
SHOW AUDIT IN DATABASE prod WHERE event_type = 'DmlAudit' LIMIT 100;
```

Database-scoped DDL (CLONE, MIRROR, PROMOTE, etc.) include `database_id` in the audit entry, enabling per-database filtering and forensics.

## Hash Chain Integrity

Every `AuditEntry` contains `prev_hash` — the SHA-256 of the previous entry. This creates a tamper-evident chain verified on startup. If an entry is modified or deleted, the chain breaks and is flagged.

Hash chain is extended with `database_id` when present (new events include this field; legacy events without database scope are preserved for compatibility).

## DML Audit (Opt-In)

Track individual data modifications (INSERT, UPDATE, DELETE) per database. **Disabled by default** — enable only when required for compliance or forensics, as the volume can be substantial.

```sql
-- Enable DML audit for writes only (INSERT, UPDATE, DELETE)
ALTER DATABASE prod SET AUDIT_DML = 'writes';

-- Enable all DML including reads
ALTER DATABASE prod SET AUDIT_DML = 'all';

-- Disable (default)
ALTER DATABASE prod SET AUDIT_DML = 'none';

-- View DML audit entries
SHOW AUDIT IN DATABASE prod WHERE event_type = 'DmlAudit' LIMIT 100;
```

**DML audit entry includes:**

| Field              | Type       | Description                        |
| ------------------ | ---------- | ---------------------------------- |
| `database_id`      | DatabaseId | Scoped to database                 |
| `tenant_id`        | TenantId   | Tenant context                     |
| `user_id`          | u32        | Who performed the operation        |
| `statement_digest` | String     | Hash of the SQL statement          |
| `collection`       | String     | Target collection name             |
| `op`               | String     | Operation (insert, update, delete) |
| `row_id`           | u32        | Surrogate key of affected row      |
| `lsn`              | u64        | Log sequence number (WAL position) |
| `timestamp`        | Timestamp  | When the operation occurred        |

**Storage cost:** Estimate ~1 KB per DML operation. For a production database with 1M writes/day, expect ~1 GB of audit storage per month.

**Implementation:** DML audit events are emitted by the Event Plane (not the request path), so audit fanout does not block writes. Events are buffered in the event bus and persisted asynchronously.

## SIEM Export

Export audit events to external security information and event management systems via CDC webhook:

```sql
CREATE CHANGE STREAM audit_export ON _system.audit
    DELIVERY WEBHOOK 'https://siem.example.com/ingest'
    WITH (format = 'json', hmac_secret = 'your-secret');
```

HMAC signatures allow the receiving system to verify event authenticity.

Database-scoped events include `database_id` field for SIEM correlation.

---

# Document Change Tracking

NodeDB does NOT have automatic `updated_at` / `created_at` columns like PostgreSQL. Here's how to track changes:

## Typeguard DEFAULT + VALUE (Schemaless)

The recommended approach for schemaless collections:

```sql
CREATE TYPEGUARD ON users (
    created_at TIMESTAMP DEFAULT now(),
    updated_at TIMESTAMP VALUE now()
);
```

- `DEFAULT now()` — set once on INSERT, never overwritten
- `VALUE now()` — set on every write (INSERT, UPSERT, UPDATE)

```sql
INSERT INTO users { id: 'u1', name: 'Alice' };
-- created_at = 2026-04-10T..., updated_at = 2026-04-10T...

UPSERT INTO users { id: 'u1', name: 'Alice Updated' };
-- created_at unchanged, updated_at refreshed
```

## Strict Schema DEFAULT

For strict collections:

```sql
CREATE COLLECTION orders (
    id TEXT PRIMARY KEY DEFAULT gen_uuid_v7(),
    customer TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT now()
) WITH (engine='document_strict');

-- created_at auto-filled on INSERT
INSERT INTO orders (customer) VALUES ('Alice');
```

## BEFORE Triggers

For custom logic:

```sql
CREATE TRIGGER set_timestamps BEFORE INSERT ON events FOR EACH ROW
BEGIN
    SET NEW.created_at = now();
    SET NEW.updated_at = now();
END;

CREATE TRIGGER update_timestamp BEFORE UPDATE ON events FOR EACH ROW
BEGIN
    SET NEW.updated_at = now();
END;
```

## CDC Change Streams

For full change history without modifying documents:

```sql
CREATE CHANGE STREAM user_changes ON users;

-- Consume changes (returns old + new values for each mutation)
SELECT * FROM CHANGES('user_changes') LIMIT 10;
```

Change streams capture INSERT, UPDATE, and DELETE events with full before/after payloads. Use consumer groups for durable, at-least-once processing.

[Back to security](README.md)
