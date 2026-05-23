// SPDX-License-Identifier: BUSL-1.1

//! Dispatcher correctness for administrative `SHOW` commands over pgwire.
//!
//! The pgwire entry point intercepts every statement starting with `SHOW `
//! and, unless the command is on a hard-coded allowlist, routes it to the
//! PostgreSQL session-parameter fallback. That fallback returns a single
//! row with one column named after the parameter and an empty string as
//! its value — making an unrouted administrative command look like a
//! successful but empty result instead of erroring or reaching its real
//! handler.
//!
//! The tests below assert each administrative `SHOW` reaches its handler
//! by checking the schema and row count of the response. Each test
//! includes a regression guard against the session-parameter fallback
//! signature: a single-column response whose column name equals the
//! lowercased parameter tail (e.g. `databases`, `roles`, `stats`) with
//! an empty value.

mod common;

use common::pgwire_harness::TestServer;

/// Returns true if the response is the pgwire session-parameter fallback:
/// exactly one row, exactly one column, column name equal to `param`
/// (case-insensitive), and an empty value.
async fn is_session_param_fallback(server: &TestServer, sql: &str, param: &str) -> bool {
    let rows = match server.query_named_rows(sql).await {
        Ok(r) => r,
        Err(_) => return false,
    };
    if rows.len() != 1 {
        return false;
    }
    let row = &rows[0];
    if row.len() != 1 {
        return false;
    }
    row.iter()
        .any(|(k, v)| k.eq_ignore_ascii_case(param) && v.is_empty())
}

// ── SHOW DATABASES ───────────────────────────────────────────────────

/// After `CREATE DATABASE`, `SHOW DATABASES` must return at least two rows
/// (default + new) and expose the per-database schema (`name`, `status`,
/// `collection_count`, ...) — never the single-column session-parameter
/// fallback.
#[tokio::test]
async fn show_databases_lists_created_database() {
    let server = TestServer::start().await;
    server
        .exec("CREATE DATABASE show_dispatch_alpha")
        .await
        .expect("CREATE DATABASE must succeed");

    let rows = server
        .query_named_rows("SHOW DATABASES")
        .await
        .expect("SHOW DATABASES must not error");

    assert!(
        rows.len() >= 2,
        "SHOW DATABASES must list both `default` and the created database; got {} row(s): {:?}",
        rows.len(),
        rows
    );
    assert!(
        rows[0].contains_key("name"),
        "SHOW DATABASES must expose a `name` column (got columns: {:?})",
        rows[0].keys().collect::<Vec<_>>()
    );
    assert!(
        rows.iter().any(|r| r
            .get("name")
            .map(|n| n == "show_dispatch_alpha")
            .unwrap_or(false)),
        "SHOW DATABASES must include the row created by CREATE DATABASE: {rows:?}"
    );

    // Regression guard: single-column response named "databases" with an
    // empty value is the session-parameter fallback signature.
    assert!(
        !is_session_param_fallback(&server, "SHOW DATABASES", "databases").await,
        "SHOW DATABASES must not be routed to the session-parameter fallback"
    );
}

// ── SHOW ROLES ───────────────────────────────────────────────────────

/// `SHOW ROLES` after `CREATE ROLE` must list the new role with a typed
/// schema, not the single-column session-parameter fallback.
#[tokio::test]
async fn show_roles_lists_created_role() {
    let server = TestServer::start().await;
    server
        .exec("CREATE ROLE show_dispatch_role")
        .await
        .expect("CREATE ROLE must succeed");

    let rows = server
        .query_named_rows("SHOW ROLES")
        .await
        .expect("SHOW ROLES must not error");

    assert!(
        !rows.is_empty(),
        "SHOW ROLES must return at least one row; got empty result"
    );
    assert!(
        rows[0].len() >= 2 || !rows[0].contains_key("roles"),
        "SHOW ROLES must use a typed multi-column schema, not the single \
         `roles` column session-parameter fallback (got columns: {:?})",
        rows[0].keys().collect::<Vec<_>>()
    );
    assert!(
        rows.iter()
            .any(|r| r.values().any(|v| v == "show_dispatch_role")),
        "SHOW ROLES must include the created role: {rows:?}"
    );

    assert!(
        !is_session_param_fallback(&server, "SHOW ROLES", "roles").await,
        "SHOW ROLES must not be routed to the session-parameter fallback"
    );
}

// ── SHOW STATS / SHOW SERVER STATS / SHOW METRICS / SHOW MEMORY ──────

/// `SHOW STATS` must reach a real handler. The session-parameter fallback
/// is a silent misroute — it returns a single-column row named `stats`
/// with an empty string, which is indistinguishable from a working
/// "no stats collected" response unless explicitly guarded.
#[tokio::test]
async fn show_stats_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW STATS", "stats").await,
        "SHOW STATS must not be routed to the session-parameter fallback \
         (single-column `stats` with empty value)"
    );
}

#[tokio::test]
async fn show_server_stats_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    // The fallback tokenises by the param tail; `SHOW SERVER STATS`
    // becomes parameter `server stats`.
    assert!(
        !is_session_param_fallback(&server, "SHOW SERVER STATS", "server stats").await,
        "SHOW SERVER STATS must not be routed to the session-parameter fallback"
    );
}

#[tokio::test]
async fn show_metrics_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW METRICS", "metrics").await,
        "SHOW METRICS must not be routed to the session-parameter fallback"
    );
}

#[tokio::test]
async fn show_memory_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW MEMORY", "memory").await,
        "SHOW MEMORY must not be routed to the session-parameter fallback"
    );
}

// ── Evidence: the dispatch flaw is systemic, not specific to the issue
//    list. Each of these is dispatched in `ddl/router/admin.rs` but is
//    unreachable because the session-parameter fallback intercepts every
//    `SHOW ` prefix that isn't on the allowlist in `handler/sql_exec.rs`.

/// `SHOW SCHEDULES` is wired in `admin.rs` and must reach that handler.
#[tokio::test]
async fn show_schedules_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW SCHEDULES", "schedules").await,
        "SHOW SCHEDULES must reach its admin-router handler, not the \
         session-parameter fallback"
    );
}

/// `SHOW SEQUENCES` is wired in `admin.rs` and must reach that handler.
#[tokio::test]
async fn show_sequences_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW SEQUENCES", "sequences").await,
        "SHOW SEQUENCES must reach its admin-router handler, not the \
         session-parameter fallback"
    );
}

/// `SHOW ALERTS` is wired in `admin.rs` and must reach that handler.
#[tokio::test]
async fn show_alerts_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW ALERTS", "alerts").await,
        "SHOW ALERTS must reach its admin-router handler, not the \
         session-parameter fallback"
    );
}

/// `SHOW MATERIALIZED VIEWS` is wired in `admin.rs` and must reach
/// that handler.
#[tokio::test]
async fn show_materialized_views_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW MATERIALIZED VIEWS", "materialized views",).await,
        "SHOW MATERIALIZED VIEWS must reach its admin-router handler, \
         not the session-parameter fallback"
    );
}

/// `SHOW BLACKLIST` is wired in `admin.rs` and must reach that handler.
#[tokio::test]
async fn show_blacklist_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW BLACKLIST", "blacklist").await,
        "SHOW BLACKLIST must reach its admin-router handler, not the \
         session-parameter fallback"
    );
}

/// `SHOW ORGS` is wired in `admin.rs` and must reach that handler.
#[tokio::test]
async fn show_orgs_is_not_session_param_fallback() {
    let server = TestServer::start().await;
    assert!(
        !is_session_param_fallback(&server, "SHOW ORGS", "orgs").await,
        "SHOW ORGS must reach its admin-router handler, not the \
         session-parameter fallback"
    );
}

// ── Strict PG-runtime-parameter handler ──────────────────────────────

/// `SHOW <unknown-name>` for a name that is not a known PostgreSQL
/// runtime parameter, not set in the session, and not claimed by any
/// administrative router must return `42704`
/// (`unrecognized configuration parameter`) — never a silent empty row.
/// This guard prevents the original ghost-row bug from regressing for
/// arbitrary new SHOW commands added in the future.
#[tokio::test]
async fn show_unknown_parameter_returns_42704() {
    let server = TestServer::start().await;
    match server
        .client
        .simple_query("SHOW totally_made_up_parameter_xyz")
        .await
    {
        Ok(msgs) => panic!(
            "SHOW on an unknown parameter must error with 42704, got success: \
             {msgs:?}"
        ),
        Err(e) => {
            let code = e.code().map(|c| c.code().to_string()).unwrap_or_default();
            let msg = e
                .as_db_error()
                .map(|d| d.message().to_string())
                .unwrap_or_else(|| e.to_string());
            assert!(
                code == "42704" || msg.contains("unrecognized configuration"),
                "expected 42704 / unrecognized configuration parameter, got \
                 code={code:?} msg={msg:?}"
            );
        }
    }
}

/// Built-in PostgreSQL runtime parameters keep working: clients depend
/// on `SHOW server_version`, `SHOW server_encoding`, and `SHOW ALL` to
/// negotiate driver behaviour at startup.
#[tokio::test]
async fn show_builtin_pg_runtime_parameters_still_work() {
    let server = TestServer::start().await;

    let version = server
        .query_text("SHOW server_version")
        .await
        .expect("SHOW server_version must succeed");
    assert_eq!(version.len(), 1, "SHOW server_version must return one row");
    assert!(
        version[0].contains("NodeDB"),
        "SHOW server_version must report a NodeDB version string, got: {:?}",
        version[0]
    );

    let encoding = server
        .query_text("SHOW server_encoding")
        .await
        .expect("SHOW server_encoding must succeed");
    assert_eq!(encoding, vec!["UTF8".to_string()]);

    let all = server
        .query_rows("SHOW ALL")
        .await
        .expect("SHOW ALL must succeed");
    // SHOW ALL returns the session parameter map — may be empty on a
    // fresh connection. The assertion is that it does not error.
    let _ = all;
}

/// Values explicitly set via `SET <name>` in the current session must
/// remain readable via `SHOW <name>`, even if `<name>` is not on the
/// built-in PG-runtime-parameter allowlist. This preserves the
/// `SET foo = 'bar'; SHOW foo;` round-trip used by some clients for
/// session-scoped configuration.
#[tokio::test]
async fn show_session_set_parameter_round_trips() {
    let server = TestServer::start().await;
    server
        .exec("SET application_name = 'mae8_bootstrap'")
        .await
        .expect("SET application_name must succeed");
    let rows = server
        .query_text("SHOW application_name")
        .await
        .expect("SHOW application_name must succeed");
    assert_eq!(rows, vec!["mae8_bootstrap".to_string()]);
}
