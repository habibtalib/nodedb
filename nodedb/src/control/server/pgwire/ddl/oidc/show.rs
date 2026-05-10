// SPDX-License-Identifier: BUSL-1.1

//! Handler for `SHOW OIDC PROVIDERS`.

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::super::types::{require_superuser, sqlstate_error, text_field};

/// Handle `SHOW OIDC PROVIDERS`.
pub fn show_oidc_providers(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    require_superuser(state, identity, None, "show OIDC providers")?;

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog not available"))?;

    let providers = catalog
        .list_oidc_providers()
        .map_err(|e| sqlstate_error("XX000", &format!("catalog list: {e}")))?;

    let schema = Arc::new(vec![
        text_field("name"),
        text_field("issuer"),
        text_field("jwks_uri"),
        text_field("audience"),
        text_field("claim_mapping_rules"),
    ]);

    let mut rows = Vec::new();
    for p in &providers {
        let mut enc = DataRowEncoder::new(schema.clone());
        enc.encode_field(&p.provider_name)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        enc.encode_field(&p.issuer)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        enc.encode_field(&p.jwks_uri)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        let aud = p.audience.as_deref().unwrap_or("");
        enc.encode_field(&aud)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        let rule_count = p.claim_mapping.len().to_string();
        enc.encode_field(&rule_count)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        rows.push(Ok(enc.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}
