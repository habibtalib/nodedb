// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::types::{int8_field, sqlstate_error, text_field};

// Re-export audit SHOW functions so callers reference `inspect::show_audit_log` etc.
pub use super::inspect_audit::{
    export_audit_log, show_audit_in_database, show_audit_log, show_audit_where,
};

/// SHOW USERS — list all active users.
///
/// Superuser sees all users. Tenant admin sees users in their tenant.
pub fn show_users(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    let schema = Arc::new(vec![
        text_field("username"),
        int8_field("tenant_id"),
        text_field("roles"),
        text_field("is_superuser"),
    ]);

    let users = state.credentials.list_user_details();
    let mut rows = Vec::new();
    let mut encoder = DataRowEncoder::new(schema.clone());

    for user in &users {
        // Filter: superuser sees all, tenant_admin sees own tenant only.
        if !identity.is_superuser && user.tenant_id != identity.tenant_id {
            continue;
        }

        encoder.encode_field(&user.username)?;
        encoder.encode_field(&(user.tenant_id.as_u64() as i64))?;
        let roles_str: String = user
            .roles
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        encoder.encode_field(&roles_str)?;
        encoder.encode_field(&if user.is_superuser { "t" } else { "f" })?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// SHOW TENANTS — list all tenants with quotas.
///
/// Superuser only.
pub fn show_tenants(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: only superuser can list tenants",
        ));
    }

    let (schema, rows) = tenant_rows(state, |_, _| true)?;
    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// SHOW TENANT <name|id> — single-tenant introspection by identifier.
///
/// Resolves `ident` first as a numeric tenant id, then as a name. The
/// row shape mirrors `SHOW TENANTS` (tenant_id, name, active_requests,
/// total_requests, rejected_requests). Returns SQLSTATE `42704`
/// (undefined_object) if no tenant matches. Superuser only.
pub fn show_tenant_by_identifier(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    ident: &str,
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: only superuser can introspect tenants",
        ));
    }

    let (schema, rows) = tenant_rows(state, |t_id, t_name| {
        if let Ok(n) = ident.parse::<u64>() {
            t_id == n
        } else {
            t_name.eq_ignore_ascii_case(ident)
        }
    })?;

    if rows.is_empty() {
        return Err(sqlstate_error(
            "42704",
            &format!("tenant '{ident}' not found"),
        ));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// SHOW TENANTS WITH NAME <name> — filtered list form. Returns a row
/// set with the same schema as `SHOW TENANTS`. Returns SQLSTATE `42704`
/// when no tenant matches — silently returning the unfiltered list (the
/// pre-fix behaviour) would be a data-disclosure hazard.
pub fn show_tenants_filtered_by_name(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
) -> PgWireResult<Vec<Response>> {
    if !identity.is_superuser {
        return Err(sqlstate_error(
            "42501",
            "permission denied: only superuser can list tenants",
        ));
    }

    let (schema, rows) = tenant_rows(state, |_t_id, t_name| t_name.eq_ignore_ascii_case(name))?;

    if rows.is_empty() {
        return Err(sqlstate_error(
            "42704",
            &format!("tenant '{name}' not found"),
        ));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// Build the `(schema, rows)` pair shared by `SHOW TENANTS` and its
/// filtered variants. The predicate decides which (id, name) pairs are
/// emitted; the tenant set is the union of catalog-registered tenants
/// and any tenant that owns at least one user (usage is tracked on
/// first request, so a tenant with no traffic still needs to be listed).
type TenantRowSet = (
    Arc<Vec<pgwire::api::results::FieldInfo>>,
    Vec<PgWireResult<pgwire::messages::data::DataRow>>,
);

fn tenant_rows<F>(state: &SharedState, pred: F) -> PgWireResult<TenantRowSet>
where
    F: Fn(u64, &str) -> bool,
{
    let schema = Arc::new(vec![
        int8_field("tenant_id"),
        text_field("name"),
        int8_field("active_requests"),
        int8_field("total_requests"),
        int8_field("rejected_requests"),
    ]);

    let tenants = match state.tenants.lock() {
        Ok(t) => t,
        Err(p) => p.into_inner(),
    };

    let mut names: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    if let Some(catalog) = state.credentials.catalog()
        && let Ok(all) = catalog.load_all_tenants()
    {
        for t in all {
            names.insert(t.tenant_id, t.name);
        }
    }

    let mut seen: std::collections::BTreeSet<u64> = names.keys().copied().collect();
    for user in &state.credentials.list_user_details() {
        seen.insert(user.tenant_id.as_u64());
    }

    let mut rows = Vec::new();
    for tid_u64 in seen {
        let tid_name = names.get(&tid_u64).map(String::as_str).unwrap_or("");
        if !pred(tid_u64, tid_name) {
            continue;
        }
        let tid = crate::types::TenantId::new(tid_u64);
        let usage = tenants.usage(tid);
        let mut encoder = DataRowEncoder::new(schema.clone());
        encoder.encode_field(&(tid_u64 as i64))?;
        encoder.encode_field(&tid_name)?;
        encoder.encode_field(&(usage.map_or(0, |u| u.active_requests as i64)))?;
        encoder.encode_field(&(usage.map_or(0, |u| u.total_requests as i64)))?;
        encoder.encode_field(&(usage.map_or(0, |u| u.rejected_requests as i64)))?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok((schema, rows))
}

/// SHOW ROLES — list all custom roles. Built-in role enum is fixed
/// and not enumerated here; this lists the user-defined roles created
/// via `CREATE ROLE`.
///
/// Superuser sees all roles. Non-superusers see roles in their own
/// tenant only.
pub fn show_roles(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    let schema = Arc::new(vec![
        text_field("name"),
        int8_field("tenant_id"),
        text_field("parent"),
        int8_field("created_at"),
    ]);

    let roles = state.roles.list_roles();
    let mut rows = Vec::new();
    for role in &roles {
        if !identity.is_superuser && role.tenant_id != identity.tenant_id {
            continue;
        }
        let mut encoder = DataRowEncoder::new(schema.clone());
        encoder.encode_field(&role.name)?;
        encoder.encode_field(&(role.tenant_id.as_u64() as i64))?;
        encoder.encode_field(&role.parent.as_deref().unwrap_or(""))?;
        encoder.encode_field(&(role.created_at as i64))?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// SHOW SESSION — display current session identity.
pub fn show_session(identity: &AuthenticatedIdentity) -> PgWireResult<Vec<Response>> {
    let schema = Arc::new(vec![
        text_field("username"),
        int8_field("user_id"),
        int8_field("tenant_id"),
        text_field("roles"),
        text_field("auth_method"),
        text_field("is_superuser"),
    ]);

    let roles_str: String = identity
        .roles
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let auth_method = format!("{:?}", identity.auth_method);

    let mut encoder = DataRowEncoder::new(schema.clone());
    encoder.encode_field(&identity.username)?;
    encoder.encode_field(&(identity.user_id as i64))?;
    encoder.encode_field(&(identity.tenant_id.as_u64() as i64))?;
    encoder.encode_field(&roles_str)?;
    encoder.encode_field(&auth_method)?;
    encoder.encode_field(&if identity.is_superuser { "t" } else { "f" })?;

    let row = encoder.take_row();
    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(vec![Ok(row)]),
    ))])
}

/// SHOW GRANTS FOR <user>
pub fn show_grants(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    // SHOW GRANTS — show own grants
    // SHOW GRANTS FOR <user> — show another user's grants (admin only)
    let target_user = if parts.len() >= 4
        && parts[1].eq_ignore_ascii_case("GRANTS")
        && parts[2].eq_ignore_ascii_case("FOR")
    {
        let target = parts[3];
        if target != identity.username
            && !identity.is_superuser
            && !identity.has_role(&crate::control::security::identity::Role::TenantAdmin)
        {
            return Err(sqlstate_error(
                "42501",
                "permission denied: can only view your own grants, or be superuser/tenant_admin",
            ));
        }
        target.to_string()
    } else {
        identity.username.clone()
    };

    let schema = Arc::new(vec![text_field("username"), text_field("role")]);

    let user = state.credentials.get_user(&target_user);
    let mut rows = Vec::new();
    let mut encoder = DataRowEncoder::new(schema.clone());

    if let Some(user) = user {
        for role in &user.roles {
            encoder.encode_field(&user.username)?;
            encoder.encode_field(&role.to_string())?;
            rows.push(Ok(encoder.take_row()));
        }
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

/// `SHOW PERMISSIONS [ON <collection>] [FOR <user|role>]`
///
/// - `SHOW PERMISSIONS` — all grants visible to the caller
/// - `SHOW PERMISSIONS ON <collection>` — grants on a specific collection plus its owner
/// - `SHOW PERMISSIONS FOR <grantee>` — direct grants to a specific user or role
/// - `SHOW PERMISSIONS ON <collection> FOR <grantee>` — intersection of the above
///
/// For `FOR <role>` only direct grants are returned; inheritance is not walked
/// (`EXPLAIN PERMISSION` owns the resolved-privilege view).
pub fn show_permissions(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    on_collection: Option<&str>,
    for_grantee: Option<&str>,
) -> PgWireResult<Vec<Response>> {
    // Non-admins may only view their own grants.
    if let Some(grantee) = for_grantee
        && grantee != identity.username
        && !identity.is_superuser
        && !identity.has_role(&crate::control::security::identity::Role::TenantAdmin)
    {
        return Err(sqlstate_error(
            "42501",
            "permission denied: can only view your own permissions, or be superuser/tenant_admin",
        ));
    }

    let schema = Arc::new(vec![
        text_field("grantee"),
        text_field("permission"),
        text_field("target"),
        text_field("type"),
    ]);

    let mut rows = Vec::new();
    let mut encoder = DataRowEncoder::new(schema.clone());

    if let Some(collection) = on_collection {
        let target = format!("collection:{}:{collection}", identity.tenant_id.as_u64());

        // Show owner row (only when collection is specified).
        if for_grantee.is_none()
            && let Some(owner) =
                state
                    .permissions
                    .get_owner("collection", identity.tenant_id, collection)
        {
            encoder.encode_field(&owner)?;
            encoder.encode_field(&"ALL (owner)")?;
            encoder.encode_field(&collection)?;
            encoder.encode_field(&"ownership")?;
            rows.push(Ok(encoder.take_row()));
        }

        // Show explicit grants on this collection.
        let grants = state.permissions.grants_on(&target);
        for grant in &grants {
            if let Some(g) = for_grantee
                && !grant.grantee.eq_ignore_ascii_case(g)
            {
                continue;
            }
            encoder.encode_field(&grant.grantee)?;
            encoder.encode_field(&format!("{:?}", grant.permission))?;
            encoder.encode_field(&collection)?;
            encoder.encode_field(&"grant")?;
            rows.push(Ok(encoder.take_row()));
        }
    } else if let Some(grantee) = for_grantee {
        // All grants for a specific grantee (direct grants only, no inheritance walk).
        let grants = state.permissions.grants_for(grantee);
        for grant in &grants {
            // Extract a human-readable target from the internal target key
            // (e.g. "collection:1:users" → "users").
            let display_target = grant
                .target
                .rsplit(':')
                .next()
                .unwrap_or(&grant.target)
                .to_string();
            encoder.encode_field(&grant.grantee)?;
            encoder.encode_field(&format!("{:?}", grant.permission))?;
            encoder.encode_field(&display_target)?;
            encoder.encode_field(&"grant")?;
            rows.push(Ok(encoder.take_row()));
        }
    } else {
        // SHOW PERMISSIONS with no filter — show all grants for the current tenant.
        // Non-admins see only their own grants.
        let all_grants = if identity.is_superuser
            || identity.has_role(&crate::control::security::identity::Role::TenantAdmin)
        {
            state.permissions.all_grants(identity.tenant_id)
        } else {
            state.permissions.grants_for(&identity.username)
        };
        for grant in &all_grants {
            let display_target = grant
                .target
                .rsplit(':')
                .next()
                .unwrap_or(&grant.target)
                .to_string();
            encoder.encode_field(&grant.grantee)?;
            encoder.encode_field(&format!("{:?}", grant.permission))?;
            encoder.encode_field(&display_target)?;
            encoder.encode_field(&"grant")?;
            rows.push(Ok(encoder.take_row()));
        }
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}
