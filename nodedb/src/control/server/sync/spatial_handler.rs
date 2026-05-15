// SPDX-License-Identifier: BUSL-1.1

//! Spatial geometry insert/delete handler for sync sessions.
//!
//! Decodes `SpatialInsertMsg` / `SpatialDeleteMsg` from a Lite client,
//! deserialises the geometry, allocates a surrogate for the document ID,
//! dispatches `SpatialOp::Insert` / `SpatialOp::Delete` to the Data Plane,
//! and returns an ACK frame.
//!
//! Structural pattern mirrors `fts_handler.rs`.

use async_trait::async_trait;
use tracing::{debug, error};

use nodedb_types::Surrogate;
use nodedb_types::geometry::Geometry;

use super::session::SyncSession;
use super::wire::*;
use crate::types::{DatabaseId, TenantId, TraceId, VShardId};

// ── Dispatcher trait ─────────────────────────────────────────────────────────

/// Encapsulates async Data Plane dispatch for spatial insert/delete.
#[async_trait]
pub trait SpatialDispatcher: Send + Sync {
    /// Insert a geometry into the R-tree on the Data Plane.
    ///
    /// `surrogate` is the stable global identity for the row; both the R-tree
    /// entry and the sparse document body are keyed by its hex encoding so
    /// cross-engine prefilter bitmaps intersect without translation.
    async fn dispatch_insert(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        field: String,
        surrogate: Surrogate,
        geometry: Geometry,
    ) -> crate::Result<()>;

    /// Remove a document's geometry from the R-tree on the Data Plane,
    /// keyed by the same surrogate used at insert time.
    async fn dispatch_delete(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        field: String,
        surrogate: Surrogate,
    ) -> crate::Result<()>;

    /// Assign a stable surrogate for `(collection, doc_id)`.
    fn assign_surrogate(&self, collection: &str, doc_id: &str) -> crate::Result<Surrogate>;
}

// ── SharedState adapter ──────────────────────────────────────────────────────

/// Production dispatcher: routes spatial ops to the Data Plane via the SPSC bridge.
pub struct SharedStateSpatialDispatcher<'a> {
    pub shared: &'a crate::control::state::SharedState,
}

#[async_trait]
impl<'a> SpatialDispatcher for SharedStateSpatialDispatcher<'a> {
    async fn dispatch_insert(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        field: String,
        surrogate: Surrogate,
        geometry: Geometry,
    ) -> crate::Result<()> {
        use crate::bridge::envelope::PhysicalPlan;
        use crate::bridge::physical_plan::SpatialOp;
        use crate::control::server::dispatch_utils::dispatch_to_data_plane_with_source;
        use crate::event::EventSource;

        let plan = PhysicalPlan::Spatial(SpatialOp::Insert {
            collection,
            field,
            surrogate,
            geometry,
        });

        dispatch_to_data_plane_with_source(
            self.shared,
            tenant_id,
            vshard,
            plan,
            TraceId::ZERO,
            EventSource::CrdtSync,
        )
        .await
        .map(|_| ())
    }

    async fn dispatch_delete(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        field: String,
        surrogate: Surrogate,
    ) -> crate::Result<()> {
        use crate::bridge::envelope::PhysicalPlan;
        use crate::bridge::physical_plan::SpatialOp;
        use crate::control::server::dispatch_utils::dispatch_to_data_plane_with_source;
        use crate::event::EventSource;

        let plan = PhysicalPlan::Spatial(SpatialOp::Delete {
            collection,
            field,
            surrogate,
        });

        dispatch_to_data_plane_with_source(
            self.shared,
            tenant_id,
            vshard,
            plan,
            TraceId::ZERO,
            EventSource::CrdtSync,
        )
        .await
        .map(|_| ())
    }

    fn assign_surrogate(&self, collection: &str, doc_id: &str) -> crate::Result<Surrogate> {
        self.shared
            .surrogate_assigner
            .assign(collection, doc_id.as_bytes())
    }
}

// ── NoOp dispatcher (loud failure) ──────────────────────────────────────────

/// Dispatcher used when `SharedState` is unavailable.
pub struct NoOpSpatialDispatcher;

#[async_trait]
impl SpatialDispatcher for NoOpSpatialDispatcher {
    async fn dispatch_insert(
        &self,
        _tenant_id: TenantId,
        _vshard: VShardId,
        _collection: String,
        _field: String,
        _surrogate: Surrogate,
        _geometry: Geometry,
    ) -> crate::Result<()> {
        Err(crate::Error::Internal {
            detail: "spatial insert routed through path lacking SharedState; \
                     check listener wiring — insert was ACKed but NOT applied"
                .to_string(),
        })
    }

    async fn dispatch_delete(
        &self,
        _tenant_id: TenantId,
        _vshard: VShardId,
        _collection: String,
        _field: String,
        _surrogate: Surrogate,
    ) -> crate::Result<()> {
        Err(crate::Error::Internal {
            detail: "spatial delete routed through path lacking SharedState; \
                     check listener wiring — delete was ACKed but NOT applied"
                .to_string(),
        })
    }

    fn assign_surrogate(&self, _collection: &str, _doc_id: &str) -> crate::Result<Surrogate> {
        Ok(Surrogate::ZERO)
    }
}

// ── Handler ──────────────────────────────────────────────────────────────────

impl SyncSession {
    /// Process a `SpatialInsertMsg`: deserialise geometry, allocate surrogate,
    /// dispatch to Data Plane R-tree, return an ACK frame.
    pub async fn handle_spatial_insert<D: SpatialDispatcher>(
        &mut self,
        msg: &SpatialInsertMsg,
        dispatcher: &D,
    ) -> Option<SyncFrame> {
        self.last_activity = std::time::Instant::now();

        if !self.authenticated {
            let ack = SpatialInsertAckMsg {
                collection: msg.collection.clone(),
                field: msg.field.clone(),
                doc_id: msg.doc_id.clone(),
                batch_id: msg.batch_id,
                accepted: false,
                reject_reason: Some("unauthenticated".to_string()),
            };
            return SyncFrame::try_encode(SyncMessageType::SpatialInsertAck, &ack);
        }

        // Deserialise the geometry from MessagePack bytes.
        let geometry: Geometry = match zerompk::from_msgpack(&msg.geometry_bytes) {
            Ok(g) => g,
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    field = %msg.field,
                    doc_id = %msg.doc_id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "spatial sync: geometry deserialisation failed"
                );
                let ack = SpatialInsertAckMsg {
                    collection: msg.collection.clone(),
                    field: msg.field.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(format!("geometry deserialise failed: {e}")),
                };
                return SyncFrame::try_encode(SyncMessageType::SpatialInsertAck, &ack);
            }
        };

        let surrogate = match dispatcher.assign_surrogate(&msg.collection, &msg.doc_id) {
            Ok(s) => s,
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    doc_id = %msg.doc_id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "spatial sync: surrogate assignment failed"
                );
                let ack = SpatialInsertAckMsg {
                    collection: msg.collection.clone(),
                    field: msg.field.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(format!("surrogate assignment failed: {e}")),
                };
                return SyncFrame::try_encode(SyncMessageType::SpatialInsertAck, &ack);
            }
        };

        let tenant_id = self.tenant_id.unwrap_or(TenantId::new(0));
        let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, &msg.collection);

        debug!(
            session = %self.session_id,
            collection = %msg.collection,
            field = %msg.field,
            doc_id = %msg.doc_id,
            batch_id = msg.batch_id,
            lite_id = %msg.lite_id,
            "spatial insert: dispatching to Data Plane"
        );

        match dispatcher
            .dispatch_insert(
                tenant_id,
                vshard,
                msg.collection.clone(),
                msg.field.clone(),
                surrogate,
                geometry,
            )
            .await
        {
            Ok(()) => {
                self.mutations_processed += 1;
                let ack = SpatialInsertAckMsg {
                    collection: msg.collection.clone(),
                    field: msg.field.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: true,
                    reject_reason: None,
                };
                SyncFrame::try_encode(SyncMessageType::SpatialInsertAck, &ack)
            }
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    field = %msg.field,
                    doc_id = %msg.doc_id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "spatial insert dispatch failed"
                );
                let ack = SpatialInsertAckMsg {
                    collection: msg.collection.clone(),
                    field: msg.field.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(e.to_string()),
                };
                SyncFrame::try_encode(SyncMessageType::SpatialInsertAck, &ack)
            }
        }
    }

    /// Process a `SpatialDeleteMsg`: dispatch removal to the Data Plane R-tree,
    /// return an ACK frame.
    pub async fn handle_spatial_delete<D: SpatialDispatcher>(
        &mut self,
        msg: &SpatialDeleteMsg,
        dispatcher: &D,
    ) -> Option<SyncFrame> {
        self.last_activity = std::time::Instant::now();

        if !self.authenticated {
            let ack = SpatialDeleteAckMsg {
                collection: msg.collection.clone(),
                field: msg.field.clone(),
                doc_id: msg.doc_id.clone(),
                batch_id: msg.batch_id,
                accepted: false,
                reject_reason: Some("unauthenticated".to_string()),
            };
            return SyncFrame::try_encode(SyncMessageType::SpatialDeleteAck, &ack);
        }

        let surrogate = match dispatcher.assign_surrogate(&msg.collection, &msg.doc_id) {
            Ok(s) => s,
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    doc_id = %msg.doc_id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "spatial sync: surrogate lookup failed for delete"
                );
                let ack = SpatialDeleteAckMsg {
                    collection: msg.collection.clone(),
                    field: msg.field.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(format!("surrogate lookup failed: {e}")),
                };
                return SyncFrame::try_encode(SyncMessageType::SpatialDeleteAck, &ack);
            }
        };

        let tenant_id = self.tenant_id.unwrap_or(TenantId::new(0));
        let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, &msg.collection);

        debug!(
            session = %self.session_id,
            collection = %msg.collection,
            field = %msg.field,
            doc_id = %msg.doc_id,
            batch_id = msg.batch_id,
            lite_id = %msg.lite_id,
            "spatial delete: dispatching to Data Plane"
        );

        match dispatcher
            .dispatch_delete(
                tenant_id,
                vshard,
                msg.collection.clone(),
                msg.field.clone(),
                surrogate,
            )
            .await
        {
            Ok(()) => {
                self.mutations_processed += 1;
                let ack = SpatialDeleteAckMsg {
                    collection: msg.collection.clone(),
                    field: msg.field.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: true,
                    reject_reason: None,
                };
                SyncFrame::try_encode(SyncMessageType::SpatialDeleteAck, &ack)
            }
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    field = %msg.field,
                    doc_id = %msg.doc_id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "spatial delete dispatch failed"
                );
                let ack = SpatialDeleteAckMsg {
                    collection: msg.collection.clone(),
                    field: msg.field.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(e.to_string()),
                };
                SyncFrame::try_encode(SyncMessageType::SpatialDeleteAck, &ack)
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    type MockCallLog = Arc<Mutex<Vec<(TenantId, String, String)>>>;

    struct MockDispatcher {
        insert_calls: MockCallLog,
        delete_calls: MockCallLog,
        result: crate::Result<()>,
    }

    impl MockDispatcher {
        fn ok() -> (Self, MockCallLog, MockCallLog) {
            let inserts = Arc::new(Mutex::new(Vec::new()));
            let deletes = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    insert_calls: inserts.clone(),
                    delete_calls: deletes.clone(),
                    result: Ok(()),
                },
                inserts,
                deletes,
            )
        }

        fn err() -> Self {
            Self {
                insert_calls: Arc::new(Mutex::new(Vec::new())),
                delete_calls: Arc::new(Mutex::new(Vec::new())),
                result: Err(crate::Error::Internal {
                    detail: "mock failure".to_string(),
                }),
            }
        }
    }

    #[async_trait]
    impl SpatialDispatcher for MockDispatcher {
        async fn dispatch_insert(
            &self,
            tenant_id: TenantId,
            _vshard: VShardId,
            collection: String,
            field: String,
            _surrogate: Surrogate,
            _geometry: Geometry,
        ) -> crate::Result<()> {
            self.insert_calls
                .lock()
                .unwrap()
                .push((tenant_id, collection, field));
            match &self.result {
                Ok(()) => Ok(()),
                Err(e) => Err(crate::Error::Internal {
                    detail: e.to_string(),
                }),
            }
        }

        async fn dispatch_delete(
            &self,
            tenant_id: TenantId,
            _vshard: VShardId,
            collection: String,
            field: String,
            _surrogate: Surrogate,
        ) -> crate::Result<()> {
            self.delete_calls
                .lock()
                .unwrap()
                .push((tenant_id, collection, field));
            match &self.result {
                Ok(()) => Ok(()),
                Err(e) => Err(crate::Error::Internal {
                    detail: e.to_string(),
                }),
            }
        }

        fn assign_surrogate(&self, _collection: &str, _doc_id: &str) -> crate::Result<Surrogate> {
            Ok(Surrogate::ZERO)
        }
    }

    fn make_session() -> SyncSession {
        SyncSession::new("test-spatial-session".to_string())
    }

    fn make_point_geometry_bytes() -> Vec<u8> {
        let geom = nodedb_types::geometry::Geometry::point(10.0, 20.0);
        zerompk::to_msgpack_vec(&geom).unwrap()
    }

    fn make_insert_msg(collection: &str, field: &str, doc_id: &str) -> SpatialInsertMsg {
        SpatialInsertMsg {
            lite_id: "lite-test".to_string(),
            collection: collection.to_string(),
            field: field.to_string(),
            doc_id: doc_id.to_string(),
            geometry_bytes: make_point_geometry_bytes(),
            batch_id: 1,
        }
    }

    fn make_delete_msg(collection: &str, field: &str, doc_id: &str) -> SpatialDeleteMsg {
        SpatialDeleteMsg {
            lite_id: "lite-test".to_string(),
            collection: collection.to_string(),
            field: field.to_string(),
            doc_id: doc_id.to_string(),
            batch_id: 2,
        }
    }

    #[tokio::test]
    async fn unauthenticated_insert_returns_rejection() {
        let mut session = make_session();
        let (mock, inserts, _) = MockDispatcher::ok();
        let msg = make_insert_msg("places", "loc", "d1");

        let frame = session.handle_spatial_insert(&msg, &mock).await;
        assert!(frame.is_some());
        let ack: SpatialInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(inserts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn authenticated_insert_dispatches_and_acks() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, inserts, _) = MockDispatcher::ok();
        let msg = make_insert_msg("places", "loc", "d1");

        let frame = session.handle_spatial_insert(&msg, &mock).await;
        assert!(frame.is_some());
        let ack: SpatialInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.doc_id, "d1");
        assert_eq!(ack.field, "loc");
        assert_eq!(inserts.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn insert_dispatch_failure_rejects() {
        let mut session = make_session();
        session.authenticated = true;
        let mock = MockDispatcher::err();
        let msg = make_insert_msg("places", "loc", "d1");

        let frame = session.handle_spatial_insert(&msg, &mock).await;
        let ack: SpatialInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(ack.reject_reason.is_some());
    }

    #[tokio::test]
    async fn authenticated_delete_dispatches_and_acks() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, _, deletes) = MockDispatcher::ok();
        let msg = make_delete_msg("places", "loc", "d1");

        let frame = session.handle_spatial_delete(&msg, &mock).await;
        let ack: SpatialDeleteAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.doc_id, "d1");
        assert_eq!(deletes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unauthenticated_delete_returns_rejection() {
        let mut session = make_session();
        let (mock, _, deletes) = MockDispatcher::ok();
        let msg = make_delete_msg("places", "loc", "d1");

        let frame = session.handle_spatial_delete(&msg, &mock).await;
        let ack: SpatialDeleteAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(deletes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_geometry_bytes_rejects_insert() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, inserts, _) = MockDispatcher::ok();

        let msg = SpatialInsertMsg {
            lite_id: "lite-test".to_string(),
            collection: "places".to_string(),
            field: "loc".to_string(),
            doc_id: "d1".to_string(),
            geometry_bytes: vec![0xFF, 0xFF, 0xFF], // invalid msgpack
            batch_id: 1,
        };

        let frame = session.handle_spatial_insert(&msg, &mock).await;
        let ack: SpatialInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(ack.reject_reason.is_some());
        assert!(inserts.lock().unwrap().is_empty());
    }
}
