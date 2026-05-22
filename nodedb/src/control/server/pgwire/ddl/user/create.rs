// SPDX-License-Identifier: BUSL-1.1

//! `CREATE USER` DDL handler.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::{AuthenticatedIdentity, Role};
use crate::control::server::pgwire::types::{parse_role, require_tenant_admin, sqlstate_error};
use crate::control::state::SharedState;
use crate::types::TenantId;

/// CREATE USER <name> WITH PASSWORD '<password>' [ROLE <role>] [TENANT <id>]
pub fn create_user(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    username: &str,
    password: &str,
    role_name: Option<&str>,
    tenant_id_override: Option<u64>,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "create users")?;

    if username.is_empty() {
        return Err(sqlstate_error(
            "42601",
            "syntax: CREATE USER <name> WITH PASSWORD '<password>' [ROLE <role>] [TENANT <id>]",
        ));
    }

    if password.is_empty() {
        return Err(sqlstate_error(
            "42601",
            "password must be a single-quoted string",
        ));
    }

    let role = role_name.map(parse_role).unwrap_or(Role::ReadWrite);
    let tenant_id = if let Some(tid) = tenant_id_override {
        if !identity.is_superuser {
            return Err(sqlstate_error("42501", "only superuser can assign tenants"));
        }
        TenantId::new(tid)
    } else {
        identity.tenant_id
    };

    // Build the full `StoredUser` locally (hash + salt + user_id).
    // Followers cannot reproduce the random salt, so this step
    // MUST happen on the proposer node. The computed record is
    // then replicated verbatim.
    let stored = state
        .credentials
        .prepare_user(username, password, tenant_id, vec![role])
        .map_err(|e| sqlstate_error("42710", &e.to_string()))?;

    let entry = crate::control::catalog_entry::CatalogEntry::PutUser(Box::new(stored.clone()));
    let log_index = crate::control::metadata_proposer::propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    if log_index == 0 {
        // Single-node / no-cluster fallback: install into the
        // in-memory cache so subsequent reads see the user.
        // Persist to redb when a catalog is wired up — the
        // catalog write is best-effort durability, not a gate
        // on the cache update. Test fixtures (and any future
        // fully-in-memory deployment) can run without a redb
        // catalog and still get correct read-after-write.
        if let Some(catalog) = state.credentials.catalog() {
            catalog
                .put_user(&stored)
                .map_err(|e| sqlstate_error("XX000", &format!("catalog write: {e}")))?;
        }
        // CREATE USER: no open sessions exist for a brand-new user.
        state.credentials.install_replicated_user(&stored, None);
    } else {
        // Cluster mode: `propose_catalog_entry` waits for the
        // entry to be applied on THIS node, which runs the
        // synchronous post_apply (`install_replicated_user`)
        // inline BEFORE the applied-index watermark bumps. So if
        // our entry really committed, `get_user` must see it now.
        //
        // If `get_user` returns None, the Raft log entry at the
        // index our leader assigned has been truncated and
        // overwritten with a noop from a new leader term (a known
        // Raft subtlety: `propose` returns the assigned log index
        // without waiting for commit; if leadership changes
        // before the quorum ack, the entry is dropped). Return a
        // retryable error so `exec_ddl_on_any_leader` re-proposes
        // on the next attempt against whoever is now leader.
        if state.credentials.get_user(username).is_none() {
            return Err(sqlstate_error(
                "40001",
                "transient: metadata entry truncated by leader change, retry",
            ));
        }
    }

    state.audit_record(
        AuditEvent::PrivilegeChange,
        Some(tenant_id),
        &identity.username,
        &format!("created user '{username}' in tenant {tenant_id}"),
    );

    Ok(vec![Response::Execution(Tag::new("CREATE USER"))])
}
