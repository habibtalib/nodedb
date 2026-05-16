// SPDX-License-Identifier: BUSL-1.1

//! FTS index/delete handler for sync sessions.
//!
//! Decodes `FtsIndexMsg` / `FtsDeleteMsg` from a Lite client,
//! allocates a surrogate for the document ID via `SurrogateAssigner`,
//! dispatches `TextOp::FtsIndexDoc` / `TextOp::FtsDeleteDoc` to the
//! Data Plane, and returns an ACK frame.
//!
//! Structural pattern mirrors `vector_handler.rs`.

use async_trait::async_trait;
use tracing::{debug, error};

use nodedb_types::Surrogate;

use super::session::SyncSession;
use super::wire::*;
use crate::types::{DatabaseId, TenantId, TraceId, VShardId};

// ── Dispatcher trait ─────────────────────────────────────────────────────────

/// Encapsulates async Data Plane dispatch for FTS index/delete.
#[async_trait]
pub trait FtsDispatcher: Send + Sync {
    /// Index a document's text on the Data Plane.
    async fn dispatch_index(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        surrogate: Surrogate,
        text: String,
    ) -> crate::Result<()>;

    /// Remove a document from the FTS index on the Data Plane.
    async fn dispatch_delete(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        surrogate: Surrogate,
    ) -> crate::Result<()>;

    /// Assign a stable surrogate for `(collection, doc_id)`.
    fn assign_surrogate(&self, collection: &str, doc_id: &str) -> crate::Result<Surrogate>;
}

// ── SharedState adapter ──────────────────────────────────────────────────────

/// Production dispatcher: routes FTS ops to the Data Plane via the SPSC bridge.
pub struct SharedStateFtsDispatcher<'a> {
    pub shared: &'a crate::control::state::SharedState,
}

#[async_trait]
impl<'a> FtsDispatcher for SharedStateFtsDispatcher<'a> {
    async fn dispatch_index(
        &self,
        tenant_id: TenantId,
        vshard: VShardId,
        collection: String,
        surrogate: Surrogate,
        text: String,
    ) -> crate::Result<()> {
        use crate::bridge::envelope::PhysicalPlan;
        use crate::control::server::dispatch_utils::dispatch_to_data_plane_with_source;
        use crate::event::EventSource;
        use nodedb_physical::physical_plan::TextOp;

        let plan = PhysicalPlan::Text(TextOp::FtsIndexDoc {
            collection,
            surrogate,
            text,
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
    ) -> crate::Result<()> {
        use crate::bridge::envelope::PhysicalPlan;
        use crate::control::server::dispatch_utils::dispatch_to_data_plane_with_source;
        use crate::event::EventSource;
        use nodedb_physical::physical_plan::TextOp;

        let plan = PhysicalPlan::Text(TextOp::FtsDeleteDoc {
            collection,
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
pub struct NoOpFtsDispatcher;

#[async_trait]
impl FtsDispatcher for NoOpFtsDispatcher {
    async fn dispatch_index(
        &self,
        _tenant_id: TenantId,
        _vshard: VShardId,
        _collection: String,
        _surrogate: Surrogate,
        _text: String,
    ) -> crate::Result<()> {
        Err(crate::Error::Internal {
            detail: "FTS index routed through path lacking SharedState; \
                     check listener wiring — index was ACKed but NOT applied"
                .to_string(),
        })
    }

    async fn dispatch_delete(
        &self,
        _tenant_id: TenantId,
        _vshard: VShardId,
        _collection: String,
        _surrogate: Surrogate,
    ) -> crate::Result<()> {
        Err(crate::Error::Internal {
            detail: "FTS delete routed through path lacking SharedState; \
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
    /// Process a `FtsIndexMsg`: allocate surrogate, dispatch to Data Plane,
    /// return an ACK frame.
    pub async fn handle_fts_index<D: FtsDispatcher>(
        &mut self,
        msg: &FtsIndexMsg,
        dispatcher: &D,
    ) -> Option<SyncFrame> {
        self.last_activity = std::time::Instant::now();

        if !self.authenticated {
            let ack = FtsIndexAckMsg {
                collection: msg.collection.clone(),
                doc_id: msg.doc_id.clone(),
                batch_id: msg.batch_id,
                accepted: false,
                reject_reason: Some("unauthenticated".to_string()),
            };
            return SyncFrame::try_encode(SyncMessageType::FtsIndexAck, &ack);
        }

        if msg.text.is_empty() {
            // Empty text — nothing to index; ACK immediately.
            let ack = FtsIndexAckMsg {
                collection: msg.collection.clone(),
                doc_id: msg.doc_id.clone(),
                batch_id: msg.batch_id,
                accepted: true,
                reject_reason: None,
            };
            return SyncFrame::try_encode(SyncMessageType::FtsIndexAck, &ack);
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
                    "fts sync: surrogate assignment failed"
                );
                let ack = FtsIndexAckMsg {
                    collection: msg.collection.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(format!("surrogate assignment failed: {e}")),
                };
                return SyncFrame::try_encode(SyncMessageType::FtsIndexAck, &ack);
            }
        };

        let tenant_id = self.tenant_id.unwrap_or(TenantId::new(0));
        let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, &msg.collection);

        debug!(
            session = %self.session_id,
            collection = %msg.collection,
            doc_id = %msg.doc_id,
            batch_id = msg.batch_id,
            lite_id = %msg.lite_id,
            "fts index: dispatching to Data Plane"
        );

        match dispatcher
            .dispatch_index(
                tenant_id,
                vshard,
                msg.collection.clone(),
                surrogate,
                msg.text.clone(),
            )
            .await
        {
            Ok(()) => {
                self.mutations_processed += 1;
                let ack = FtsIndexAckMsg {
                    collection: msg.collection.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: true,
                    reject_reason: None,
                };
                SyncFrame::try_encode(SyncMessageType::FtsIndexAck, &ack)
            }
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    doc_id = %msg.doc_id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "fts index dispatch failed"
                );
                let ack = FtsIndexAckMsg {
                    collection: msg.collection.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(e.to_string()),
                };
                SyncFrame::try_encode(SyncMessageType::FtsIndexAck, &ack)
            }
        }
    }

    /// Process a `FtsDeleteMsg`: look up surrogate, dispatch tombstone to
    /// Data Plane, return an ACK frame.
    pub async fn handle_fts_delete<D: FtsDispatcher>(
        &mut self,
        msg: &FtsDeleteMsg,
        dispatcher: &D,
    ) -> Option<SyncFrame> {
        self.last_activity = std::time::Instant::now();

        if !self.authenticated {
            let ack = FtsDeleteAckMsg {
                collection: msg.collection.clone(),
                doc_id: msg.doc_id.clone(),
                batch_id: msg.batch_id,
                accepted: false,
                reject_reason: Some("unauthenticated".to_string()),
            };
            return SyncFrame::try_encode(SyncMessageType::FtsDeleteAck, &ack);
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
                    "fts sync: surrogate lookup failed for delete"
                );
                let ack = FtsDeleteAckMsg {
                    collection: msg.collection.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(format!("surrogate lookup failed: {e}")),
                };
                return SyncFrame::try_encode(SyncMessageType::FtsDeleteAck, &ack);
            }
        };

        let tenant_id = self.tenant_id.unwrap_or(TenantId::new(0));
        let vshard = VShardId::from_collection_in_database(DatabaseId::DEFAULT, &msg.collection);

        debug!(
            session = %self.session_id,
            collection = %msg.collection,
            doc_id = %msg.doc_id,
            batch_id = msg.batch_id,
            lite_id = %msg.lite_id,
            "fts delete: dispatching to Data Plane"
        );

        match dispatcher
            .dispatch_delete(tenant_id, vshard, msg.collection.clone(), surrogate)
            .await
        {
            Ok(()) => {
                self.mutations_processed += 1;
                let ack = FtsDeleteAckMsg {
                    collection: msg.collection.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: true,
                    reject_reason: None,
                };
                SyncFrame::try_encode(SyncMessageType::FtsDeleteAck, &ack)
            }
            Err(e) => {
                error!(
                    session = %self.session_id,
                    collection = %msg.collection,
                    doc_id = %msg.doc_id,
                    batch_id = msg.batch_id,
                    error = %e,
                    "fts delete dispatch failed"
                );
                let ack = FtsDeleteAckMsg {
                    collection: msg.collection.clone(),
                    doc_id: msg.doc_id.clone(),
                    batch_id: msg.batch_id,
                    accepted: false,
                    reject_reason: Some(e.to_string()),
                };
                SyncFrame::try_encode(SyncMessageType::FtsDeleteAck, &ack)
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
        index_calls: MockCallLog,
        delete_calls: MockCallLog,
        result: crate::Result<()>,
    }

    impl MockDispatcher {
        fn ok() -> (Self, MockCallLog, MockCallLog) {
            let indexes = Arc::new(Mutex::new(Vec::new()));
            let deletes = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    index_calls: indexes.clone(),
                    delete_calls: deletes.clone(),
                    result: Ok(()),
                },
                indexes,
                deletes,
            )
        }

        fn err() -> Self {
            Self {
                index_calls: Arc::new(Mutex::new(Vec::new())),
                delete_calls: Arc::new(Mutex::new(Vec::new())),
                result: Err(crate::Error::Internal {
                    detail: "mock failure".to_string(),
                }),
            }
        }
    }

    #[async_trait]
    impl FtsDispatcher for MockDispatcher {
        async fn dispatch_index(
            &self,
            tenant_id: TenantId,
            _vshard: VShardId,
            collection: String,
            _surrogate: Surrogate,
            _text: String,
        ) -> crate::Result<()> {
            self.index_calls
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

        async fn dispatch_delete(
            &self,
            tenant_id: TenantId,
            _vshard: VShardId,
            collection: String,
            _surrogate: Surrogate,
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
        SyncSession::new("test-fts-session".to_string())
    }

    fn make_index_msg(collection: &str, doc_id: &str, text: &str) -> FtsIndexMsg {
        FtsIndexMsg {
            lite_id: "lite-test".to_string(),
            collection: collection.to_string(),
            doc_id: doc_id.to_string(),
            text: text.to_string(),
            batch_id: 1,
        }
    }

    fn make_delete_msg(collection: &str, doc_id: &str) -> FtsDeleteMsg {
        FtsDeleteMsg {
            lite_id: "lite-test".to_string(),
            collection: collection.to_string(),
            doc_id: doc_id.to_string(),
            batch_id: 2,
        }
    }

    #[tokio::test]
    async fn unauthenticated_index_returns_rejection() {
        let mut session = make_session();
        let (mock, indexes, _) = MockDispatcher::ok();
        let msg = make_index_msg("docs", "d1", "hello world");

        let frame = session.handle_fts_index(&msg, &mock).await;
        assert!(frame.is_some());
        let ack: FtsIndexAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(indexes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn authenticated_index_dispatches_and_acks() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, indexes, _) = MockDispatcher::ok();
        let msg = make_index_msg("docs", "d1", "hello world");

        let frame = session.handle_fts_index(&msg, &mock).await;
        assert!(frame.is_some());
        let ack: FtsIndexAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.doc_id, "d1");
        assert_eq!(indexes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn empty_text_acks_without_dispatch() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, indexes, _) = MockDispatcher::ok();
        let msg = make_index_msg("docs", "d1", "");

        let frame = session.handle_fts_index(&msg, &mock).await;
        let ack: FtsIndexAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(ack.accepted);
        assert!(indexes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn index_dispatch_failure_rejects() {
        let mut session = make_session();
        session.authenticated = true;
        let mock = MockDispatcher::err();
        let msg = make_index_msg("docs", "d1", "hello");

        let frame = session.handle_fts_index(&msg, &mock).await;
        let ack: FtsIndexAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(ack.reject_reason.is_some());
    }

    #[tokio::test]
    async fn authenticated_delete_dispatches_and_acks() {
        let mut session = make_session();
        session.authenticated = true;
        let (mock, _, deletes) = MockDispatcher::ok();
        let msg = make_delete_msg("docs", "d1");

        let frame = session.handle_fts_delete(&msg, &mock).await;
        let ack: FtsDeleteAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.doc_id, "d1");
        assert_eq!(deletes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unauthenticated_delete_returns_rejection() {
        let mut session = make_session();
        let (mock, _, deletes) = MockDispatcher::ok();
        let msg = make_delete_msg("docs", "d1");

        let frame = session.handle_fts_delete(&msg, &mock).await;
        let ack: FtsDeleteAckMsg = frame.unwrap().decode_body().unwrap();
        assert!(!ack.accepted);
        assert!(deletes.lock().unwrap().is_empty());
    }
}
