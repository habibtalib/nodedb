// SPDX-License-Identifier: BUSL-1.1

//! Session management DDL commands.
//!
//! ```sql
//! SHOW SESSIONS
//! KILL SESSION '<session_id>'
//! KILL USER SESSIONS '<auth_user_id>'
//! VERIFY AUDIT CHAIN
//! ```

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::types::{sqlstate_error, text_field};

/// SHOW SESSIONS
pub fn show_sessions(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    _parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: requires superuser",
        ));
    }

    let sessions = state.session_registry.list_all();

    let schema = Arc::new(vec![
        text_field("session_id"),
        text_field("user_id"),
        text_field("db_user"),
        text_field("auth_method"),
        text_field("connected_at"),
        text_field("last_active"),
        text_field("client_ip"),
        text_field("protocol"),
    ]);

    let rows: Vec<_> = sessions
        .iter()
        .map(|s| {
            let mut enc = DataRowEncoder::new(schema.clone());
            let _ = enc.encode_field(&s.session_id);
            let _ = enc.encode_field(&s.user_id.to_string());
            let _ = enc.encode_field(&s.db_user);
            let _ = enc.encode_field(&s.auth_method);
            let _ = enc.encode_field(&s.connected_at.to_string());
            let _ = enc.encode_field(&s.last_active.to_string());
            let _ = enc.encode_field(&s.client_ip);
            let _ = enc.encode_field(&s.protocol);
            Ok(enc.take_row())
        })
        .collect();

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// KILL SESSION '<session_id>'
pub fn kill_session(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: requires superuser",
        ));
    }
    if parts.len() < 3 {
        return Err(sqlstate_error(
            "42601",
            "syntax: KILL SESSION '<session_id>'",
        ));
    }
    let session_id = parts[2].trim_matches('\'');

    state.session_registry.unregister(session_id);

    state.audit_record(
        crate::control::security::audit::AuditEvent::AdminAction,
        Some(identity.tenant_id),
        &identity.username,
        &format!("killed session '{session_id}'"),
    );

    Ok(vec![Response::Execution(Tag::new("KILL SESSION"))])
}

/// KILL USER SESSIONS '<auth_user_id>'
pub fn kill_user_sessions(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: requires superuser",
        ));
    }
    if parts.len() < 4 {
        return Err(sqlstate_error(
            "42601",
            "syntax: KILL USER SESSIONS '<auth_user_id>'",
        ));
    }
    let user_id_str = parts[3].trim_matches('\'');
    let user_id: u64 = user_id_str.parse().map_err(|_| {
        sqlstate_error(
            "22003",
            &format!("invalid user_id '{user_id_str}': must be numeric"),
        )
    })?;

    let killed = state.session_registry.kill_sessions_for_user(user_id);

    state.audit_record(
        crate::control::security::audit::AuditEvent::AdminAction,
        Some(identity.tenant_id),
        &identity.username,
        &format!("killed {killed} sessions for user_id={user_id}"),
    );

    Ok(vec![Response::Execution(Tag::new(&format!(
        "KILL {killed}"
    )))])
}

/// VERIFY AUDIT CHAIN
pub fn verify_audit_chain(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    _parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: requires superuser",
        ));
    }

    let audit = state.audit.lock().unwrap_or_else(|p| p.into_inner());
    match audit.verify_chain() {
        Ok(()) => {
            let schema = Arc::new(vec![text_field("status"), text_field("entries")]);
            let mut enc = DataRowEncoder::new(schema.clone());
            let _ = enc.encode_field(&"VALID");
            let _ = enc.encode_field(&audit.len().to_string());
            Ok(vec![Response::Query(QueryResponse::new(
                schema,
                stream::iter(vec![Ok(enc.take_row())]),
            ))])
        }
        Err(broken_seq) => Err(sqlstate_error(
            "XX001",
            &format!("audit chain broken at sequence {broken_seq}"),
        )),
    }
}
