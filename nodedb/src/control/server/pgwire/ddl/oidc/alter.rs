// SPDX-License-Identifier: BUSL-1.1

//! Handler for `ALTER OIDC PROVIDER <name> SET CLAIM MAPPING WHEN ...`.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::catalog_entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::audit::AuditEvent;
use crate::control::security::catalog::oidc_providers::StoredClaimMappingRule;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;
use nodedb_sql::ddl_ast::statement::OidcClaimMappingClause;

use super::super::super::types::{require_superuser, sqlstate_error};

/// Handle `ALTER OIDC PROVIDER <name> SET CLAIM MAPPING WHEN ...`.
///
/// Replaces the entire claim-mapping list for the named provider.
pub async fn alter_oidc_provider_claim_mapping(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    claim_mappings: &[OidcClaimMappingClause],
) -> PgWireResult<Vec<Response>> {
    require_superuser(state, identity, None, "alter OIDC providers")?;

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog not available"))?;

    let mut provider = catalog
        .get_oidc_provider(name)
        .map_err(|e| sqlstate_error("XX000", &format!("catalog read: {e}")))?
        .ok_or_else(|| {
            sqlstate_error("42704", &format!("OIDC provider '{name}' does not exist"))
        })?;

    let stored_mappings: Vec<StoredClaimMappingRule> = claim_mappings
        .iter()
        .map(|cm| StoredClaimMappingRule {
            claim_name: cm.claim_name.clone(),
            claim_value: cm.claim_value.clone(),
            default_database: cm.default_database,
            add_databases: cm.add_databases.clone(),
            add_roles: cm.add_roles.clone(),
        })
        .collect();

    provider.claim_mapping = stored_mappings;

    let entry = CatalogEntry::PutOidcProvider(Box::new(provider.clone()));
    let log_index = propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    if log_index == 0 {
        catalog
            .put_oidc_provider(&provider)
            .map_err(|e| sqlstate_error("XX000", &format!("catalog write: {e}")))?;
    }

    state.audit_record(
        AuditEvent::OidcProviderChanged,
        Some(identity.tenant_id),
        &identity.username,
        &format!(
            "ALTER OIDC PROVIDER {name} SET CLAIM MAPPING ({} rules)",
            provider.claim_mapping.len()
        ),
    );

    Ok(vec![Response::Execution(Tag::new("ALTER OIDC PROVIDER"))])
}
