//! pgwire handlers for custom type DDL.
//!
//! - `CREATE TYPE <name> AS ENUM ('label1', 'label2', ...)`
//! - `CREATE TYPE <name> AS (<field1> <type1>, ...)`
//! - `DROP TYPE [IF EXISTS] <name>`
//! - `ALTER TYPE <name> ADD VALUE 'label'`
//! - `SHOW TYPES`
//!
//! Custom types are tenant-scoped. DROP TYPE is blocked when any collection
//! schema references the type. Each type receives a stable u32 OID from the
//! high-numbered range (70000+) so pgwire clients see a recognisable type.

use std::sync::Arc;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response, Tag};
use pgwire::error::PgWireResult;

use crate::control::security::catalog::{CompositeField, CustomTypeDef, StoredCustomType};
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;

use super::super::types::{require_admin, sqlstate_error, text_field};

/// Handle `CREATE TYPE <name> AS ENUM ('label1', ...)`.
pub async fn create_enum_type(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    labels: &[String],
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "create custom types")?;
    let tenant_id = identity.tenant_id.as_u64();

    if state.custom_type_registry.exists(tenant_id, name) {
        return Err(sqlstate_error(
            "42710",
            &format!("type '{name}' already exists"),
        ));
    }

    let oid = state.custom_type_registry.alloc_oid();
    let created_at = current_epoch_secs()?;
    let stored = StoredCustomType {
        tenant_id,
        name: name.to_string(),
        def: CustomTypeDef::Enum {
            labels: labels.to_vec(),
        },
        oid,
        created_at,
    };

    persist_and_register(state, stored).await?;

    Ok(vec![Response::Execution(Tag::new("CREATE TYPE"))])
}

/// Handle `CREATE TYPE <name> AS (<field1> <type1>, ...)`.
pub async fn create_composite_type(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    fields: &[(String, String)],
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "create custom types")?;
    let tenant_id = identity.tenant_id.as_u64();

    if state.custom_type_registry.exists(tenant_id, name) {
        return Err(sqlstate_error(
            "42710",
            &format!("type '{name}' already exists"),
        ));
    }

    let oid = state.custom_type_registry.alloc_oid();
    let created_at = current_epoch_secs()?;
    let composite_fields: Vec<CompositeField> = fields
        .iter()
        .map(|(n, t)| CompositeField {
            name: n.clone(),
            type_name: t.clone(),
        })
        .collect();
    let stored = StoredCustomType {
        tenant_id,
        name: name.to_string(),
        def: CustomTypeDef::Composite {
            fields: composite_fields,
        },
        oid,
        created_at,
    };

    persist_and_register(state, stored).await?;

    Ok(vec![Response::Execution(Tag::new("CREATE TYPE"))])
}

/// Handle `DROP TYPE [IF EXISTS] <name>`.
pub async fn drop_type(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    name: &str,
    if_exists: bool,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "drop custom types")?;
    let tenant_id = identity.tenant_id.as_u64();

    if !state.custom_type_registry.exists(tenant_id, name) {
        if if_exists {
            return Ok(vec![Response::Execution(Tag::new("DROP TYPE"))]);
        }
        return Err(sqlstate_error(
            "42704",
            &format!("type '{name}' does not exist"),
        ));
    }

    // Drop-protection: reject if any collection schema references this type.
    let referencing = find_referencing_collections(state, tenant_id, name);
    if !referencing.is_empty() {
        let list = referencing.join(", ");
        return Err(sqlstate_error(
            "2BP01",
            &format!("cannot drop type '{name}': it is referenced by collections: {list}"),
        ));
    }

    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog not available"))?;

    let entry = crate::control::catalog_entry::CatalogEntry::DeleteCustomType {
        tenant_id,
        name: name.to_string(),
    };
    let log_index = crate::control::metadata_proposer::propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    if log_index == 0 {
        catalog
            .delete_custom_type(tenant_id, name)
            .map_err(|e| sqlstate_error("XX000", &format!("catalog delete: {e}")))?;
    }

    state.custom_type_registry.unregister(tenant_id, name);

    Ok(vec![Response::Execution(Tag::new("DROP TYPE"))])
}

/// Handle `ALTER TYPE <name> ADD VALUE 'label'`.
pub async fn alter_type_add_value(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    type_name: &str,
    label: &str,
) -> PgWireResult<Vec<Response>> {
    require_admin(identity, "alter custom types")?;
    let tenant_id = identity.tenant_id.as_u64();

    let mut stored = state
        .custom_type_registry
        .get(tenant_id, type_name)
        .ok_or_else(|| sqlstate_error("42704", &format!("type '{type_name}' does not exist")))?;

    let labels = match &mut stored.def {
        CustomTypeDef::Enum { labels } => labels,
        CustomTypeDef::Composite { .. } => {
            return Err(sqlstate_error(
                "42809",
                &format!("type '{type_name}' is not an enum type"),
            ));
        }
    };

    if labels.iter().any(|l| l == label) {
        return Err(sqlstate_error(
            "42710",
            &format!("enum label '{label}' already exists in type '{type_name}'"),
        ));
    }

    labels.push(label.to_string());

    persist_and_register(state, stored).await?;

    Ok(vec![Response::Execution(Tag::new("ALTER TYPE"))])
}

/// Handle `SHOW TYPES`.
pub fn show_types(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
) -> PgWireResult<Vec<Response>> {
    let tenant_id = identity.tenant_id.as_u64();
    let types = state.custom_type_registry.list_for_tenant(tenant_id);

    let schema = Arc::new(vec![
        text_field("name"),
        text_field("kind"),
        text_field("definition"),
        text_field("oid"),
    ]);
    let mut rows = Vec::new();
    for t in &types {
        let mut enc = DataRowEncoder::new(schema.clone());
        enc.encode_field(&t.name)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        let (kind, def_str) = type_summary(&t.def);
        enc.encode_field(&kind)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        enc.encode_field(&def_str)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        let oid_str = t.oid.to_string();
        enc.encode_field(&oid_str)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        rows.push(Ok(enc.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Persist the entry to catalog and register in the in-memory registry.
async fn persist_and_register(state: &SharedState, stored: StoredCustomType) -> PgWireResult<()> {
    let catalog = state
        .credentials
        .catalog()
        .as_ref()
        .ok_or_else(|| sqlstate_error("XX000", "system catalog not available"))?;

    let entry =
        crate::control::catalog_entry::CatalogEntry::PutCustomType(Box::new(stored.clone()));
    let log_index = crate::control::metadata_proposer::propose_catalog_entry(state, &entry)
        .map_err(|e| sqlstate_error("XX000", &format!("metadata propose: {e}")))?;
    if log_index == 0 {
        catalog
            .put_custom_type(&stored)
            .map_err(|e| sqlstate_error("XX000", &format!("catalog write: {e}")))?;
    }

    state.custom_type_registry.register(stored);
    Ok(())
}

/// Scan all collection schemas for references to `type_name`.
///
/// Collections store field definitions as `(field_name, type_name)` pairs in `fields`.
/// A type is "referenced" when any field's type name matches `type_name`.
fn find_referencing_collections(
    state: &SharedState,
    tenant_id: u64,
    type_name: &str,
) -> Vec<String> {
    let catalog = match state.credentials.catalog() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let collections = match catalog.load_collections_for_tenant(tenant_id) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut referencing = Vec::new();
    for coll in &collections {
        if coll
            .fields
            .iter()
            .any(|(_field, ty)| ty.eq_ignore_ascii_case(type_name))
        {
            referencing.push(coll.name.clone());
        }
    }
    referencing
}

fn current_epoch_secs() -> PgWireResult<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| sqlstate_error("XX000", "system clock error"))
}

fn type_summary(def: &CustomTypeDef) -> (String, String) {
    match def {
        CustomTypeDef::Enum { labels } => ("enum".to_string(), labels.join(", ")),
        CustomTypeDef::Composite { fields } => {
            let defs: Vec<String> = fields
                .iter()
                .map(|f| format!("{} {}", f.name, f.type_name))
                .collect();
            ("composite".to_string(), defs.join(", "))
        }
    }
}
