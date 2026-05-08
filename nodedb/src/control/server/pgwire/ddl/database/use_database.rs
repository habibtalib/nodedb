// SPDX-License-Identifier: BUSL-1.1

//! Handler for `USE DATABASE <name>` — mid-session database switch.
//!
//! Issues a session reset per the design:
//!   1. Aborts any open transaction.
//!   2. Invalidates all prepared statements for this connection.
//!   3. Rebinds `current_database` to the new database.
//!
//! If the named database does not exist, returns `DATABASE_NOT_FOUND` (3D000).
//! `\c <name>` in the CLI expands to `USE DATABASE <name>`.

use std::net::SocketAddr;

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::server::pgwire::session::SessionStore;
use crate::control::state::SharedState;

use super::super::super::types::sqlstate_error;

/// Handle `USE DATABASE <name>`.
///
/// `sessions` must be the per-handler `SessionStore` so the transaction and
/// prepared-statement state for this connection can be reset atomically.
pub fn handle_use_database(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    sessions: &SessionStore,
    addr: &SocketAddr,
    name: &str,
) -> PgWireResult<Vec<Response>> {
    let catalog = match state.credentials.catalog() {
        Some(c) => c,
        None => {
            return Err(sqlstate_error("XX000", "system catalog unavailable"));
        }
    };

    // Verify the named database exists.
    let db_id = catalog
        .get_database_id_by_name(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog lookup failed: {e}")))?
        .ok_or_else(|| sqlstate_error("3D000", &format!("database '{name}' does not exist")))?;

    // Access check: per-user `accessible_databases` enforcement is owned by
    // the user-management subsystem and is not yet wired here. Until it
    // lands, any authenticated user may switch to any existing database.

    // Session reset: abort open transaction, invalidate prepared statements.
    sessions.reset_for_database_switch(addr, db_id);

    state.audit_record(
        crate::control::security::audit::AuditEvent::DdlChange,
        None,
        &identity.username,
        &format!("USE DATABASE {name}"),
    );

    Ok(vec![Response::Execution(Tag::new("USE DATABASE"))])
}
