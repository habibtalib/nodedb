// SPDX-License-Identifier: BUSL-1.1

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::audit::AuditEvent;
use crate::control::security::identity::{AuthenticatedIdentity, Role};
use crate::control::state::SharedState;

use super::super::types::{parse_role, require_tenant_admin, sqlstate_error};

/// CREATE SERVICE ACCOUNT <name> [ROLE <role>] [TENANT <id>]
///                                [FOR DATABASE <db>]
///                                [FOR TENANT <id> IN DATABASE <db>]
///
/// Creates a service account — a non-interactive identity that can only
/// authenticate via API keys. No password, no pgwire login.
pub fn create_service_account(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "create service accounts")?;

    if parts.len() < 4 {
        return Err(sqlstate_error(
            "42601",
            "syntax: CREATE SERVICE ACCOUNT <name> [ROLE <role>] [FOR DATABASE <db>]",
        ));
    }

    let name = parts[3];

    // Parse optional ROLE, TENANT, FOR DATABASE / IN DATABASE.
    let mut role = Role::ReadWrite;
    let mut tenant_id = identity.tenant_id;
    let mut accessible_databases: Vec<nodedb_types::id::DatabaseId> = vec![];
    let mut seen_for_database = false;
    let mut seen_for_tenant = false;
    let mut i = 4;
    while i < parts.len() {
        let up = parts[i].to_uppercase();
        match up.as_str() {
            "ROLE" if i + 1 < parts.len() => {
                role = parse_role(parts[i + 1]);
                i += 2;
            }
            "TENANT" if i + 1 < parts.len() => {
                if !identity.is_superuser {
                    return Err(sqlstate_error("42501", "only superuser can assign tenants"));
                }
                let tid: u64 = parts[i + 1]
                    .parse()
                    .map_err(|_| sqlstate_error("42601", "TENANT must be a numeric ID"))?;
                tenant_id = crate::types::TenantId::new(tid);
                seen_for_tenant = true;
                i += 2;
            }
            "FOR" if i + 1 < parts.len() => {
                let next_up = parts[i + 1].to_uppercase();
                match next_up.as_str() {
                    "DATABASE" if i + 2 < parts.len() => {
                        let db_name = parts[i + 2];
                        let db_id = resolve_database(state, db_name)?;
                        accessible_databases = vec![db_id];
                        seen_for_database = true;
                        i += 3;
                    }
                    "TENANT" if i + 2 < parts.len() => {
                        // FOR TENANT <id> IN DATABASE <db> — superuser only.
                        if !identity.is_superuser {
                            return Err(sqlstate_error(
                                "42501",
                                "only superuser can use FOR TENANT ... IN DATABASE",
                            ));
                        }
                        let tid: u64 = parts[i + 2]
                            .parse()
                            .map_err(|_| sqlstate_error("42601", "TENANT must be a numeric ID"))?;
                        tenant_id = crate::types::TenantId::new(tid);
                        seen_for_tenant = true;
                        i += 3;
                        // Expect IN DATABASE <db> immediately after.
                        if i + 2 < parts.len()
                            && parts[i].to_uppercase() == "IN"
                            && parts[i + 1].to_uppercase() == "DATABASE"
                        {
                            let db_name = parts[i + 2];
                            let db_id = resolve_database(state, db_name)?;
                            accessible_databases = vec![db_id];
                            seen_for_database = true;
                            i += 3;
                        } else {
                            return Err(sqlstate_error(
                                "42601",
                                "FOR TENANT ... must be followed by IN DATABASE <name>",
                            ));
                        }
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            "IN" if i + 2 < parts.len() && parts[i + 1].to_uppercase() == "DATABASE" => {
                let db_name = parts[i + 2];
                let db_id = resolve_database(state, db_name)?;
                accessible_databases = vec![db_id];
                seen_for_database = true;
                i += 3;
            }
            _ => {
                i += 1;
            }
        }
    }

    // FOR TENANT without IN DATABASE is a syntax error.
    if seen_for_tenant && !seen_for_database && !accessible_databases.is_empty() {
        // already set — fine
    } else if seen_for_tenant && !seen_for_database && accessible_databases.is_empty() {
        // If FOR TENANT was used standalone (old form), that's allowed for backwards compat.
        // Only reject when user explicitly wrote FOR TENANT ... (without IN DATABASE in
        // the new sense) after the new parser added the requirement above, which already
        // returns an error inline. So no additional check needed here.
    }
    let _ = seen_for_tenant; // suppress unused warning

    state
        .credentials
        .create_service_account(name, tenant_id, vec![role], accessible_databases)
        .map_err(|e| sqlstate_error("42710", &e.to_string()))?;

    state.audit_record(
        AuditEvent::PrivilegeChange,
        Some(tenant_id),
        &identity.username,
        &format!("created service account '{name}' in tenant {tenant_id}"),
    );

    Ok(vec![Response::Execution(Tag::new(
        "CREATE SERVICE ACCOUNT",
    ))])
}

/// DROP SERVICE ACCOUNT <name>
pub fn drop_service_account(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    require_tenant_admin(identity, "drop service accounts")?;

    if parts.len() < 4 {
        return Err(sqlstate_error(
            "42601",
            "syntax: DROP SERVICE ACCOUNT <name>",
        ));
    }

    let name = parts[3];

    // Verify it's actually a service account.
    let user = state
        .credentials
        .get_user(name)
        .ok_or_else(|| sqlstate_error("42704", &format!("service account '{name}' not found")))?;
    if !user.is_service_account {
        return Err(sqlstate_error(
            "42809",
            &format!("'{name}' is a user, not a service account. Use DROP USER instead."),
        ));
    }

    let dropped = state
        .credentials
        .deactivate_user(name)
        .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;

    if dropped {
        state.audit_record(
            AuditEvent::PrivilegeChange,
            Some(identity.tenant_id),
            &identity.username,
            &format!("dropped service account '{name}'"),
        );
        Ok(vec![Response::Execution(Tag::new("DROP SERVICE ACCOUNT"))])
    } else {
        Err(sqlstate_error(
            "42704",
            &format!("service account '{name}' not found"),
        ))
    }
}

/// Resolve a database name to its `DatabaseId`, returning a pgwire error if not found.
fn resolve_database(state: &SharedState, name: &str) -> PgWireResult<nodedb_types::id::DatabaseId> {
    let catalog = state.credentials.catalog();
    // catalog: Option<Arc<SystemCatalog>>
    // map: Option<Result<Option<DatabaseId>>>
    // transpose: Result<Option<Option<DatabaseId>>>
    // ? + flatten: Option<DatabaseId>
    let resolved: Option<nodedb_types::id::DatabaseId> = catalog
        .as_ref()
        .map(|cat| cat.get_database_id_by_name(name))
        .transpose()
        .map_err(|e| sqlstate_error("XX000", &e.to_string()))?
        .flatten();
    resolved.ok_or_else(|| sqlstate_error("42704", &format!("database '{name}' not found")))
}
