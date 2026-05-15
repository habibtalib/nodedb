// SPDX-License-Identifier: BUSL-1.1

//! `CREATE [OR REPLACE] PROCEDURE` pgwire handler.
//!
//! Grammar:
//! ```text
//! CREATE [OR REPLACE] PROCEDURE <name>(<param> <type> [, ...])
//!   [WITH (MAX_ITERATIONS = N, TIMEOUT = N)]
//!   AS BEGIN ... END;
//! ```

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::catalog::procedure_types::StoredProcedure;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::super::types::{require_tenant_admin, sqlstate_error};
use super::parse::parse_create_procedure;
use super::routability::extract_routability;

/// Handle `CREATE [OR REPLACE] PROCEDURE ...`
pub fn create_procedure(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    sql: &str,
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "create procedures")?;

    let parsed = parse_create_procedure(sql)?;
    let tenant_id = identity.tenant_id.as_u64();

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog not available"))?;

    if !parsed.or_replace
        && let Ok(Some(_)) = catalog.get_procedure(tenant_id, &parsed.name)
    {
        return Err(sqlstate_error(
            "42723",
            &format!("procedure '{}' already exists", parsed.name),
        ));
    }

    // Validate body parses as procedural SQL.
    crate::control::planner::procedural::parse_block(&parsed.body_sql)
        .map_err(|e| sqlstate_error("42601", &format!("procedure body parse error: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| sqlstate_error("XX000", "system clock before UNIX epoch"))?
        .as_secs();

    let routability = extract_routability(&parsed.body_sql);

    let stored = StoredProcedure {
        tenant_id,
        name: parsed.name.clone(),
        parameters: parsed.parameters,
        body_sql: parsed.body_sql,
        max_iterations: parsed.max_iterations,
        timeout_secs: parsed.timeout_secs,
        routability,
        owner: identity.username.clone(),
        created_at: now,
        descriptor_version: 0,
        modification_hlc: nodedb_types::Hlc::ZERO,
    };

    // Replicate through the metadata raft group. Every node's
    // applier writes the record to local redb and clears the
    // parsed block cache so the next CALL re-parses the new body.
    let entry = crate::control::catalog_entry::CatalogEntry::PutProcedure(Box::new(stored.clone()));
    super::super::super::catalog_propose::propose_and_apply(state, &entry)?;

    // Broadcast to connected Lite sessions after the catalog commit is durable.
    emit_procedure_put(state, &stored);

    state.audit_record(
        crate::control::security::audit::AuditEvent::AdminAction,
        Some(identity.tenant_id),
        &identity.username,
        &format!("CREATE PROCEDURE {}", stored.name),
    );

    Ok(vec![Response::Execution(Tag::new("CREATE PROCEDURE"))])
}

/// Encode the stored procedure and broadcast a `DefinitionSyncMsg` to all
/// connected Lite sessions after the catalog commit is durable.
fn emit_procedure_put(
    state: &crate::control::state::SharedState,
    stored: &crate::control::security::catalog::procedure_types::StoredProcedure,
) {
    use nodedb_types::sync::wire::DefinitionSyncMsg;

    let lite_params: Vec<serde_json::Value> = stored
        .parameters
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "data_type": p.data_type,
                "direction": p.direction.as_str(),
            })
        })
        .collect();

    let payload_json = serde_json::json!({
        "name": stored.name,
        "parameters": lite_params,
        "body_sql": stored.body_sql,
        "max_iterations": stored.max_iterations,
        "timeout_secs": stored.timeout_secs,
        "owner": stored.owner,
        "created_at": stored.created_at,
    });

    match sonic_rs::to_vec(&payload_json) {
        Ok(payload) => {
            let msg = DefinitionSyncMsg {
                definition_type: "procedure".into(),
                name: stored.name.clone(),
                action: "put".into(),
                payload,
            };
            state.definition_sync_fanout.broadcast(&msg);
        }
        Err(e) => {
            tracing::warn!(
                name = %stored.name,
                error = %e,
                "definition_sync: failed to serialize procedure payload; skipping broadcast"
            );
        }
    }
}
