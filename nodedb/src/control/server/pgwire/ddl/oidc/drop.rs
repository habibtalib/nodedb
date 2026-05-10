// SPDX-License-Identifier: BUSL-1.1

//! Handler for `DROP OIDC PROVIDER [IF EXISTS] <name>`.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::catalog_entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_superuser, sqlstate_error};

/// Handle `DROP OIDC PROVIDER [IF EXISTS] <name>`.
pub async fn drop_oidc_provider(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    if_exists: bool,
) -> PgWireResult<Vec<Response>> {
    require_superuser(state, identity, None, "drop OIDC providers")?;

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog not available"))?;

    if catalog
        .get_oidc_provider(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog read: {e}")))?
        .is_none()
    {
        if if_exists {
            return Ok(vec![Response::Execution(Tag::new("DROP OIDC PROVIDER"))]);
        }
        return Err(sqlstate_error(
            "42704",
            &format!("OIDC provider '{name}' does not exist"),
        ));
    }

    let entry = CatalogEntry::DeleteOidcProvider {
        name: name.to_string(),
    };
    let log_index = propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    if log_index == 0 {
        catalog
            .delete_oidc_provider(name)
            .map_err(|e| sqlstate_error("XX000", &format!("catalog delete: {e}")))?;
    }

    state.audit_record(
        AuditEvent::OidcProviderChanged,
        Some(identity.tenant_id),
        &identity.username,
        &format!("DROP OIDC PROVIDER {name}"),
    );

    Ok(vec![Response::Execution(Tag::new("DROP OIDC PROVIDER"))])
}
