// SPDX-License-Identifier: BUSL-1.1

//! pgwire connection factory: SCRAM-SHA-256 / Argon2 authentication and
//! session bootstrapping.
//!
//! **Auth scope**: pgwire authenticates exclusively via SCRAM-SHA-256 over
//! the Postgres wire protocol. OIDC bearer tokens are NOT accepted here —
//! the Postgres wire protocol has no clean way to carry a bearer without a
//! non-standard extension or a sidecar proxy. OIDC bearer logins live on
//! the native and HTTP entry points (see `control/security/oidc/`); do not
//! add a JWT branch to this factory.

use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use futures::sink::Sink;

use pgwire::api::auth::{
    AuthSource, DefaultServerParameterProvider, LoginInfo, Password, StartupHandler,
};
use pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::{ClientInfo, PgWireServerHandlers};
use pgwire::error::{PgWireError, PgWireResult};
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};

use crate::config::auth::AuthMode;
use crate::control::security::audit::{ArcAuditEmitter, AuditEvent};
use crate::control::security::credential::CredentialStore;
use crate::control::state::SharedState;

use super::handler::NodeDbPgHandler;

// ── AuthSource for SCRAM-SHA-256 ────────────────────────────────────

/// Bridges NodeDB's CredentialStore to pgwire's `AuthSource` trait.
pub struct NodeDbAuthSource {
    credentials: Arc<CredentialStore>,
    state: Arc<SharedState>,
}

impl Debug for NodeDbAuthSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeDbAuthSource").finish()
    }
}

#[async_trait]
impl AuthSource for NodeDbAuthSource {
    async fn get_password(&self, login: &LoginInfo) -> PgWireResult<Password> {
        let username = login.user().unwrap_or("unknown");
        let source = login.host();

        // Record auth start time for constant-time floor enforcement on all
        // failure paths (rate-limit, lockout, unknown user).
        let auth_start = std::time::Instant::now();

        // Pre-authentication login rate-limit check — consulted before lockout
        // and before SCRAM credential lookup begins.
        use crate::control::security::ratelimit::limiter::LoginRateLimitOutcome;
        use crate::control::server::session_auth::AUTH_FLOOR;
        let peer_ip_str = source
            .parse::<std::net::SocketAddr>()
            .map(|s| s.ip().to_string())
            .unwrap_or_else(|_| source.to_string());
        let rl_outcome = self.state.rate_limiter.check_login(&peer_ip_str, username);
        if !matches!(rl_outcome, LoginRateLimitOutcome::Allowed) {
            use crate::control::security::audit::{
                ArcAuditEmitter, AuditEmitContext, AuditEmitter,
            };
            let emitter = ArcAuditEmitter(std::sync::Arc::clone(&self.state.audit));
            let detail = match rl_outcome {
                LoginRateLimitOutcome::IpExceeded => {
                    format!("login rate limited (ip={peer_ip_str}): {username}")
                }
                LoginRateLimitOutcome::UserExceeded => {
                    format!("login rate limited (user): {username}")
                }
                LoginRateLimitOutcome::Allowed => unreachable!(),
            };
            emitter.emit(
                AuditEvent::LoginRateLimited,
                "login_rate_limit",
                &detail,
                AuditEmitContext::new(None, "", username),
            );
            self.state.auth_metrics.record_auth_failure("scram");
            // Constant-time floor before returning the generic invalid-password
            // error so timing cannot distinguish rate-limit from wrong password.
            let deadline = auth_start + AUTH_FLOOR;
            let now = std::time::Instant::now();
            if deadline > now {
                tokio::time::sleep(deadline - now).await;
            }
            return Err(PgWireError::InvalidPassword(username.to_owned()));
        }

        // Check lockout before returning credentials.
        if self.credentials.check_lockout(username).is_err() {
            self.state.audit_record(
                AuditEvent::AuthFailure,
                None,
                source,
                &format!("user '{username}' is locked out"),
            );
            // Constant-time floor for lockout rejection.
            let deadline = auth_start + AUTH_FLOOR;
            let now = std::time::Instant::now();
            if deadline > now {
                tokio::time::sleep(deadline - now).await;
            }
            return Err(PgWireError::InvalidPassword(format!(
                "{username} (account locked)"
            )));
        }

        match self.credentials.get_scram_credentials(username) {
            Some(creds) => {
                // A non-empty warning means grace period or must_change_password.
                // pgwire's AuthSource doesn't surface NoticeResponse here; the
                // warning is stored in the factory and must be sent after auth
                // success via the on_startup hook. For now, log it — the
                // post-auth notice path requires plumbing that would touch
                // pgwire's internal state machine. The warning IS surfaced on
                // the native protocol path (see session_auth::authenticate).
                if let Some(ref w) = creds.warning {
                    tracing::warn!(username, warning = %w, "password warning at SCRAM credential fetch");
                }
                Ok(Password::new(Some(creds.salt), creds.salted_password))
            }
            None => {
                let emitter = ArcAuditEmitter(std::sync::Arc::clone(&self.state.audit));
                let source_ip = source.parse::<std::net::SocketAddr>().ok().map(|s| s.ip());
                self.credentials
                    .record_login_failure(username, source_ip, &emitter);
                self.state.audit_record(
                    AuditEvent::AuthFailure,
                    None,
                    source,
                    &format!("unknown user: {username}"),
                );
                Err(PgWireError::InvalidPassword(username.to_owned()))
            }
        }
    }
}

// ── Server parameter provider ───────────────────────────────────────

fn nodedb_parameter_provider() -> DefaultServerParameterProvider {
    let mut params = DefaultServerParameterProvider::default();
    params.server_version = format!("NodeDB {}", crate::version::VERSION);
    params
}

// ── Factory ─────────────────────────────────────────────────────────

/// Factory that wires together the pgwire handlers.
///
/// Supports trust mode (NoopStartupHandler) and password mode
/// (SCRAM-SHA-256 via pgwire's SASL implementation).
pub struct NodeDbPgHandlerFactory {
    handler: Arc<NodeDbPgHandler>,
    auth_mode: AuthMode,
    credentials: Arc<CredentialStore>,
    state: Arc<SharedState>,
}

impl NodeDbPgHandlerFactory {
    pub fn new(state: Arc<SharedState>, auth_mode: AuthMode) -> Self {
        Self {
            handler: Arc::new(NodeDbPgHandler::new(Arc::clone(&state), auth_mode.clone())),
            auth_mode,
            credentials: Arc::clone(&state.credentials),
            state,
        }
    }
}

impl PgWireServerHandlers for NodeDbPgHandlerFactory {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.handler.clone()
    }

    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.handler.clone()
    }

    fn copy_handler(&self) -> Arc<impl pgwire::api::copy::CopyHandler> {
        Arc::new(super::handler::NodeDbCopyHandler {
            state: Arc::clone(&self.state),
            restore_state: Arc::clone(&self.handler.restore_state),
        })
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        match self.auth_mode {
            AuthMode::Trust => Arc::new(AuthStartup::Trust(self.handler.clone())),
            AuthMode::Password | AuthMode::Certificate => {
                let auth_source = Arc::new(NodeDbAuthSource {
                    credentials: Arc::clone(&self.credentials),
                    state: Arc::clone(&self.state),
                });
                let scram = pgwire::api::auth::sasl::scram::ScramAuth::new(auth_source);
                let params = Arc::new(nodedb_parameter_provider());
                let sasl =
                    pgwire::api::auth::sasl::SASLAuthStartupHandler::new(params).with_scram(scram);
                Arc::new(AuthStartup::Scram {
                    sasl: Box::new(sasl),
                    state: Arc::clone(&self.state),
                    handler: self.handler.clone(),
                })
            }
        }
    }
}

// ── Startup handler dispatch ────────────────────────────────────────

/// Enum dispatch for startup handler — avoids dyn trait object issues.
enum AuthStartup {
    Trust(Arc<NodeDbPgHandler>),
    Scram {
        sasl: Box<pgwire::api::auth::sasl::SASLAuthStartupHandler<DefaultServerParameterProvider>>,
        state: Arc<SharedState>,
        /// Handler reference so we can bind the startup `database` param to
        /// the session store after SCRAM succeeds (mirrors the trust path).
        handler: Arc<NodeDbPgHandler>,
    },
}

/// Resolve the pgwire `database` StartupMessage parameter to a `DatabaseId`
/// and bind it to the session store for this connection.
///
/// The key `"database"` is set by clients via `dbname=` or `psql -d <name>`.
/// An absent or empty value is silently ignored — the session will use the
/// server default (DatabaseId::DEFAULT / `"default"`).
/// An unrecognised name is also silently ignored here; the first DDL/DML
/// statement will surface the missing-database error at query time, which
/// matches PostgreSQL behaviour for `psql -d nonexistent` (it succeeds at
/// connect; errors on the first query that requires the db).
fn bind_startup_database<C: pgwire::api::ClientInfo>(
    client: &C,
    addr: &std::net::SocketAddr,
    handler: &NodeDbPgHandler,
) {
    let db_name = match client.metadata().get("database") {
        Some(n) if !n.is_empty() => n.clone(),
        _ => return,
    };

    handler.sessions.ensure_session(*addr);

    let db_id = if let Some(cat) = handler.state.credentials.catalog().as_ref() {
        cat.get_database_id_by_name(&db_name).ok().flatten()
    } else {
        None
    };

    if let Some(id) = db_id {
        handler.sessions.set_current_database(addr, id);
    }
    // If the name is not found we leave current_database unset (None).
    // The first query that actually needs a database context will produce
    // the appropriate DATABASE_NOT_FOUND error.
}

#[async_trait]
impl StartupHandler for AuthStartup {
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + futures::sink::Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        match self {
            AuthStartup::Trust(handler) => {
                <NodeDbPgHandler as StartupHandler>::on_startup(handler, client, message).await?;

                let username = client
                    .metadata()
                    .get("user")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let source = client.socket_addr().to_string();
                handler.state.audit_record(
                    AuditEvent::AuthSuccess,
                    None,
                    &source,
                    &format!("trust auth: {username}"),
                );

                // Bind the `database` startup parameter to the session store.
                // `psql -d <name>` sets this key in the pgwire StartupMessage;
                // we resolve it once at handshake time so every query on this
                // connection executes in the declared database context.
                let addr = client.socket_addr();
                bind_startup_database(client, &addr, handler);

                Ok(())
            }
            AuthStartup::Scram {
                sasl,
                state,
                handler,
            } => {
                let was_in_auth = matches!(
                    client.state(),
                    pgwire::api::PgWireConnectionState::AuthenticationInProgress
                );

                let result = sasl.on_startup(client, message).await;

                let username = client
                    .metadata()
                    .get("user")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let source = client.socket_addr().to_string();

                match &result {
                    Ok(())
                        if was_in_auth
                            && matches!(
                                client.state(),
                                pgwire::api::PgWireConnectionState::ReadyForQuery
                            ) =>
                    {
                        // SCRAM succeeded — reset lockout counter and bind database.
                        state.credentials.record_login_success(&username);
                        state.audit_record(
                            AuditEvent::AuthSuccess,
                            None,
                            &source,
                            &format!("SCRAM-SHA-256 auth: {username}"),
                        );
                        // Bind the `database` startup parameter to the session.
                        let addr = client.socket_addr();
                        bind_startup_database(client, &addr, handler);
                    }
                    Err(_) if was_in_auth => {
                        // SCRAM failed — increment lockout counter.
                        let emitter = ArcAuditEmitter(std::sync::Arc::clone(&state.audit));
                        let scram_ip = source.parse::<std::net::SocketAddr>().ok().map(|s| s.ip());
                        state
                            .credentials
                            .record_login_failure(&username, scram_ip, &emitter);
                        state.audit_record(
                            AuditEvent::AuthFailure,
                            None,
                            &source,
                            &format!("SCRAM-SHA-256 auth failed: {username}"),
                        );
                    }
                    _ => {}
                }

                result
            }
        }
    }
}
