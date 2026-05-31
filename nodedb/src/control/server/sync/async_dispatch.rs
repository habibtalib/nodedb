// SPDX-License-Identifier: BUSL-1.1

//! Async Data Plane dispatch helpers for the sync WebSocket listener.
//!
//! Contains async functions that cross the Control Plane / Data Plane boundary
//! via the SPSC bridge: shape-subscription snapshot queries and CRDT delta
//! constraint validation.

use std::time::Duration;

use tracing::{info, warn};

use crate::control::state::SharedState;

use super::wire::{CompensationHint, DeltaPushMsg, DeltaRejectMsg, SyncFrame, SyncMessageType};

/// Handle ShapeSubscribe with real WAL LSN and Data Plane snapshot.
pub(super) async fn handle_shape_subscribe_async(
    shared: &SharedState,
    session: &super::session::SyncSession,
    frame: &SyncFrame,
) -> Option<SyncFrame> {
    use crate::bridge::envelope::PhysicalPlan;
    use crate::control::server::pgwire::ddl::sync_dispatch::dispatch_async;
    use crate::types::TenantId;
    use nodedb_physical::physical_plan::DocumentOp;

    let msg: super::shape::handler::ShapeSubscribeMsg = frame.decode_body()?;
    let tenant_id = session.tenant_id.map(|t| t.as_u64()).unwrap_or(0);

    // Quota enforcement — reject before dispatch.
    let tid = TenantId::new(tenant_id);
    if let Err(e) = shared.check_tenant_quota(tid) {
        warn!(tenant_id, error = %e, "sync: shape subscribe rejected by quota");
        return None;
    }

    // Get current WAL LSN — this is the watermark for the snapshot.
    let current_lsn = shared.wal.next_lsn().as_u64().saturating_sub(1);

    // Dispatch a query to the Data Plane to get matching data for this shape.
    shared.tenant_request_start(tid);
    let snapshot_data = match &msg.shape.shape_type {
        nodedb_types::sync::shape::ShapeType::Document {
            collection,
            predicate,
        } => {
            // Query the Data Plane for all documents in this collection.
            let plan = PhysicalPlan::Document(DocumentOp::RangeScan {
                collection: collection.clone(),
                field: String::new(), // Empty = full collection scan.
                lower: None,
                upper: None,
                limit: 10_000, // Cap for safety.
            });
            match dispatch_async(
                shared,
                TenantId::new(tenant_id),
                collection,
                plan,
                Duration::from_secs(10),
            )
            .await
            {
                Ok(payload) => {
                    filter_snapshot_by_predicate(payload, predicate, &msg.shape.shape_id)
                }
                Err(e) => {
                    tracing::warn!(
                        shape_id = %msg.shape.shape_id,
                        error = %e,
                        "shape snapshot query failed, sending empty snapshot"
                    );
                    super::shape::handler::ShapeSnapshotData::empty()
                }
            }
        }
        nodedb_types::sync::shape::ShapeType::Vector { collection, .. } => {
            // For vector shapes, the snapshot is the collection metadata.
            // Full vector data is too large — Lite rebuilds from its own HNSW.
            super::shape::handler::ShapeSnapshotData {
                data: collection.as_bytes().to_vec(),
                doc_count: 0,
            }
        }
        nodedb_types::sync::shape::ShapeType::Graph { .. } => {
            // Graph shapes: snapshot is the subgraph from root nodes.
            // For now, return empty — full graph snapshot needs BFS dispatch.
            super::shape::handler::ShapeSnapshotData::empty()
        }
        nodedb_types::sync::shape::ShapeType::Array {
            array_name,
            coord_range,
        } => {
            // Array shapes: validate the array exists, initialize the subscriber
            // cursor, and return empty snapshot data. Full catch-up (op-log
            // replay or tile snapshot) is driven by Phase H on the Lite side.
            //
            // 1. Validate the array exists in the schema registry.
            let array_known = shared.array_sync_schemas.schema_hlc(array_name).is_some();
            if !array_known {
                warn!(
                    session = %session.session_id,
                    array = %array_name,
                    "array shape subscribe: array not known to Origin schema registry"
                );
                // Return without registering — the subscribe response will go
                // back with an empty snapshot, and the Lite peer will retry
                // when the schema is synced.
                shared.tenant_request_end(tid);
                return super::shape::handler::handle_subscribe(
                    &session.session_id,
                    tenant_id,
                    &msg,
                    &super::shape::registry::ShapeRegistry::new(),
                    current_lsn,
                    |_, _| super::shape::handler::ShapeSnapshotData::empty(),
                );
            }

            // 2. Initialize the subscriber cursor at Hlc::ZERO so Phase H's
            //    catch-up path delivers all history on first sync.
            shared.array_subscriber_cursors.register(
                &session.session_id,
                array_name,
                coord_range.clone(),
            );

            info!(
                session = %session.session_id,
                array = %array_name,
                "array shape subscribed; cursor initialized at HLC::ZERO"
            );

            // 3. Return empty snapshot data — catch-up via Phase H.
            super::shape::handler::ShapeSnapshotData::empty()
        }
        // ShapeType is #[non_exhaustive]: new variants added in future protocol
        // versions reach this arm before the handler is updated. Return empty
        // snapshot — the subscriber will receive a well-formed but unpopulated
        // response and can retry once the server is updated.
        _ => {
            warn!(
                session = %session.session_id,
                "shape subscribe: unknown shape_type variant, sending empty snapshot"
            );
            super::shape::handler::ShapeSnapshotData::empty()
        }
    };

    shared.tenant_request_end(tid);

    // Register the shape subscription.
    let registry = super::shape::registry::ShapeRegistry::new();
    let response = super::shape::handler::handle_subscribe(
        &session.session_id,
        tenant_id,
        &msg,
        &registry,
        current_lsn,
        |_shape, _lsn| snapshot_data,
    );

    info!(
        session = %session.session_id,
        shape_id = %msg.shape.shape_id,
        lsn = current_lsn,
        "shape subscribed with WAL LSN watermark"
    );

    response
}

/// Async constraint validation for a delta before sending DeltaAck.
///
/// Dispatches the delta to the Data Plane's CRDT engine for pre-validation
/// (UNIQUE, FK constraints). If validation fails, converts the DeltaAck
/// to a DeltaReject with a typed CompensationHint.
pub(super) async fn validate_delta_constraints(
    shared: &SharedState,
    delta_msg: &DeltaPushMsg,
    ack_frame: SyncFrame,
) -> Option<SyncFrame> {
    use crate::bridge::envelope::PhysicalPlan;
    use crate::control::server::pgwire::ddl::sync_dispatch::dispatch_async_with_source;
    use crate::types::TenantId;
    use nodedb_physical::physical_plan::CrdtOp;

    // Dispatch a CrdtApply plan to the Data Plane. If the CRDT engine
    // rejects it (constraint violation), we get an error back.
    // Uses EventSource::CrdtSync so triggers are NOT fired on replicated deltas.
    let tenant_id = TenantId::new(0); // Trust mode default tenant.

    // Quota enforcement — reject before dispatch.
    if let Err(e) = shared.check_tenant_quota(tenant_id) {
        warn!(error = %e, "sync: delta validation rejected by quota");
        let reject = DeltaRejectMsg {
            mutation_id: delta_msg.mutation_id,
            reason: e.to_string(),
            compensation: Some(CompensationHint::Custom {
                constraint: "quota".into(),
                detail: e.to_string(),
            }),
        };
        return SyncFrame::try_encode(SyncMessageType::DeltaReject, &reject);
    }

    let surrogate = match shared
        .surrogate_assigner
        .assign(&delta_msg.collection, delta_msg.document_id.as_bytes())
    {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "sync: surrogate assignment failed");
            let reject = DeltaRejectMsg {
                mutation_id: delta_msg.mutation_id,
                reason: e.to_string(),
                compensation: Some(CompensationHint::Custom {
                    constraint: "surrogate".into(),
                    detail: e.to_string(),
                }),
            };
            return SyncFrame::try_encode(SyncMessageType::DeltaReject, &reject);
        }
    };

    let plan = PhysicalPlan::Crdt(CrdtOp::Apply {
        collection: delta_msg.collection.clone(),
        document_id: delta_msg.document_id.clone(),
        delta: delta_msg.delta.clone(),
        peer_id: delta_msg.peer_id,
        mutation_id: delta_msg.mutation_id,
        surrogate,
    });

    shared.tenant_request_start(tenant_id);
    let dispatch_result = dispatch_async_with_source(
        shared,
        tenant_id,
        &delta_msg.collection,
        plan,
        Duration::from_secs(10),
        crate::event::EventSource::CrdtSync,
    )
    .await;
    shared.tenant_request_end(tenant_id);

    match dispatch_result {
        Ok(_payload) => {
            // Constraint check passed — send the original DeltaAck.
            Some(ack_frame)
        }
        Err(e) => {
            let error_detail = e.to_string();
            // Constraint check failed — convert to DeltaReject.
            warn!(
                collection = %delta_msg.collection,
                doc = %delta_msg.document_id,
                error = %error_detail,
                "sync: delta constraint violation"
            );

            let hint = if error_detail.contains("unique") || error_detail.contains("UNIQUE") {
                CompensationHint::UniqueViolation {
                    field: "unknown".into(),
                    conflicting_value: delta_msg.document_id.clone(),
                }
            } else if error_detail.contains("foreign") || error_detail.contains("FK") {
                CompensationHint::ForeignKeyMissing {
                    referenced_id: delta_msg.document_id.clone(),
                }
            } else {
                CompensationHint::Custom {
                    constraint: "constraint".into(),
                    detail: error_detail.clone(),
                }
            };

            let reject = DeltaRejectMsg {
                mutation_id: delta_msg.mutation_id,
                reason: error_detail,
                compensation: Some(hint),
            };
            SyncFrame::try_encode(SyncMessageType::DeltaReject, &reject)
        }
    }
}

// ── Snapshot predicate filtering ──────────────────────────────────────────────

/// Filter a raw snapshot payload by a shape predicate.
///
/// Decodes the msgpack document rows, evaluates each document's data bytes
/// against the `MetadataFilter` decoded from `predicate_bytes`, and re-encodes
/// only the matching rows. An empty predicate returns the payload unchanged.
/// A predicate that fails to decode is logged as a warning and the entire
/// snapshot is returned empty (fail-closed, consistent with delta routing).
fn filter_snapshot_by_predicate(
    payload: Vec<u8>,
    predicate_bytes: &[u8],
    shape_id: &str,
) -> super::shape::handler::ShapeSnapshotData {
    use crate::data::executor::response_codec::{
        decode_raw_scan_to_docs, encode_raw_document_rows,
    };
    use nodedb_query::metadata_filter::matches_metadata_filter;
    use nodedb_types::filter::MetadataFilter;

    if predicate_bytes.is_empty() {
        let doc_count = decode_raw_scan_to_docs(&payload).len();
        return super::shape::handler::ShapeSnapshotData {
            data: payload,
            doc_count,
        };
    }

    let filter = match zerompk::from_msgpack::<MetadataFilter>(predicate_bytes) {
        Ok(f) => f,
        Err(err) => {
            warn!(
                shape_id,
                error = %err,
                "shape snapshot: failed to decode predicate; sending empty snapshot"
            );
            return super::shape::handler::ShapeSnapshotData::empty();
        }
    };

    let docs = decode_raw_scan_to_docs(&payload);
    let mut matching: Vec<(String, Vec<u8>)> = Vec::new();

    for (doc_id, data_bytes) in docs {
        let doc_json = crate::control::server::sync::security::delta_bytes_to_json(&data_bytes);
        if matches_metadata_filter(&doc_json, &filter) {
            matching.push((doc_id, data_bytes));
        }
    }

    let doc_count = matching.len();
    match encode_raw_document_rows(&matching) {
        Ok(data) => super::shape::handler::ShapeSnapshotData { data, doc_count },
        Err(err) => {
            // Fail closed: a re-encode failure must not ship a header whose
            // doc_count disagrees with its (empty) body. Drop the snapshot,
            // matching the predicate-decode failure path above.
            warn!(
                shape_id,
                error = %err,
                "shape snapshot: failed to encode filtered rows; sending empty snapshot"
            );
            super::shape::handler::ShapeSnapshotData::empty()
        }
    }
}
