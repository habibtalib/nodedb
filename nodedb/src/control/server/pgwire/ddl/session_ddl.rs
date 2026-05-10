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
        text_field("idle_seconds"),
        text_field("client_ip"),
        text_field("protocol"),
        text_field("current_database"),
        text_field("bytes_in"),
        text_field("bytes_out"),
        text_field("current_statement"),
        text_field("token_expires_in_secs"),
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
            let _ = enc.encode_field(&s.idle_seconds.to_string());
            let _ = enc.encode_field(&s.client_ip);
            let _ = enc.encode_field(&s.protocol);
            let _ = enc.encode_field(&s.current_database.as_u64().to_string());
            let _ = enc.encode_field(&s.bytes_in.to_string());
            let _ = enc.encode_field(&s.bytes_out.to_string());
            let current_stmt = s
                .current_statement_digest
                .as_deref()
                .unwrap_or("")
                .to_string();
            let _ = enc.encode_field(&current_stmt);
            let token_exp = s
                .token_expires_in_seconds
                .map(|v| v.to_string())
                .unwrap_or_default();
            let _ = enc.encode_field(&token_exp);
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
    if parts.len() < 3 {
        return Err(sqlstate_error(
            "42601",
            "syntax: KILL SESSION '<session_id>'",
        ));
    }
    let session_id = parts[2].trim_matches('\'');

    // Resolve target's bound database WITHOUT killing — needed for the
    // DatabaseOwner authority branch and to surface a precise audit row.
    let target_db = match state.session_registry.lookup_session_database(session_id) {
        Some(db) => db,
        None => {
            return Err(sqlstate_error(
                "42704",
                &format!("session '{session_id}' not found"),
            ));
        }
    };

    // Permission: Superuser, ClusterAdmin, or DatabaseOwner of the session's db.
    let authorized = identity.is_superuser
        || identity.has_cluster_admin()
        || identity.is_database_owner(target_db);
    if !authorized {
        state.audit_record_with_db(
            crate::control::security::audit::AuditEvent::PermissionDenied,
            Some(identity.tenant_id),
            Some(target_db),
            &identity.username,
            &format!("KILL SESSION '{session_id}'"),
        );
        return Err(sqlstate_error(
            "42501",
            "permission denied: KILL SESSION requires superuser, cluster_admin, or database_owner of the session's database",
        ));
    }

    // Now signal the kill. The session may have disconnected between the
    // permission check above and this call; `kill_session_by_id` returns
    // `None` in that race, in which case no kill signal was sent and we
    // must not record `SessionRevoked` (which would be a false audit).
    match state.session_registry.kill_session_by_id(
        session_id,
        crate::control::security::sessions::KillReason::AdminKill,
    ) {
        Some(_db) => {
            state.audit_record_with_db(
                crate::control::security::audit::AuditEvent::SessionRevoked,
                Some(identity.tenant_id),
                Some(target_db),
                &identity.username,
                &format!("killed session '{session_id}' by {}", identity.username),
            );
            Ok(vec![Response::Execution(Tag::new("KILL SESSION"))])
        }
        None => {
            // Session disappeared between authority check and kill — return
            // a precise error to the client and a separate audit record so
            // operators can distinguish "kill applied" from "kill raced".
            state.audit_record_with_db(
                crate::control::security::audit::AuditEvent::AdminAction,
                Some(identity.tenant_id),
                Some(target_db),
                &identity.username,
                &format!(
                    "KILL SESSION '{session_id}' raced — session disconnected before kill applied"
                ),
            );
            Err(sqlstate_error(
                "42704",
                &format!("session '{session_id}' disconnected before KILL applied"),
            ))
        }
    }
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

    let killed = state.session_registry.kill_sessions_for_user(
        user_id,
        crate::control::security::sessions::KillReason::AdminKill,
    );

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
