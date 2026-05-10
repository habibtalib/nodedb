// SPDX-License-Identifier: BUSL-1.1

//! Authentication and ping handlers.

use nodedb_types::protocol::{AuthMethod as ProtoAuth, NativeResponse};

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

/// Authenticate a native protocol client.
///
/// Returns `(identity, warning)` — warning is non-empty when the account
/// is in a password grace period or `must_change_password` is set.
///
/// `OidcBearer` tokens are validated directly against the OIDC provider catalog
/// (not the `JwksRegistry` provider list), enabling runtime `CREATE OIDC PROVIDER`
/// without a server restart.
pub(crate) async fn handle_auth(
    state: &SharedState,
    auth_mode: &crate::config::auth::AuthMode,
    auth: &ProtoAuth,
    peer_addr: &str,
) -> crate::Result<(AuthenticatedIdentity, Option<String>)> {
    if let ProtoAuth::OidcBearer { token, .. } = auth {
        let identity = crate::control::security::oidc::verify_bearer_token(state, token).await?;
        state.audit_record(
            crate::control::security::audit::AuditEvent::AuthSuccess,
            Some(identity.tenant_id),
            peer_addr,
            &format!(
                "OIDC bearer login: sub={} method=oidc_bearer",
                identity.username
            ),
        );
        state.auth_metrics.record_auth_success("oidc_bearer");
        return Ok((identity, None));
    }

    let body = match auth {
        ProtoAuth::Trust { username } => {
            serde_json::json!({ "method": "trust", "username": username })
        }
        ProtoAuth::Password { username, password } => {
            serde_json::json!({ "method": "password", "username": username, "password": password })
        }
        ProtoAuth::ApiKey { token } => {
            serde_json::json!({ "method": "api_key", "token": token })
        }
        _ => {
            return Err(crate::Error::BadRequest {
                detail: "unsupported authentication method".into(),
            });
        }
    };

    super::super::super::session_auth::authenticate(state, auth_mode, &body, peer_addr).await
}

/// Respond to a ping with a pong.
pub(crate) fn handle_ping(seq: u64) -> NativeResponse {
    NativeResponse::status_row(seq, "PONG")
}
