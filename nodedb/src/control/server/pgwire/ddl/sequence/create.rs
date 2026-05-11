// SPDX-License-Identifier: BUSL-1.1

//! `CREATE SEQUENCE` handler.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::catalog::sequence_types::StoredSequence;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::sqlstate_error;

/// Handle `CREATE [IF NOT EXISTS] SEQUENCE <name> [options…]`.
///
/// All option fields are pre-parsed by the `nodedb-sql` AST layer.
/// `format_template_raw` and `reset_period_raw` are finalised here
/// because those parsers live in the `nodedb` crate (not `nodedb-sql`).
#[allow(clippy::too_many_arguments)]
pub fn create_sequence(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    start: Option<i64>,
    increment: Option<i64>,
    min_value: Option<i64>,
    max_value: Option<i64>,
    cycle: bool,
    cache: Option<i64>,
    format_template_raw: Option<&str>,
    reset_period_raw: Option<&str>,
    gap_free: bool,
    _scope: Option<&str>,
) -> PgWireResult<Vec<Response>> {
    let tenant_id = identity.tenant_id.as_u64();

    let mut def = StoredSequence::new(tenant_id, name.to_string(), identity.username.clone());

    if let Some(s) = start {
        def.start_value = s;
    }
    if let Some(inc) = increment {
        def.increment = inc;
    }
    if let Some(min) = min_value {
        def.min_value = min;
    }
    if let Some(max) = max_value {
        def.max_value = max;
    }
    def.cycle = cycle;
    if let Some(c) = cache {
        def.cache_size = c;
    }
    if let Some(fmt) = format_template_raw {
        let tokens = crate::control::sequence::format::parse_format_template(fmt)
            .map_err(|e| sqlstate_error("42601", &format!("invalid FORMAT: {e}")))?;
        def.format_template = Some(tokens);
    }
    if let Some(reset) = reset_period_raw {
        def.reset_scope = crate::control::sequence::format::ResetScope::parse(reset)
            .map_err(|e| sqlstate_error("42601", &e.to_string()))?;
    }
    def.gap_free = gap_free;

    // Apply defaults for descending sequences.
    if def.increment < 0 && def.min_value == 1 && def.max_value == i64::MAX {
        def.max_value = -1;
        def.min_value = i64::MIN;
        if def.start_value == 1 {
            def.start_value = -1;
        }
    }

    def.created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    def.validate()
        .map_err(|e| sqlstate_error("42P17", &e.to_string()))?;

    if state.sequence_registry.exists(tenant_id, &def.name) {
        return Err(sqlstate_error(
            "42P07",
            &format!("sequence \"{}\" already exists", def.name),
        ));
    }

    let entry = crate::control::catalog_entry::CatalogEntry::PutSequence(Box::new(def.clone()));
    let log_index = super::super::catalog_propose::propose_and_apply(state, &entry)?;
    if log_index == 0 {
        state
            .sequence_registry
            .create(def)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
    }

    state.schema_version.bump();

    Ok(vec![Response::Execution(Tag::new("CREATE SEQUENCE"))])
}
