// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use futures::stream;
use nodedb_types::id::DatabaseId;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response, Tag};
use pgwire::error::PgWireResult;
use smallvec::SmallVec;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::{AuthenticatedIdentity, DatabaseSet};
use crate::control::state::SharedState;

use super::super::types::{int8_field, require_tenant_admin, sqlstate_error, text_field};

/// CREATE API KEY FOR <user> [EXPIRES <seconds>] [WITH SCOPES ...] [WITH DATABASES (db1, db2)]
///
/// Returns the full API key (shown once). Requires admin or self.
pub fn create_api_key(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if parts.len() < 5 {
        return Err(sqlstate_error(
            "42601",
            "syntax: CREATE API KEY FOR <user> [EXPIRES <seconds>] [WITH DATABASES (db1, db2)]",
        ));
    }

    if !parts[1].eq_ignore_ascii_case("API")
        || !parts[2].eq_ignore_ascii_case("KEY")
        || !parts[3].eq_ignore_ascii_case("FOR")
    {
        return Err(sqlstate_error(
            "42601",
            "syntax: CREATE API KEY FOR <user> [EXPIRES <seconds>] [WITH DATABASES (db1, db2)]",
        ));
    }

    let target_username = parts[4];

    // Users can create keys for themselves; admin required for others.
    if target_username != identity.username {
        require_tenant_admin(identity, "create API keys for other users")?;
    }

    // Look up the target user.
    let target_user = state
        .credentials
        .get_user(target_username)
        .ok_or_else(|| sqlstate_error("42704", &format!("user '{target_username}' not found")))?;

    // Parse optional EXPIRES.
    let mut expires_secs: u64 = 0;
    if let Some(expires_idx) = parts.iter().position(|p| p.eq_ignore_ascii_case("EXPIRES"))
        && let Some(secs_str) = parts.get(expires_idx + 1)
    {
        expires_secs = secs_str
            .parse()
            .map_err(|_| sqlstate_error("42601", "EXPIRES must be a number of seconds"))?;
    }

    // Parse optional WITH SCOPES.
    let key_scopes = parse_key_scopes(parts, state)?;

    // Parse optional WITH DATABASES (db1, db2, ...).
    let requested_db_ids = parse_with_databases(parts, state)?;

    // Build owner_set for subset validation at CREATE time.
    let owner_set = build_owner_database_set_for_user(state, &target_user)?;

    // Validate: requested set must be ⊆ owner_set.
    let accessible_databases = match requested_db_ids {
        None => {
            // No WITH DATABASES clause: inherit owner at bind time.
            vec![]
        }
        Some(ids) => {
            for &db_id in &ids {
                if !owner_set.contains(db_id) {
                    let db_name = state
                        .credentials
                        .catalog()
                        .as_ref()
                        .and_then(|cat| cat.get_database_name_by_id(db_id).ok().flatten())
                        .unwrap_or_else(|| format!("<id:{}>", db_id.as_u64()));
                    return Err(sqlstate_error(
                        "42501",
                        &format!(
                            "permission denied: API key cannot have wider access than owner; \
                             database '{db_name}' not in owner's set"
                        ),
                    ));
                }
            }
            ids
        }
    };

    // Build the `StoredApiKey` on the proposer — generates key_id +
    // secret + SHA-256 hash. Only the returned `token` contains the
    // plaintext secret (shown once to the client). The hashed record
    // replicates through raft; every node's applier writes redb +
    // installs the record into the in-memory cache.
    let (stored, token) =
        state
            .api_keys
            .prepare_key(crate::control::security::apikey::CreateKeyParams {
                username: target_username,
                user_id: target_user.user_id,
                tenant_id: target_user.tenant_id,
                expires_secs,
                scope: key_scopes,
                accessible_databases,
            });
    let entry = crate::control::catalog_entry::CatalogEntry::PutApiKey(Box::new(stored.clone()));
    let log_index = crate::control::metadata_proposer::propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    if log_index == 0
        && let Some(catalog) = state.credentials.catalog()
    {
        catalog
            .put_api_key(&stored)
            .map_err(|e| sqlstate_error("XX000", &format!("catalog write: {e}")))?;
        state.api_keys.install_replicated_key(&stored);
    }

    state.audit_record(
        AuditEvent::PrivilegeChange,
        Some(identity.tenant_id),
        &identity.username,
        &format!("created API key for user '{target_username}'"),
    );

    // Return the token as a query result (shown once).
    let schema = Arc::new(vec![text_field("api_key")]);
    let mut encoder = DataRowEncoder::new(schema.clone());
    encoder
        .encode_field(&token)
        .map_err(|e| sqlstate_error("XX000", &format!("encode error: {e}")))?;
    let row = encoder.take_row();

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(vec![Ok(row)]),
    ))])
}

/// REVOKE API KEY <key_id>
pub fn revoke_api_key(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if parts.len() < 4 {
        return Err(sqlstate_error("42601", "syntax: REVOKE API KEY <key_id>"));
    }

    if !parts[1].eq_ignore_ascii_case("API") || !parts[2].eq_ignore_ascii_case("KEY") {
        return Err(sqlstate_error("42601", "syntax: REVOKE API KEY <key_id>"));
    }

    let key_id = parts[3];

    // Check if the key belongs to the current user or if they're admin.
    let keys = state.api_keys.list_keys_for_user(&identity.username);
    let owns_key = keys.iter().any(|k| k.key_id == key_id);
    if !owns_key {
        require_tenant_admin(identity, "revoke API keys for other users")?;
    }

    // Pre-check existence locally so "key not found" doesn't touch raft.
    let exists_before = state.api_keys.get_key(key_id).is_some();
    if !exists_before {
        return Err(sqlstate_error(
            "42704",
            &format!("API key '{key_id}' not found"),
        ));
    }

    let entry = crate::control::catalog_entry::CatalogEntry::RevokeApiKey {
        key_id: key_id.to_string(),
    };
    let log_index = crate::control::metadata_proposer::propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    let revoked = if log_index == 0 {
        let catalog = state.credentials.catalog();
        state
            .api_keys
            .revoke_key(key_id, catalog.as_ref())
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?
    } else {
        // Cluster mode: trust the committed log index — the
        // in-memory cache update runs in a spawned tokio task and
        // may not be visible yet.
        true
    };

    if revoked {
        state.audit_record(
            AuditEvent::PrivilegeChange,
            Some(identity.tenant_id),
            &identity.username,
            &format!("revoked API key '{key_id}'"),
        );
        Ok(vec![Response::Execution(Tag::new("REVOKE API KEY"))])
    } else {
        Err(sqlstate_error(
            "42704",
            &format!("API key '{key_id}' not found"),
        ))
    }
}

/// LIST API KEYS [FOR <user>] / SHOW API KEYS [FOR <user>]
pub fn list_api_keys(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    // Normalise: both LIST and SHOW lead here; skip the command verb at parts[0]
    // and the "API KEYS" at parts[1..2]; optionally "FOR <user>" at parts[3..4].
    let target_username = if parts.len() >= 5 && parts[3].eq_ignore_ascii_case("FOR") {
        let target = parts[4];
        if target != identity.username {
            require_tenant_admin(identity, "list API keys for other users")?;
        }
        target.to_string()
    } else if parts.len() >= 4 && parts[3].eq_ignore_ascii_case("FOR") {
        return Err(sqlstate_error("42601", "expected username after FOR"));
    } else {
        // Default: list own keys (or all if superuser).
        identity.username.clone()
    };

    let keys = if identity.is_superuser && target_username == identity.username {
        state.api_keys.list_all_keys()
    } else {
        state.api_keys.list_keys_for_user(&target_username)
    };

    let schema = Arc::new(vec![
        text_field("key_id"),
        text_field("username"),
        int8_field("expires_at"),
        text_field("is_revoked"),
        text_field("databases"),
        int8_field("created_at"),
    ]);

    let mut rows = Vec::with_capacity(keys.len());
    let mut encoder = DataRowEncoder::new(schema.clone());

    for key in &keys {
        encoder
            .encode_field(&key.key_id)
            .map_err(|e| sqlstate_error("XX000", &format!("encode error: {e}")))?;
        encoder
            .encode_field(&key.username)
            .map_err(|e| sqlstate_error("XX000", &format!("encode error: {e}")))?;
        encoder
            .encode_field(&(key.expires_at as i64))
            .map_err(|e| sqlstate_error("XX000", &format!("encode error: {e}")))?;
        encoder
            .encode_field(&if key.is_revoked { "t" } else { "f" })
            .map_err(|e| sqlstate_error("XX000", &format!("encode error: {e}")))?;

        // Render the database access column.
        let db_display = if key.accessible_databases.is_empty() {
            "(inherit)".to_string()
        } else {
            let names: Vec<String> = key
                .accessible_databases
                .iter()
                .map(|&db_id| {
                    state
                        .credentials
                        .catalog()
                        .as_ref()
                        .and_then(|cat| cat.get_database_name_by_id(db_id).ok().flatten())
                        .unwrap_or_else(|| format!("<id:{}>", db_id.as_u64()))
                })
                .collect();
            names.join(",")
        };
        encoder
            .encode_field(&db_display)
            .map_err(|e| sqlstate_error("XX000", &format!("encode error: {e}")))?;

        encoder
            .encode_field(&(key.created_at as i64))
            .map_err(|e| sqlstate_error("XX000", &format!("encode error: {e}")))?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// Parse `WITH SCOPES 'scope1', 'scope2'` from DDL parts.
/// Resolves scope names via ScopeStore to (permission, collection) pairs.
/// Terminators: EXPIRES, DATABASES (or end of tokens).
fn parse_key_scopes(
    parts: &[&str],
    state: &SharedState,
) -> PgWireResult<Vec<crate::control::security::apikey::KeyScope>> {
    let scopes_idx = parts.iter().position(|p| p.to_uppercase() == "SCOPES");
    let Some(idx) = scopes_idx else {
        return Ok(vec![]);
    };
    // Check preceding word is WITH.
    if idx == 0 || parts[idx - 1].to_uppercase() != "WITH" {
        return Ok(vec![]);
    }

    let scope_names: Vec<&str> = parts[idx + 1..]
        .iter()
        .take_while(|p| {
            let up = p.to_uppercase();
            !up.starts_with("EXPIRES") && !up.starts_with("DATABASES") && up != "WITH"
        })
        .map(|s| s.trim_matches('\'').trim_end_matches(','))
        .collect();

    let mut key_scopes = Vec::new();
    for scope_name in &scope_names {
        let resolved = state.scope_defs.resolve(scope_name);
        if resolved.is_empty() {
            return Err(sqlstate_error(
                "42704",
                &format!("scope '{scope_name}' not found or empty"),
            ));
        }
        for (perm, coll) in resolved {
            key_scopes.push(crate::control::security::apikey::KeyScope {
                permission: perm,
                collection: coll,
            });
        }
    }

    Ok(key_scopes)
}

/// Parse `WITH DATABASES (db1, db2)` or `WITH DATABASES db1, db2` from DDL parts.
///
/// Returns `None` if the clause is absent (signals "inherit owner").
/// Returns `Some(vec![...])` with resolved `DatabaseId`s if present.
/// Rejects unknown database names with SQLSTATE `42704`.
fn parse_with_databases(
    parts: &[&str],
    state: &SharedState,
) -> PgWireResult<Option<Vec<DatabaseId>>> {
    let db_idx = parts.iter().position(|p| p.to_uppercase() == "DATABASES");
    let Some(idx) = db_idx else {
        return Ok(None);
    };
    // Check preceding word is WITH.
    if idx == 0 || parts[idx - 1].to_uppercase() != "WITH" {
        return Ok(None);
    }

    // Collect comma-separated names until EXPIRES or end-of-tokens.
    // Strip surrounding parens if present.
    let raw_names: Vec<&str> = parts[idx + 1..]
        .iter()
        .take_while(|p| !p.to_uppercase().starts_with("EXPIRES"))
        .map(|s| {
            s.trim_start_matches('(')
                .trim_end_matches(')')
                .trim_end_matches(',')
        })
        .filter(|s| !s.is_empty())
        .collect();

    if raw_names.is_empty() {
        return Err(sqlstate_error(
            "42601",
            "WITH DATABASES requires at least one database name",
        ));
    }

    let catalog = state.credentials.catalog();
    let mut ids = Vec::with_capacity(raw_names.len());
    for name in raw_names {
        // catalog: Option<Arc<SystemCatalog>>
        // map produces: Option<Result<Option<DatabaseId>>>
        // transpose: Result<Option<Option<DatabaseId>>>
        // ? + flatten: Option<DatabaseId>
        let resolved: Option<DatabaseId> = catalog
            .as_ref()
            .map(|cat| cat.get_database_id_by_name(name))
            .transpose()
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?
            .flatten();
        match resolved {
            Some(id) => ids.push(id),
            None => {
                return Err(sqlstate_error(
                    "42704",
                    &format!("database '{name}' not found"),
                ));
            }
        }
    }

    Ok(Some(ids))
}

/// Build the owner's `DatabaseSet` from a `UserRecord` for CREATE-time subset validation.
fn build_owner_database_set_for_user(
    state: &SharedState,
    user: &crate::control::security::credential::record::UserRecord,
) -> PgWireResult<DatabaseSet> {
    if user.is_superuser {
        return Ok(DatabaseSet::All);
    }
    if user.is_service_account && !user.accessible_databases.is_empty() {
        return Ok(DatabaseSet::Some(SmallVec::from_iter(
            user.accessible_databases.iter().copied(),
        )));
    }
    // Regular user or legacy service account: read from database_grants.
    let db_ids = state
        .credentials
        .catalog()
        .as_ref()
        .map(|cat| cat.list_user_grant_databases(user.user_id))
        .transpose()
        .map_err(|e| sqlstate_error("XX000", &e.to_string()))?
        .unwrap_or_else(|| vec![DatabaseId::DEFAULT]);
    Ok(DatabaseSet::Some(SmallVec::from_iter(db_ids)))
}
