// SPDX-License-Identifier: BUSL-1.1

//! Session parameter commands: SET, SHOW, SHOW ALL, EXPLAIN.

use std::sync::Arc;

use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};

use crate::control::security::identity::AuthenticatedIdentity;

use super::super::types::{error_to_sqlstate, sqlstate_error, text_field};
use super::core::NodeDbPgHandler;

/// Outcome of classifying a `SET TRANSACTION` / `SET SESSION CHARACTERISTICS` command.
enum TransactionCmd {
    /// `READ ONLY` — store access mode, return SET.
    SetReadOnly,
    /// `READ WRITE` — store access mode, return SET.
    SetReadWrite,
    /// `ISOLATION LEVEL READ COMMITTED` — silent accept (Snapshot Isolation is strictly stronger).
    AcceptIsolation,
    /// Any unsupported isolation level or unknown option — reject with SQLSTATE 0A000.
    RejectIsolation(String),
}

/// Classify a `SET TRANSACTION` or `SET SESSION CHARACTERISTICS` SQL statement.
///
/// `upper` must be `sql.to_uppercase()`. `sql` is the original, used for error messages.
fn classify_transaction_cmd(upper: &str, sql: &str) -> TransactionCmd {
    // Isolation-level branch: check before READ ONLY/READ WRITE so that a statement
    // like "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED" does not accidentally
    // match the READ-only access-mode branch.
    if upper.contains("ISOLATION LEVEL") {
        // READ COMMITTED: silent accept.
        if upper.contains("READ COMMITTED") {
            return TransactionCmd::AcceptIsolation;
        }

        let level = if upper.contains("SERIALIZABLE") {
            Some("SERIALIZABLE")
        } else if upper.contains("REPEATABLE READ") {
            Some("REPEATABLE READ")
        } else if upper.contains("READ UNCOMMITTED") {
            Some("READ UNCOMMITTED")
        } else {
            None
        };

        let message = match level {
            Some(lvl) => format!(
                "SET TRANSACTION ISOLATION LEVEL {lvl} is not supported; \
                 NodeDB enforces Snapshot Isolation"
            ),
            None => format!(
                "unsupported SET TRANSACTION option: {}",
                sql.split_whitespace().skip(2).collect::<Vec<_>>().join(" ")
            ),
        };
        return TransactionCmd::RejectIsolation(message);
    }

    // Access-mode branch.
    if upper.contains("READ ONLY") {
        return TransactionCmd::SetReadOnly;
    }
    if upper.contains("READ WRITE") {
        return TransactionCmd::SetReadWrite;
    }

    // Unknown option.
    TransactionCmd::RejectIsolation(format!(
        "unsupported SET TRANSACTION option: {}",
        sql.split_whitespace().skip(2).collect::<Vec<_>>().join(" ")
    ))
}

impl NodeDbPgHandler {
    /// Handle SET commands: parse, validate, store in session.
    pub(super) fn handle_set(
        &self,
        identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
        sql: &str,
    ) -> PgWireResult<Vec<Response>> {
        use super::super::session::parse_set_command;
        use pgwire::api::results::Tag;

        // Handle SET TRANSACTION ... and SET SESSION CHARACTERISTICS AS TRANSACTION ...
        let upper = sql.to_uppercase();
        if upper.starts_with("SET TRANSACTION") || upper.starts_with("SET SESSION CHARACTERISTICS")
        {
            match classify_transaction_cmd(&upper, sql) {
                TransactionCmd::SetReadOnly => {
                    self.sessions.set_parameter(
                        addr,
                        "transaction_access_mode".into(),
                        "read_only".into(),
                    );
                    return Ok(vec![Response::Execution(Tag::new("SET"))]);
                }
                TransactionCmd::SetReadWrite => {
                    self.sessions.set_parameter(
                        addr,
                        "transaction_access_mode".into(),
                        "read_write".into(),
                    );
                    return Ok(vec![Response::Execution(Tag::new("SET"))]);
                }
                TransactionCmd::AcceptIsolation => {
                    return Ok(vec![Response::Execution(Tag::new("SET"))]);
                }
                TransactionCmd::RejectIsolation(message) => {
                    return Err(sqlstate_error(
                        nodedb_types::error::sqlstate::FEATURE_NOT_SUPPORTED,
                        &message,
                    ));
                }
            }
        }

        // `SET ROLE <name>` and `SET SESSION AUTHORIZATION '<name>'` use
        // PostgreSQL's space-not-equals syntax, so `parse_set_command` (which
        // splits on `=` / `TO`) returns `None` for them. Catch the keywords
        // before falling through to that parser — both must reject explicitly
        // rather than land on the silent success path (the root cause behind
        // SET TENANT looking like a no-op).
        if upper.starts_with("SET ROLE ") || upper == "SET ROLE" {
            return Err(sqlstate_error(
                nodedb_types::error::sqlstate::FEATURE_NOT_SUPPORTED,
                "SET ROLE is not supported: a session's role set is identity-bound \
                 at CREATE USER time. Use GRANT/REVOKE ROLE TO <user> to change \
                 a user's roles, or reconnect with a different user.",
            ));
        }
        if upper.starts_with("SET SESSION AUTHORIZATION") {
            return Err(sqlstate_error(
                nodedb_types::error::sqlstate::FEATURE_NOT_SUPPORTED,
                "SET SESSION AUTHORIZATION is not supported: identity is bound at \
                 connection time. Reconnect as the target user.",
            ));
        }

        let (key, value) = match parse_set_command(sql) {
            Some(kv) => kv,
            None => {
                // Statements that look like `SET <something>` but don't match
                // any of the recognized shapes (k=v, k TO v, TRANSACTION,
                // ROLE, SESSION AUTHORIZATION) must NOT silently succeed.
                // Silent success on unparsed SET is exactly the bug class
                // that allowed `SET TENANT = 'x'` to look like a no-op.
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".to_owned(),
                    "42601".to_owned(),
                    format!("syntax error in SET command: {sql}"),
                ))));
            }
        };

        // Identity / security context keys are dispatched before the
        // generic store-in-session path. Storing them in the parameter bag
        // without an enforcement contract is the silent-no-op class — every
        // such key must either be honored end-to-end or rejected explicitly.
        match key.as_str() {
            "tenant" => {
                return self.handle_set_tenant_name_or_id(identity, addr, &value);
            }
            "nodedb.tenant_id" => {
                return self.handle_set_tenant_by_id(identity, addr, &value);
            }
            "role" => {
                return Err(sqlstate_error(
                    nodedb_types::error::sqlstate::FEATURE_NOT_SUPPORTED,
                    "SET ROLE is not supported: a session's role set is identity-bound \
                     at CREATE USER time. Use GRANT/REVOKE ROLE TO <user> to change \
                     a user's roles, or reconnect with a different user.",
                ));
            }
            "session_authorization" => {
                return Err(sqlstate_error(
                    nodedb_types::error::sqlstate::FEATURE_NOT_SUPPORTED,
                    "SET SESSION AUTHORIZATION is not supported: identity is bound at \
                     connection time. Reconnect as the target user.",
                ));
            }
            _ => {}
        }

        if key == "nodedb.consistency" {
            match value.as_str() {
                "strong" | "eventual" => {}
                s if s.starts_with("bounded_staleness") => {}
                _ => {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".to_owned(),
                        "22023".to_owned(),
                        format!(
                            "invalid value for nodedb.consistency: '{value}'. Valid: strong, bounded_staleness(<ms>), eventual"
                        ),
                    ))));
                }
            }
        }

        if key == super::super::session::read_consistency::PARAM_KEY
            && super::super::session::read_consistency::parse_value(&value).is_none()
        {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "22023".to_owned(),
                format!(
                    "invalid value for {}: '{value}'. Valid: strong, bounded_staleness:<secs>, eventual",
                    super::super::session::read_consistency::PARAM_KEY
                ),
            ))));
        }

        if key == super::super::session::cross_shard_mode::PARAM_KEY
            && super::super::session::cross_shard_mode::parse_value(&value).is_none()
        {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "22023".to_owned(),
                format!(
                    "invalid value for {}: '{value}'. Valid values: 'strict', 'best_effort_non_atomic'",
                    super::super::session::cross_shard_mode::PARAM_KEY
                ),
            ))));
        }

        // Eager validation for `nodedb.auth_session`: drive the resolve path
        // now so rate-limit / audit / fingerprint checks fire on each SET
        // rather than being deferred to the next query. A probing client
        // that hammers SET LOCAL with bogus handles and never runs a query
        // must still be throttled and observed.
        if key == "nodedb.auth_session" {
            use crate::control::security::session_handle::{ClientFingerprint, ResolveOutcome};
            let caller_fp = ClientFingerprint::from_peer(identity.tenant_id, addr);
            let conn_key = addr.to_string();
            match self
                .state
                .session_handles
                .resolve(&value, &conn_key, &caller_fp)
            {
                ResolveOutcome::RateLimited => {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "FATAL".to_owned(),
                        "53300".to_owned(),
                        "session handle resolve rate limit exceeded on this \
                         connection — closing"
                            .to_owned(),
                    ))));
                }
                ResolveOutcome::Resolved(_) | ResolveOutcome::Miss => {
                    // Store the raw value either way — Miss might be a
                    // handle that was valid previously and expired; the
                    // next query's resolve will fall back to base identity.
                }
            }
        }

        // Any key that reaches this point must be a known runtime parameter.
        // Mirroring the `SHOW` side (params.rs `is_known_pg_runtime_parameter`),
        // unknown keys return `42704 undefined_object` instead of being
        // silently stored — silent storage is the class of bug that allowed
        // `SET TENANT` to look successful while routing nothing.
        if !super::super::session::is_known_settable_runtime_parameter(&key) {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "42704".to_owned(),
                format!("unrecognized configuration parameter \"{key}\""),
            ))));
        }

        self.sessions.set_parameter(addr, key, value);
        Ok(vec![Response::Execution(Tag::new("SET"))])
    }

    /// Apply (or clear) a session-level tenant override after policy checks.
    ///
    /// Common path for `SET TENANT = '<name>' | <id> | DEFAULT` and
    /// `SET nodedb.tenant_id = <id>`. Caller must pass the resolved tenant
    /// (or `None` for the DEFAULT / reset path).
    fn apply_tenant_override(
        &self,
        identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
        new_tenant: Option<crate::types::TenantId>,
        source: &str,
    ) -> PgWireResult<Vec<Response>> {
        use crate::control::security::audit::AuditEvent;
        use pgwire::api::results::Tag;

        if !identity.is_superuser {
            return Err(sqlstate_error(
                "42501",
                "only superuser may change session tenant; a regular user's \
                 tenant is identity-bound at CREATE USER time",
            ));
        }
        if self.sessions.transaction_state(addr) != super::super::session::TransactionState::Idle {
            return Err(sqlstate_error(
                "25001",
                "cannot change session tenant inside an active transaction \
                 (COMMIT or ROLLBACK first)",
            ));
        }

        let prior = self.sessions.get_effective_tenant_id(addr);
        self.sessions.set_effective_tenant_id(addr, new_tenant);

        let detail = match new_tenant {
            Some(t) => format!(
                "{source}: tenant switched from {} to {}",
                prior.unwrap_or(identity.tenant_id),
                t
            ),
            None => format!(
                "{source}: tenant reset to identity-bound {}",
                identity.tenant_id
            ),
        };
        self.state.audit_record(
            AuditEvent::PrivilegeChange,
            Some(identity.tenant_id),
            &identity.username,
            &detail,
        );

        Ok(vec![Response::Execution(Tag::new("SET"))])
    }

    /// Handle `SET TENANT = '<name>' | <id> | DEFAULT`.
    pub(super) fn handle_set_tenant_name_or_id(
        &self,
        identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
        value: &str,
    ) -> PgWireResult<Vec<Response>> {
        if value.eq_ignore_ascii_case("default") {
            return self.apply_tenant_override(identity, addr, None, "SET TENANT = DEFAULT");
        }
        let resolved = if let Ok(id) = value.parse::<u64>() {
            crate::types::TenantId::new(id)
        } else {
            let catalog =
                self.state.credentials.catalog().as_ref().ok_or_else(|| {
                    sqlstate_error("42704", &format!("tenant '{value}' not found"))
                })?;
            let stored = catalog
                .find_tenant_by_name(value)
                .map_err(|e| sqlstate_error("XX000", &format!("catalog read: {e}")))?
                .ok_or_else(|| sqlstate_error("42704", &format!("tenant '{value}' not found")))?;
            crate::types::TenantId::new(stored.tenant_id)
        };
        self.apply_tenant_override(identity, addr, Some(resolved), "SET TENANT")
    }

    /// Handle `SET nodedb.tenant_id = <id> | DEFAULT`.
    pub(super) fn handle_set_tenant_by_id(
        &self,
        identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
        value: &str,
    ) -> PgWireResult<Vec<Response>> {
        if value.eq_ignore_ascii_case("default") {
            return self.apply_tenant_override(
                identity,
                addr,
                None,
                "SET nodedb.tenant_id = DEFAULT",
            );
        }
        let id: u64 = value.parse().map_err(|_| {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "22023".to_owned(),
                format!("invalid value for nodedb.tenant_id: '{value}'. Must be an integer."),
            )))
        })?;
        self.apply_tenant_override(
            identity,
            addr,
            Some(crate::types::TenantId::new(id)),
            "SET nodedb.tenant_id",
        )
    }

    /// Reset the session's tenant override back to the identity-bound tenant.
    /// Backs the `RESET TENANT` statement.
    pub(crate) fn handle_reset_tenant(
        &self,
        identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
    ) -> PgWireResult<Vec<Response>> {
        use pgwire::api::results::Tag;
        // Allow even when no override is installed — RESET should be idempotent.
        if !identity.is_superuser {
            // No silent success: matches SET TENANT's policy so a non-superuser
            // can't probe whether a tenant override exists.
            return Err(sqlstate_error("42501", "only superuser may RESET TENANT"));
        }
        if self.sessions.transaction_state(addr) != super::super::session::TransactionState::Idle {
            return Err(sqlstate_error(
                "25001",
                "cannot RESET TENANT inside an active transaction",
            ));
        }
        self.sessions.set_effective_tenant_id(addr, None);
        Ok(vec![Response::Execution(Tag::new("RESET"))])
    }

    /// Handle SHOW commands: return session parameter values.
    pub(super) fn handle_show(
        &self,
        identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
        sql: &str,
    ) -> PgWireResult<Vec<Response>> {
        use super::super::session::{is_known_pg_runtime_parameter, parse_show_command};

        let param = match parse_show_command(sql) {
            Some(p) => p,
            None => {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".to_owned(),
                    "42601".to_owned(),
                    "syntax error: SHOW <parameter> or SHOW ALL".to_owned(),
                ))));
            }
        };

        if param == "all" {
            return self.handle_show_all(addr);
        }

        // `SHOW TENANT` (singular) reports the session's *effective* tenant —
        // the override installed via `SET TENANT = ...` if any, otherwise the
        // identity-bound tenant. Returns a single row with `tenant_id` and
        // `tenant_name` so a session that switched can confirm where its
        // writes will land. `SHOW TENANTS` (plural) is a separate DDL.
        if param == "tenant" {
            let effective = self
                .sessions
                .get_effective_tenant_id(addr)
                .unwrap_or(identity.tenant_id);
            let name = self
                .state
                .credentials
                .catalog()
                .as_ref()
                .and_then(|c| c.load_all_tenants().ok())
                .and_then(|tenants| {
                    tenants
                        .into_iter()
                        .find(|t| t.tenant_id == effective.as_u64())
                        .map(|t| t.name)
                })
                .unwrap_or_default();
            let schema = Arc::new(vec![text_field("tenant_id"), text_field("tenant_name")]);
            let mut encoder = DataRowEncoder::new(schema.clone());
            encoder.encode_field(&effective.as_u64().to_string())?;
            encoder.encode_field(&name)?;
            let row = encoder.take_row();
            return Ok(vec![Response::Query(QueryResponse::new(
                schema,
                futures::stream::iter(vec![Ok(row)]),
            ))]);
        }

        // Resolve the value from the runtime-parameter sources in order:
        // built-in PG runtime constants first, then a value explicitly set
        // by `SET` in this session. If neither matches and the parameter
        // is not on the known-parameter allowlist, return `42704`
        // (`undefined_object`) — the same SQLSTATE PostgreSQL uses when
        // a client requests an unrecognised runtime parameter. This
        // prevents administrative commands like `SHOW DATABASES`,
        // `SHOW ROLES`, `SHOW STATS`, `SHOW METRICS`, `SHOW MEMORY`
        // from being silently swallowed as if they were unset session
        // parameters; those commands are routed through the DDL / AST
        // router before this handler is reached.
        let builtin = match param.as_str() {
            "server_version" => Some(format!("NodeDB {}", crate::version::VERSION)),
            "server_encoding" => Some("UTF8".into()),
            _ => None,
        };
        let session_value = self.sessions.get_parameter(addr, &param);

        let value = match (builtin, session_value) {
            (Some(v), _) => v,
            (None, Some(v)) => v,
            (None, None) => {
                if !is_known_pg_runtime_parameter(&param) {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".to_owned(),
                        "42704".to_owned(),
                        format!("unrecognized configuration parameter \"{param}\""),
                    ))));
                }
                String::new()
            }
        };

        let schema = Arc::new(vec![text_field(&param)]);
        let mut encoder = DataRowEncoder::new(schema.clone());
        encoder.encode_field(&value)?;
        let row = encoder.take_row();
        Ok(vec![Response::Query(QueryResponse::new(
            schema,
            futures::stream::iter(vec![Ok(row)]),
        ))])
    }

    /// SHOW ALL — return all session parameters.
    pub(super) fn handle_show_all(
        &self,
        addr: &std::net::SocketAddr,
    ) -> PgWireResult<Vec<Response>> {
        let schema = Arc::new(vec![text_field("name"), text_field("setting")]);

        let params = self.sessions.all_parameters(addr);
        let mut rows = Vec::with_capacity(params.len());
        let mut encoder = DataRowEncoder::new(schema.clone());

        for (key, value) in &params {
            encoder.encode_field(key)?;
            encoder.encode_field(value)?;
            rows.push(Ok(encoder.take_row()));
        }

        Ok(vec![Response::Query(QueryResponse::new(
            schema,
            futures::stream::iter(rows),
        ))])
    }

    /// Handle EXPLAIN: plan the inner SQL and return the plan description.
    pub(super) async fn handle_explain(
        &self,
        identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
        sql: &str,
    ) -> PgWireResult<Vec<Response>> {
        let upper = sql.to_uppercase();
        let is_analyze = upper.starts_with("EXPLAIN ANALYZE ");

        let inner_sql = if is_analyze {
            sql[16..].trim()
        } else if upper.starts_with("EXPLAIN ") {
            sql[8..].trim()
        } else {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "42601".to_owned(),
                "syntax error in EXPLAIN".to_owned(),
            ))));
        };

        let database_id = self
            .sessions
            .get_current_database(addr)
            .unwrap_or(crate::types::DatabaseId::DEFAULT);
        if super::super::ddl::dispatch(&self.state, identity, inner_sql, database_id)
            .await
            .is_some()
        {
            let schema = Arc::new(vec![text_field("QUERY PLAN")]);
            let plan_text = format!(
                "DDL: {}",
                inner_sql
                    .split_whitespace()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let mut encoder = DataRowEncoder::new(schema.clone());
            encoder.encode_field(&plan_text)?;
            let row = encoder.take_row();
            return Ok(vec![Response::Query(QueryResponse::new(
                schema,
                futures::stream::iter(vec![Ok(row)]),
            ))]);
        }

        let tenant_id = identity.tenant_id;
        let auth_ctx = crate::control::server::session_auth::build_auth_context(identity);
        let perm_cache = self.state.permission_cache.read().await;
        let sec = crate::control::planner::context::PlanSecurityContext {
            identity,
            auth: &auth_ctx,
            rls_store: &self.state.rls,
            permissions: &self.state.permissions,
            roles: &self.state.roles,
            permission_cache: Some(&*perm_cache),
        };
        let tasks = self
            .query_ctx
            .plan_sql_with_rls(inner_sql, tenant_id, database_id, &sec)
            .await
            .map_err(|e| {
                let (severity, code, message) = error_to_sqlstate(&e);
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    severity.to_owned(),
                    code.to_owned(),
                    message,
                )))
            })?;

        let schema = Arc::new(vec![text_field("QUERY PLAN")]);
        let mut rows = Vec::new();
        let mut encoder = DataRowEncoder::new(schema.clone());

        // Prepend Calvin preamble row when tasks span multiple vShards.
        {
            use crate::control::planner::calvin::calvin_explain_preamble;
            let mode = self.sessions.cross_shard_txn_mode(addr);
            if let Some(preamble) = calvin_explain_preamble(&tasks, mode, None) {
                encoder.encode_field(&preamble)?;
                rows.push(Ok(encoder.take_row()));
            }
        }

        if tasks.is_empty() {
            encoder.encode_field(&"Empty plan (no tasks)")?;
            rows.push(Ok(encoder.take_row()));
        } else {
            for (i, task) in tasks.iter().enumerate() {
                let plan_desc = format!(
                    "Task {}: {:?} tenant={} vshard={}",
                    i + 1,
                    task.plan,
                    task.tenant_id.as_u64(),
                    task.vshard_id.as_u32(),
                );
                for line in plan_desc.lines() {
                    encoder.encode_field(&line)?;
                    rows.push(Ok(encoder.take_row()));
                }
            }
        }

        Ok(vec![Response::Query(QueryResponse::new(
            schema,
            futures::stream::iter(rows),
        ))])
    }
}

#[cfg(test)]
mod tests {
    use super::{TransactionCmd, classify_transaction_cmd};

    /// tenant_id values above u32::MAX must parse without error via u64.
    #[test]
    fn tenant_id_above_u32_max_parses_as_u64() {
        let big = "4294967296"; // u32::MAX + 1
        assert!(big.parse::<u64>().is_ok(), "should parse as u64");
        assert!(big.parse::<u32>().is_err(), "should NOT parse as u32");
    }

    fn run(sql: &str) -> TransactionCmd {
        let upper = sql.to_uppercase();
        classify_transaction_cmd(&upper, sql)
    }

    fn is_accept(cmd: TransactionCmd) -> bool {
        matches!(
            cmd,
            TransactionCmd::SetReadOnly
                | TransactionCmd::SetReadWrite
                | TransactionCmd::AcceptIsolation
        )
    }

    fn rejection_code(cmd: TransactionCmd) -> Option<String> {
        match cmd {
            TransactionCmd::RejectIsolation(msg) => Some(msg),
            _ => None,
        }
    }

    #[test]
    fn set_transaction_read_only() {
        assert!(is_accept(run("SET TRANSACTION READ ONLY")));
        assert!(matches!(
            run("SET TRANSACTION READ ONLY"),
            TransactionCmd::SetReadOnly
        ));
    }

    #[test]
    fn set_transaction_read_write() {
        assert!(matches!(
            run("SET TRANSACTION READ WRITE"),
            TransactionCmd::SetReadWrite
        ));
    }

    #[test]
    fn set_transaction_read_committed() {
        assert!(matches!(
            run("SET TRANSACTION ISOLATION LEVEL READ COMMITTED"),
            TransactionCmd::AcceptIsolation
        ));
    }

    #[test]
    fn set_transaction_serializable() {
        let msg = rejection_code(run("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE"))
            .expect("expected rejection");
        assert!(
            msg.contains("SERIALIZABLE"),
            "message should name the level: {msg}"
        );
        assert!(
            msg.contains("Snapshot Isolation"),
            "message should mention Snapshot Isolation: {msg}"
        );
    }

    #[test]
    fn set_transaction_repeatable_read() {
        let msg = rejection_code(run("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"))
            .expect("expected rejection");
        assert!(msg.contains("REPEATABLE READ"), "{msg}");
    }

    #[test]
    fn set_transaction_read_uncommitted() {
        let msg = rejection_code(run("SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED"))
            .expect("expected rejection");
        assert!(msg.contains("READ UNCOMMITTED"), "{msg}");
    }

    #[test]
    fn set_session_characteristics_serializable() {
        let sql = "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE";
        let msg = rejection_code(run(sql)).expect("expected rejection");
        assert!(msg.contains("SERIALIZABLE"), "{msg}");
    }

    #[test]
    fn set_transaction_unknown_option() {
        let msg = rejection_code(run("SET TRANSACTION DEFERRABLE"))
            .expect("expected rejection for unknown option");
        assert!(
            msg.contains("unsupported"),
            "message should say unsupported: {msg}"
        );
    }
}
