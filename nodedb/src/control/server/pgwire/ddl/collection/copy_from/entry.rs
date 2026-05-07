// SPDX-License-Identifier: BUSL-1.1

//! Entry point: `copy_from_file`, path validation, and engine-support check.

use nodedb_types::DatabaseId;
use std::path::Path;

use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use nodedb_sql::ddl_ast::statement::CopyFormat;
use nodedb_types::CollectionType;

use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::server::pgwire::types::sqlstate_error;
use crate::control::state::SharedState;

use super::csv_import::import_csv;
use super::json_import::{import_json_array, import_ndjson};

/// Maximum file size accepted for COPY FROM (16 GiB).
pub(super) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Execute `COPY <collection> FROM '<path>' [WITH (...)]`.
pub async fn copy_from_file(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    collection: &str,
    path: &str,
    format: Option<&CopyFormat>,
    delimiter: Option<char>,
    header: bool,
) -> PgWireResult<Vec<Response>> {
    validate_path(path)?;

    // Check file size before reading.
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| sqlstate_error("58030", &format!("COPY: cannot stat file '{path}': {e}")))?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(sqlstate_error(
            "54000",
            &format!(
                "COPY: file '{path}' is {} bytes, exceeds limit of {} bytes",
                metadata.len(),
                MAX_FILE_BYTES
            ),
        ));
    }

    // Determine format (caller has already auto-detected from extension; this is a safety net).
    let resolved_format = format.ok_or_else(|| {
        sqlstate_error(
            "42601",
            &format!(
                "COPY: cannot infer format for '{path}'; \
                 add WITH (FORMAT ndjson|json|csv)"
            ),
        )
    })?;

    // Validate engine: reject Timeseries and Spatial.
    check_engine_support(state, identity, collection)?;

    let tenant_id = identity.tenant_id;

    let row_count = match resolved_format {
        CopyFormat::Ndjson => import_ndjson(state, identity, tenant_id, collection, path).await?,
        CopyFormat::JsonArray => {
            import_json_array(state, identity, tenant_id, collection, path).await?
        }
        CopyFormat::Csv => {
            import_csv(
                state,
                identity,
                tenant_id,
                collection,
                path,
                delimiter.unwrap_or(','),
                header,
            )
            .await?
        }
    };

    Ok(vec![Response::Execution(Tag::new(&format!(
        "COPY {row_count}"
    )))])
}

/// Reject paths with `..` segments and non-absolute paths.
fn validate_path(path: &str) -> PgWireResult<()> {
    if !path.starts_with('/') {
        return Err(sqlstate_error(
            "42601",
            &format!(
                "COPY: path '{path}' is not absolute; \
                 only absolute server-side paths are accepted"
            ),
        ));
    }
    let p = Path::new(path);
    for component in p.components() {
        use std::path::Component;
        if matches!(component, Component::ParentDir) {
            return Err(sqlstate_error(
                "42501",
                &format!(
                    "COPY: path '{path}' contains '..'; \
                     directory traversal is not permitted"
                ),
            ));
        }
    }
    Ok(())
}

/// Verify the collection engine supports COPY FROM.
fn check_engine_support(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    collection: &str,
) -> PgWireResult<()> {
    let tenant_id = identity.tenant_id;
    let catalog = match state.credentials.catalog() {
        Some(c) => c,
        None => return Ok(()), // No catalog means schemaless fallback — allow.
    };
    let stored = match catalog.get_collection(DatabaseId::DEFAULT, tenant_id.as_u64(), collection) {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()), // Collection doesn't exist yet — will fail at INSERT.
        Err(e) => {
            return Err(sqlstate_error(
                "XX000",
                &format!("COPY: catalog lookup failed: {e}"),
            ));
        }
    };

    match &stored.collection_type {
        CollectionType::Columnar(profile) => {
            use nodedb_types::ColumnarProfile;
            match profile {
                ColumnarProfile::Plain => Ok(()),
                ColumnarProfile::Timeseries { .. } => Err(sqlstate_error(
                    "0A000",
                    &format!(
                        "COPY: collection '{collection}' uses the timeseries engine; \
                         use ILP or INSERT with explicit time column instead"
                    ),
                )),
                ColumnarProfile::Spatial { .. } => Err(sqlstate_error(
                    "0A000",
                    &format!(
                        "COPY: collection '{collection}' uses the spatial engine; \
                         use INSERT with a WKT/GeoJSON geometry column instead"
                    ),
                )),
            }
        }
        CollectionType::Document(_) => Ok(()),
        CollectionType::KeyValue(_) => Ok(()),
    }
}

/// Wrap a row-level error to include the row number in the message.
pub(super) fn wrap_row_error(
    e: pgwire::error::PgWireError,
    line_no: usize,
    fmt: &str,
) -> pgwire::error::PgWireError {
    use pgwire::error::{ErrorInfo, PgWireError};
    match e {
        PgWireError::UserError(info) => PgWireError::UserError(Box::new(ErrorInfo::new(
            info.severity.clone(),
            info.code.clone(),
            format!("COPY: {fmt} row {line_no}: {}", info.message),
        ))),
        other => other,
    }
}
