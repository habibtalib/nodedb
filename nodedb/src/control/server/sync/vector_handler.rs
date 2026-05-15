// SPDX-License-Identifier: BUSL-1.1

//! Vector insert/delete handler for sync sessions.
//!
//! Decodes `VectorInsertMsg` / `VectorDeleteMsg` from a Lite client,
//! allocates a surrogate for the document ID via `SurrogateAssigner`,
//! dispatches `VectorOp::Insert` / `VectorOp::DeleteBySurrogate` to the
//! Data Plane, and returns an ACK frame.
//!
//! Structural pattern mirrors `columnar_handler.rs`:
//! a dispatcher trait ties ingest and ACK together so an ACK can never be
//! returned without at least attempting dispatch.

use async_trait::async_trait;
use tracing::{debug, error, warn};

use nodedb_types::Surrogate;

use super::session::SyncSession;
use super::wire::*;
use crate::types::{DatabaseId, TenantId, TraceId, VShardId};

// ── Dispatcher trait ─────────────────────────────────────────────────────────

/// Parameters for a single vector insert dispatch.
pub struct VectorInsertParams {
    pub collection: String,
    pub vector: Vec<f32>,
    pub dim: usize,
    pub field_name: String,
    pub surrogate: Surrogate,
}

/// Encapsulates async Data Plane dispatch for vector insert/delete.
#[async_trait]
pub trait VectorDispatcher: Send + Sync {
    /// Insert a vector into the HNSW index on the Data Plane.
    async fn dispatch_insert(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        params: VectorInsertParams,
    ) -> crate::Result<()>;

    /// Delete a vector by surrogate from the HNSW index on the Data Plane.
    async fn dispatch_delete(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        surrogate: Surrogate,
        field_name: String,
    ) -> crate::Result<()>;

    /// Assign a stable surrogate for `(collection, doc_id)`.
    fn assign_surrogate(&self, collection: &str, doc_id: &str) -> crate::Result<Surrogate>;
}

// ── SharedState adapter ──────────────────────────────────────────────────────

/// Production dispatcher: routes vector ops to the Data Plane via the SPSC
/// bridge using `EventSource::CrdtSync` (suppresses AFTER triggers on synced
/// data).
pub struct SharedStateVectorDispatcher<'a> {
    pub shared: &'a crate::control::state::SharedState,
}

#[async_trait]
impl<'a> VectorDispatcher for SharedStateVectorDispatcher<'a> {
    async fn dispatch_insert(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        params: VectorInsertParams,
    ) -> crate::Result<()> {
        use crate::bridge::envelope::PhysicalPlan;
        use crate::bridge::physical_plan::VectorOp;
        use crate::control::server::dispatch_utils::dispatch_to_data_plane_with_source;
        use crate::event::EventSource;

        let plan = PhysicalPlan::Vector(VectorOp::Insert {
            collection: params.collection,
            vector: params.vector,
            dim: params.dim,
            field_name: params.field_name,
            surrogate: params.surrogate,
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
        surrogate: Surrogate,
        field_name: String,
    ) -> crate::Result<()> {
        use crate::bridge::envelope::PhysicalPlan;
        use crate::bridge::physical_plan::VectorOp;
        use crate::control::server::dispatch_utils::dispatch_to_data_plane_with_source;
        use crate::event::EventSource;

        let plan = PhysicalPlan::Vector(VectorOp::DeleteBySurrogate {
            collection,
            surrogate,
            field_name,
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
pub struct NoOpVectorDispatcher;

#[async_trait]
impl VectorDispatcher for NoOpVectorDispatcher {
    async fn dispatch_insert(
        &self,
        _tenant_id: TenantId,
        _vshard: VShardId,
        _params: VectorInsertParams,
    ) -> crate::Result<()> {
        Err(crate::Error::Internal {
            detail: "vector insert routed through path lacking SharedState; \
                     check listener wiring — insert was ACKed but NOT applied"
                .to_string(),
        })
    }

    async fn dispatch_delete(
        &self,
        _tenant_id: TenantId,
        _vshard: VShardId,
        _collection: String,
        _surrogate: Surrogate,
        _field_name: String,
    ) -> crate::Result<()> {
        Err(crate::Error::Internal {
            detail: "vector delete routed through path lacking SharedState; \
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
    /// Process a `VectorInsertMsg`: allocate surrogate, dispatch to Data Plane,
    /// return an ACK frame.
    ///
    /// Unauthenticated sessions receive a rejection ACK without dispatch.
    pub async fn handle_vector_insert<D: VectorDispatcher>(
        &mut self,
        msg: &VectorInsertMsg,
        dispatcher: &D,
    ) -> Option<SyncFrame> {
        self.last_activity = std::time::Instant::now();

        if !self.authenticated {
            let ack = VectorInsertAckMsg {
                collection: msg.collection.clone(),
                id: msg.id.clone(),
                batch_id: msg.batch_id,
                accepted: false,
                reject_reason: Some("unauthenticated".to_string()),
            };
            return SyncFrame::try_encode(SyncMessageType::VectorInsertAck, &ack);
        }

        if msg.vector.len() != msg.dim || msg.dim == 0 {
            warn!(
                session = %self.session_id,
                collection = %msg.collection,
                id = %msg.id,
                batch_id = msg.batch_id,
                stated_dim = msg.dim,
                actual_len = msg.vector.len(),
                "vector sync: dimension mismatch; rejecting"
            );
            let ack = VectorInsertAckMsg {
                collection: msg.collection.clone(),
                id: msg.id.clone(),
                batch_id: msg.batch_id,
                accepted: false,
                reject_reason: Some(format!(
                    "dimension mismatch: stated {}, actual {}",
                    msg.dim,
                    msg.vector.len()
                )),
            };
            return SyncFrame::try_encode(SyncMessageType::VectorInsertAck, &ack);
        }

        let surrogate = match dispatcher.assign_surrogate(&msg.collection, &msg.id) {
            Ok(s) => s,
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    id = %msg.id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "vector sync: surrogate assignment failed"
                );
                let ack = VectorInsertAckMsg {
                    collection: msg.collection.clone(),
                    id: msg.id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(format!("surrogate assignment failed: {e}")),
                };
                return SyncFrame::try_encode(SyncMessageType::VectorInsertAck, &ack);
            }
        };

        let tenant_id = self.tenant_id.unwrap_or(TenantId::new(0));
        let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, &msg.collection);

        debug!(
            session = %self.session_id,
            collection = %msg.collection,
            id = %msg.id,
            batch_id = msg.batch_id,
            dim = msg.dim,
            lite_id = %msg.lite_id,
            "vector insert: dispatching to Data Plane"
        );

        match dispatcher
            .dispatch_insert(
                tenant_id,
                vshard,
                VectorInsertParams {
                    collection: msg.collection.clone(),
                    vector: msg.vector.clone(),
                    dim: msg.dim,
                    field_name: msg.field_name.clone(),
                    surrogate,
                },
            )
            .await
        {
            Ok(()) => {
                self.mutations_processed += 1;
                let ack = VectorInsertAckMsg {
                    collection: msg.collection.clone(),
                    id: msg.id.clone(),
                    batch_id: msg.batch_id,
                    accepted: true,
                    reject_reason: None,
                };
                SyncFrame::try_encode(SyncMessageType::VectorInsertAck, &ack)
            }
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    id = %msg.id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "vector insert dispatch failed"
                );
                let ack = VectorInsertAckMsg {
                    collection: msg.collection.clone(),
                    id: msg.id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(e.to_string()),
                };
                SyncFrame::try_encode(SyncMessageType::VectorInsertAck, &ack)
            }
        }
    }

    /// Process a `VectorDeleteMsg`: look up surrogate, dispatch tombstone to
    /// Data Plane, return an ACK frame.
    pub async fn handle_vector_delete<D: VectorDispatcher>(
        &mut self,
        msg: &VectorDeleteMsg,
        dispatcher: &D,
    ) -> Option<SyncFrame> {
        self.last_activity = std::time::Instant::now();

        if !self.authenticated {
            let ack = VectorDeleteAckMsg {
                collection: msg.collection.clone(),
                id: msg.id.clone(),
                batch_id: msg.batch_id,
                accepted: false,
                reject_reason: Some("unauthenticated".to_string()),
            };
            return SyncFrame::try_encode(SyncMessageType::VectorDeleteAck, &ack);
        }

        // Resolve surrogate — idempotent: if the surrogate was never assigned,
        // the delete is a no-op.
        let surrogate = match dispatcher.assign_surrogate(&msg.collection, &msg.id) {
            Ok(s) => s,
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    id = %msg.id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "vector sync: surrogate lookup failed for delete"
                );
                let ack = VectorDeleteAckMsg {
                    collection: msg.collection.clone(),
                    id: msg.id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(format!("surrogate lookup failed: {e}")),
                };
                return SyncFrame::try_encode(SyncMessageType::VectorDeleteAck, &ack);
            }
        };

        let tenant_id = self.tenant_id.unwrap_or(TenantId::new(0));
        let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, &msg.collection);

        debug!(
            session = %self.session_id,
            collection = %msg.collection,
            id = %msg.id,
            batch_id = msg.batch_id,
            lite_id = %msg.lite_id,
            "vector delete: dispatching to Data Plane"
        );

        match dispatcher
            .dispatch_delete(
                tenant_id,
                vshard,
                msg.collection.clone(),
                surrogate,
                msg.field_name.clone(),
            )
            .await
        {
            Ok(()) => {
                self.mutations_processed += 1;
                let ack = VectorDeleteAckMsg {
                    collection: msg.collection.clone(),
                    id: msg.id.clone(),
                    batch_id: msg.batch_id,
                    accepted: true,
                    reject_reason: None,
                };
                SyncFrame::try_encode(SyncMessageType::VectorDeleteAck, &ack)
            }
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    id = %msg.id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "vector delete dispatch failed"
                );
                let ack = VectorDeleteAckMsg {
                    collection: msg.collection.clone(),
                    id: msg.id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(e.to_string()),
                };
                SyncFrame::try_encode(SyncMessageType::VectorDeleteAck, &ack)
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
    impl VectorDispatcher for MockDispatcher {
        async fn dispatch_insert(
            &self,
            tenant_id: TenantId,
            _vshard: VShardId,
            params: VectorInsertParams,
        ) -> crate::Result<()> {
            self.insert_calls
                .lock()
                .unwrap()
                .push((tenant_id, params.collection, String::new()));
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
            _surrogate: Surrogate,
            _field_name: String,
        ) -> crate::Result<()> {
            self.delete_calls
                .lock()
                .unwrap()
                .push((tenant_id, collection, String::new()));
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
        SyncSession::new("test-vector-session".to_string())
    }

    fn make_insert_msg(collection: &str, id: &str, vector: Vec<f32>) -> VectorInsertMsg {
        let dim = vector.len();
        VectorInsertMsg {
            lite_id: "lite-test".to_string(),
            collection: collection.to_string(),
            id: id.to_string(),
            vector,
            dim,
            field_name: String::new(),
            batch_id: 1,
        }
    }

    fn make_delete_msg(collection: &str, id: &str) -> VectorDeleteMsg {
        VectorDeleteMsg {
            lite_id: "lite-test".to_string(),
            collection: collection.to_string(),
            id: id.to_string(),
            field_name: String::new(),
            batch_id: 2,
        }
    }

    #[tokio::test]
    async fn unauthenticated_insert_returns_rejection() {
        let mut session = make_session();
        let (mock, inserts, _) = MockDispatcher::ok();
        let msg = make_insert_msg("vecs", "v1", vec![1.0, 0.0, 0.0]);

        let frame = session.handle_vector_insert(&msg, &mock).await;
        assert!(frame.is_some());
        let ack: VectorInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(inserts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn authenticated_insert_dispatches_and_acks() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, inserts, _) = MockDispatcher::ok();
        let msg = make_insert_msg("vecs", "v1", vec![1.0, 0.0, 0.0]);

        let frame = session.handle_vector_insert(&msg, &mock).await;
        assert!(frame.is_some());
        let ack: VectorInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.id, "v1");
        assert_eq!(inserts.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn insert_dimension_mismatch_rejects() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, _, _) = MockDispatcher::ok();
        let mut msg = make_insert_msg("vecs", "v1", vec![1.0, 0.0, 0.0]);
        msg.dim = 5; // Mismatch: vector.len() == 3, dim == 5

        let frame = session.handle_vector_insert(&msg, &mock).await;
        let ack: VectorInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(ack.reject_reason.unwrap().contains("dimension mismatch"));
    }

    #[tokio::test]
    async fn insert_dispatch_failure_rejects() {
        let mut session = make_session();
        session.authenticated = true;
        let mock = MockDispatcher::err();
        let msg = make_insert_msg("vecs", "v1", vec![1.0, 0.0]);

        let frame = session.handle_vector_insert(&msg, &mock).await;
        let ack: VectorInsertAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(ack.reject_reason.is_some());
    }

    #[tokio::test]
    async fn authenticated_delete_dispatches_and_acks() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, _, deletes) = MockDispatcher::ok();
        let msg = make_delete_msg("vecs", "v1");

        let frame = session.handle_vector_delete(&msg, &mock).await;
        let ack: VectorDeleteAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.id, "v1");
        assert_eq!(deletes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unauthenticated_delete_returns_rejection() {
        let mut session = make_session();
        let (mock, _, deletes) = MockDispatcher::ok();
        let msg = make_delete_msg("vecs", "v1");

        let frame = session.handle_vector_delete(&msg, &mock).await;
        let ack: VectorDeleteAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(deletes.lock().unwrap().is_empty());
    }
}
