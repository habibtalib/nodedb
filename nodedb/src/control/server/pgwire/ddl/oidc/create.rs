// SPDX-License-Identifier: BUSL-1.1

//! Handler for `CREATE OIDC PROVIDER`.

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::catalog_entry::CatalogEntry;
use crate::control::metadata_proposer::propose_catalog_entry;
use crate::control::security::audit::AuditEvent;
use crate::control::security::catalog::StoredOidcProvider;
use crate::control::security::catalog::oidc_providers::StoredClaimMappingRule;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;
use nodedb_sql::ddl_ast::statement::OidcClaimMappingClause;

use super::super::super::types::{require_superuser, sqlstate_error};

/// Handle `CREATE OIDC PROVIDER <name> ISSUER '<iss>' JWKS_URI '<uri>' ...`.
pub async fn create_oidc_provider(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    issuer: &str,
    jwks_uri: &str,
    audience: Option<&str>,
    claim_mappings: &[OidcClaimMappingClause],
) -> PgWireResult<Vec<Response>> {
    require_superuser(state, identity, None, "create OIDC providers")?;

    if issuer.is_empty() {
        return Err(sqlstate_error("22023", "ISSUER must not be empty"));
    }
    if jwks_uri.is_empty() {
        return Err(sqlstate_error("22023", "JWKS_URI must not be empty"));
    }

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog not available"))?;

    // Check for duplicate by provider name.
    match catalog.get_oidc_provider(name) {
        Ok(Some(_)) => {
            return Err(sqlstate_error(
                "42710",
                &format!("OIDC provider '{name}' already exists"),
            ));
        }
        Ok(None) => {}
        Err(e) => {
            return Err(sqlstate_error("XX000", &format!("catalog read: {e}")));
        }
    }

    // Check for duplicate issuer (one issuer → one provider).
    match catalog.list_oidc_providers() {
        Ok(providers) => {
            if providers.iter().any(|p| p.issuer == issuer) {
                return Err(sqlstate_error(
                    "42710",
                    &format!("OIDC provider with issuer '{issuer}' already exists"),
                ));
            }
        }
        Err(e) => {
            return Err(sqlstate_error("XX000", &format!("catalog list: {e}")));
        }
    }

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

    let provider = StoredOidcProvider {
        provider_name: name.to_string(),
        issuer: issuer.to_string(),
        jwks_uri: jwks_uri.to_string(),
        audience: audience.map(str::to_string),
        claim_mapping: stored_mappings,
        created_at_lsn: 0,
    };

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
        &format!("CREATE OIDC PROVIDER {name} issuer={issuer}"),
    );

    Ok(vec![Response::Execution(Tag::new("CREATE OIDC PROVIDER"))])
}
