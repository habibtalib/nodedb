// SPDX-License-Identifier: BUSL-1.1

//! `ALTER SCHEDULE` DDL handler.
//!
//! Supports: ENABLE, DISABLE, SET CRON 'expr'.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;
use crate::event::scheduler::cron::CronExpr;

use super::super::super::types::sqlstate_error;

/// Handle `ALTER SCHEDULE <name> ENABLE | DISABLE | SET CRON '<expr>'`.
///
/// `name`, `action`, and `cron_expr` come from the typed
/// [`NodedbStatement::AlterSchedule`] variant.
pub fn alter_schedule(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    action: &str,
    cron_expr: Option<&str>,
) -> PgWireResult<Vec<Response>> {
    let tenant_id = identity.tenant_id.as_u64();

    // Look up the schedule in the registry.
    let mut def = state
        .schedule_registry
        .get(tenant_id, name)
        .ok_or_else(|| sqlstate_error("42704", &format!("schedule \"{name}\" does not exist")))?;

    match action {
        "ENABLE" => {
            def.enabled = true;
        }
        "DISABLE" => {
            def.enabled = false;
        }
        "SET" => {
            let new_cron = cron_expr.ok_or_else(|| {
                sqlstate_error(
                    "42601",
                    "ALTER SCHEDULE SET CRON requires a quoted cron expression",
                )
            })?;

            CronExpr::parse(new_cron)
                .map_err(|e| sqlstate_error("22023", &format!("invalid cron expression: {e}")))?;

            def.cron_expr = new_cron.to_string();
        }
        _ => {
            return Err(sqlstate_error(
                "42601",
                "ALTER SCHEDULE supports: ENABLE, DISABLE, SET CRON 'expr'",
            ));
        }
    }

    // Persist the updated definition through the same metadata-raft
    // propose path every other parent-replicated ALTER uses, so a
    // cluster deployment converges on the new state cluster-wide and
    // the single-node fallback writes both the primary row and its
    // OWNERS row. The earlier direct `catalog.put_schedule(&def)`
    // call did neither — divergence on replicas, orphan on disk.
    let entry = crate::control::catalog_entry::CatalogEntry::PutSchedule(Box::new(def.clone()));
    super::super::catalog_propose::propose_and_apply(state, &entry)?;

    // Update in-memory registry.
    state.schedule_registry.update(def);

    Ok(vec![Response::Execution(Tag::new("ALTER SCHEDULE"))])
}
