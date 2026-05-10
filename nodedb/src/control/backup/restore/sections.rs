// SPDX-License-Identifier: BUSL-1.1

//! Catalog-section and data-section helpers for RESTORE TENANT.

use nodedb_types::DatabaseId;
use std::sync::Arc;

use crate::Error;
use crate::control::state::SharedState;
use crate::types::TenantDataSnapshot;

pub(super) fn merge_sections(
    sections: &[nodedb_types::backup_envelope::Section],
) -> Result<TenantDataSnapshot, Error> {
    let mut merged = TenantDataSnapshot::default();
    for section in sections {
        if is_metadata_section(section) {
            continue;
        }
        let snap: TenantDataSnapshot =
            zerompk::from_msgpack(&section.body).map_err(|_| Error::Internal {
                detail: "invalid backup format: section payload is not a tenant snapshot".into(),
            })?;
        merged.documents.extend(snap.documents);
        merged.indexes.extend(snap.indexes);
        merged.edges.extend(snap.edges);
        merged.vectors.extend(snap.vectors);
        merged.kv_tables.extend(snap.kv_tables);
        merged.crdt_state.extend(snap.crdt_state);
        merged.timeseries.extend(snap.timeseries);
    }
    Ok(merged)
}

pub(super) fn is_metadata_section(section: &nodedb_types::backup_envelope::Section) -> bool {
    matches!(
        section.origin_node_id,
        nodedb_types::backup_envelope::SECTION_ORIGIN_CATALOG_ROWS
            | nodedb_types::backup_envelope::SECTION_ORIGIN_SOURCE_TOMBSTONES
    )
}

/// Apply catalog-row and source-tombstone sections to the destination catalog.
/// Runs BEFORE the data-section restore.
pub(super) fn apply_metadata_sections(
    state: &Arc<SharedState>,
    tenant_id: u64,
    env: &nodedb_types::backup_envelope::Envelope,
) {
    use nodedb_types::backup_envelope::{
        SECTION_ORIGIN_CATALOG_ROWS, SECTION_ORIGIN_SOURCE_TOMBSTONES, SourceTombstoneEntry,
        StoredCollectionBlob,
    };
    let Some(catalog) = state.credentials.catalog() else {
        return;
    };

    for section in &env.sections {
        match section.origin_node_id {
            SECTION_ORIGIN_CATALOG_ROWS => {
                let Ok(blobs) = zerompk::from_msgpack::<Vec<StoredCollectionBlob>>(&section.body)
                else {
                    tracing::warn!(
                        tenant_id,
                        "restore: catalog-rows section failed to decode — skipping"
                    );
                    continue;
                };
                for blob in blobs {
                    let Ok(coll) = zerompk::from_msgpack::<
                        crate::control::security::catalog::StoredCollection,
                    >(&blob.bytes) else {
                        tracing::warn!(
                            tenant_id,
                            name = %blob.name,
                            "restore: catalog row failed to decode — skipping"
                        );
                        continue;
                    };
                    if let Err(e) = catalog.put_collection(DatabaseId::DEFAULT, &coll) {
                        tracing::warn!(
                            tenant_id,
                            name = %blob.name,
                            error = %e,
                            "restore: catalog put_collection failed"
                        );
                    }
                }
            }
            SECTION_ORIGIN_SOURCE_TOMBSTONES => {
                let Ok(tombs) = zerompk::from_msgpack::<Vec<SourceTombstoneEntry>>(&section.body)
                else {
                    tracing::warn!(
                        tenant_id,
                        "restore: source-tombstones section failed to decode — skipping"
                    );
                    continue;
                };
                for t in tombs {
                    if let Err(e) =
                        catalog.record_wal_tombstone(tenant_id, &t.collection, t.purge_lsn)
                    {
                        tracing::warn!(
                            tenant_id,
                            collection = %t.collection,
                            purge_lsn = t.purge_lsn,
                            error = %e,
                            "restore: record_wal_tombstone failed"
                        );
                    }
                }
            }
            _ => {}
        }
    }
}
