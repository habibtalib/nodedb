// SPDX-License-Identifier: BUSL-1.1

//! WebSocket session handler for NodeDB-Lite sync connections.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracing::{info, warn};

use nodedb_types::sync::wire::array::{
    ArrayAckMsg, ArrayCatchupRequestMsg, ArrayDeltaBatchMsg, ArrayDeltaMsg, ArrayRejectMsg,
    ArraySchemaSyncMsg, ArraySnapshotChunkMsg, ArraySnapshotMsg,
};

use super::listener::SyncListenerState;
use super::wire::{
    ColumnarInsertMsg, DeltaPushMsg, FtsDeleteMsg, FtsIndexMsg, PresenceUpdateMsg,
    SpatialDeleteMsg, SpatialInsertMsg, SyncMessageType, TimeseriesPushMsg, VectorDeleteMsg,
    VectorInsertMsg,
};

use crate::control::state::SharedState;

/// Handle one sync session with full RLS, audit, DLQ wired in.
pub(super) async fn handle_sync_session(
    mut ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    addr: SocketAddr,
    state: &SyncListenerState,
    shared: Option<Arc<SharedState>>,
) {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let session_id = format!(
        "sync-{addr}-{}",
        state.connections_accepted.load(Ordering::Relaxed)
    );
    let mut session =
        super::session::SyncSession::with_rate_limit(session_id.clone(), &state.config.rate_limit);
    session.device_metadata.remote_addr = addr.to_string();

    let jwt_validator =
        crate::control::security::jwt::JwtValidator::new(state.config.jwt_config.clone());

    let mut crdt_delivery_rx: Option<
        tokio::sync::mpsc::Receiver<crate::event::crdt_sync::types::OutboundDelta>,
    > = None;
    let mut crdt_control_rx: Option<
        tokio::sync::mpsc::Receiver<nodedb_types::sync::wire::SyncFrame>,
    > = None;
    let mut crdt_registered = false;

    let mut presence_rx: Option<tokio::sync::mpsc::Receiver<std::sync::Arc<Vec<u8>>>> = None;
    let mut presence_registered = false;

    let array_inbound: Option<Arc<crate::control::array_sync::OriginArrayInbound>> =
        shared.as_ref().map(|s| {
            let engine = Arc::new(crate::control::array_sync::OriginApplyEngine::new(
                Arc::clone(&s.array_sync_schemas),
                Arc::clone(&s.array_sync_op_log),
            ));
            let fanout = Arc::new(crate::control::array_sync::ArrayFanout::new(
                Arc::clone(&s.shape_registry),
                Arc::clone(&s.array_delivery),
                Arc::clone(&s.array_subscriber_cursors),
                Arc::clone(&s.array_snapshot_hlcs),
                Arc::clone(&s.array_merger_registry),
                0,
                0,
            ));
            let inbound = crate::control::array_sync::OriginArrayInbound::new(
                engine,
                Arc::clone(&s.array_sync_schemas),
                Arc::clone(s),
                crate::types::TenantId::new(0),
            )
            .with_observer(fanout);
            Arc::new(inbound)
        });

    let mut array_delivery_rx: Option<tokio::sync::mpsc::Receiver<Vec<u8>>> = None;
    let mut array_delivery_registered = false;

    let mut definition_sync_rx: Option<tokio::sync::mpsc::Receiver<Vec<u8>>> = None;
    let mut definition_sync_registered = false;

    loop {
        // Flush any outbound definition-sync frames before blocking. This
        // handles the window between registration and the next WS message.
        if let Some(ref mut rx) = definition_sync_rx {
            while let Ok(frame_bytes) = rx.try_recv() {
                if ws.send(Message::Binary(frame_bytes.into())).await.is_err() {
                    return;
                }
            }
        }

        // Await the next inbound message OR a definition-sync frame, whichever
        // arrives first.  Without this select! the handler would block on
        // ws.next() indefinitely when no client traffic is expected, starving
        // the server-push delivery path.
        let msg_result = if let Some(ref mut rx) = definition_sync_rx {
            tokio::select! {
                biased;
                ws_msg = ws.next() => {
                    match ws_msg {
                        Some(r) => r,
                        None => break,
                    }
                }
                frame_bytes = rx.recv() => {
                    match frame_bytes {
                        Some(bytes) => {
                            if ws.send(Message::Binary(bytes.into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        None => break,
                    }
                }
            }
        } else {
            match ws.next().await {
                Some(r) => r,
                None => break,
            }
        };

        match msg_result {
            Ok(Message::Binary(data)) => {
                if let Some(frame) = super::wire::SyncFrame::from_bytes(&data) {
                    if frame.msg_type == SyncMessageType::ShapeSubscribe
                        && let Some(shared) = shared.as_ref()
                        && let Some(response) = super::async_dispatch::handle_shape_subscribe_async(
                            shared, &session, &frame,
                        )
                        .await
                    {
                        if session.authenticated
                            && presence_registered
                            && let Some(sub_msg) =
                                frame.decode_body::<super::wire::ShapeSubscribeMsg>()
                            && let Some(coll) = sub_msg.shape.collection()
                        {
                            let channel = format!("shape:{coll}");
                            shared
                                .presence
                                .write()
                                .await
                                .subscribe_to_channel(&session_id, &channel);
                        }

                        if ws
                            .send(Message::Binary(response.to_bytes().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::PresenceUpdate
                        && session.authenticated
                        && let Some(shared) = shared.as_ref()
                    {
                        if let Some(msg) = frame.decode_body::<PresenceUpdateMsg>() {
                            let user_id = session.username.as_deref().unwrap_or("anonymous");
                            let mut mgr = shared.presence.write().await;
                            let outbound = mgr.handle_update(&session_id, user_id, &msg);
                            let senders = mgr.senders().clone();
                            drop(mgr);
                            outbound.send_all(&senders);
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::TimeseriesPush {
                        if let Some(ts_msg) = frame.decode_body::<TimeseriesPushMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::timeseries_handler::SharedStateDispatcher { shared };
                                session.handle_timeseries_push(&ts_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::timeseries_handler::NoOpDispatcher;
                                session.handle_timeseries_push(&ts_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::ColumnarInsert {
                        if let Some(col_msg) = frame.decode_body::<ColumnarInsertMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::columnar_handler::SharedStateColumnarDispatcher {
                                        shared,
                                    };
                                session.handle_columnar_insert(&col_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::columnar_handler::NoOpColumnarDispatcher;
                                session.handle_columnar_insert(&col_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::VectorInsert {
                        if let Some(vec_msg) = frame.decode_body::<VectorInsertMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::vector_handler::SharedStateVectorDispatcher { shared };
                                session.handle_vector_insert(&vec_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::vector_handler::NoOpVectorDispatcher;
                                session.handle_vector_insert(&vec_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::VectorDelete {
                        if let Some(vec_msg) = frame.decode_body::<VectorDeleteMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::vector_handler::SharedStateVectorDispatcher { shared };
                                session.handle_vector_delete(&vec_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::vector_handler::NoOpVectorDispatcher;
                                session.handle_vector_delete(&vec_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::FtsIndex {
                        if let Some(fts_msg) = frame.decode_body::<FtsIndexMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::fts_handler::SharedStateFtsDispatcher { shared };
                                session.handle_fts_index(&fts_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::fts_handler::NoOpFtsDispatcher;
                                session.handle_fts_index(&fts_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::FtsDelete {
                        if let Some(fts_msg) = frame.decode_body::<FtsDeleteMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::fts_handler::SharedStateFtsDispatcher { shared };
                                session.handle_fts_delete(&fts_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::fts_handler::NoOpFtsDispatcher;
                                session.handle_fts_delete(&fts_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::SpatialInsert {
                        if let Some(sp_msg) = frame.decode_body::<SpatialInsertMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::spatial_handler::SharedStateSpatialDispatcher { shared };
                                session.handle_spatial_insert(&sp_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::spatial_handler::NoOpSpatialDispatcher;
                                session.handle_spatial_insert(&sp_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if frame.msg_type == SyncMessageType::SpatialDelete {
                        if let Some(sp_msg) = frame.decode_body::<SpatialDeleteMsg>() {
                            let ack = if let Some(shared) = shared.as_ref() {
                                let dispatcher =
                                    super::spatial_handler::SharedStateSpatialDispatcher { shared };
                                session.handle_spatial_delete(&sp_msg, &dispatcher).await
                            } else {
                                let dispatcher = super::spatial_handler::NoOpSpatialDispatcher;
                                session.handle_spatial_delete(&sp_msg, &dispatcher).await
                            };
                            if let Some(ack) = ack {
                                let ack_bytes = ack.to_bytes();
                                if ws.send(Message::Binary(ack_bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if matches!(
                        frame.msg_type,
                        SyncMessageType::ArrayDelta
                            | SyncMessageType::ArrayDeltaBatch
                            | SyncMessageType::ArraySnapshot
                            | SyncMessageType::ArraySnapshotChunk
                            | SyncMessageType::ArraySchema
                            | SyncMessageType::ArrayAck
                            | SyncMessageType::ArrayReject
                            | SyncMessageType::ArrayCatchupRequest
                    ) {
                        if frame.msg_type == SyncMessageType::ArrayReject {
                            if let Some(msg) = frame.decode_body::<ArrayRejectMsg>() {
                                warn!(
                                    session = %session_id,
                                    array = %msg.array,
                                    reason = ?msg.reason,
                                    "sync: received ArrayReject (outbound-only); ignoring"
                                );
                            }
                            continue;
                        }

                        if let Some(inbound) = &array_inbound {
                            let reject_frame = dispatch_array_frame(&frame, inbound).await;
                            if let Some(f) = reject_frame
                                && ws.send(Message::Binary(f.to_bytes().into())).await.is_err()
                            {
                                break;
                            }
                        }
                        continue;
                    }

                    let response = if let Some(shared) = shared.as_ref() {
                        let rls_store = &shared.rls;
                        let mut audit = shared.audit.lock().unwrap_or_else(|p| p.into_inner());
                        let mut dlq = shared.sync_dlq.lock().unwrap_or_else(|p| p.into_inner());
                        session.process_frame(
                            &frame,
                            &jwt_validator,
                            Some(rls_store),
                            Some(&mut audit),
                            Some(&mut dlq),
                            Some(&shared.epoch_tracker),
                        )
                    } else {
                        session.process_frame(&frame, &jwt_validator, None, None, None, None)
                    };

                    if let Some(response) = response {
                        let final_response = if response.msg_type == SyncMessageType::DeltaAck
                            && let Some(shared) = shared.as_ref()
                            && let Some(delta_msg) = frame.decode_body::<DeltaPushMsg>()
                        {
                            super::async_dispatch::validate_delta_constraints(
                                shared, &delta_msg, response,
                            )
                            .await
                        } else {
                            Some(response)
                        };

                        if let Some(r) = final_response
                            && ws.send(Message::Binary(r.to_bytes().into())).await.is_err()
                        {
                            break;
                        }
                    }
                }
            }
            Ok(Message::Ping(data)) => {
                let Ok(_) = ws.send(Message::Pong(data)).await else {
                    break;
                };
            }
            Ok(Message::Close(_)) => break,
            Err(e) => {
                warn!(session = %session_id, error = %e, "sync: WebSocket error");
                break;
            }
            _ => {}
        }

        if session.authenticated
            && !crdt_registered
            && let Some(shared) = shared.as_ref()
        {
            let tenant_id = session.tenant_id.map(|t| t.as_u64()).unwrap_or(0);
            let peer_id = session.device_metadata.peer_id;
            let config = crate::event::crdt_sync::types::DeliveryConfig::default();
            let (drx, crx) = shared.crdt_sync_delivery.register(
                session_id.clone(),
                peer_id,
                tenant_id,
                Vec::new(),
                &config,
            );
            crdt_delivery_rx = Some(drx);
            crdt_control_rx = Some(crx);
            crdt_registered = true;
        }

        if session.authenticated
            && !array_delivery_registered
            && let Some(shared) = shared.as_ref()
        {
            let rx = shared.array_delivery.register(session_id.clone());
            array_delivery_rx = Some(rx);
            array_delivery_registered = true;
        }

        if session.authenticated
            && !definition_sync_registered
            && let Some(shared) = shared.as_ref()
        {
            let rx = shared.definition_sync_fanout.register(session_id.clone());
            definition_sync_rx = Some(rx);
            definition_sync_registered = true;
        }

        if session.authenticated
            && !presence_registered
            && let Some(shared) = shared.as_ref()
        {
            let (tx, rx) = tokio::sync::mpsc::channel(256);
            shared
                .presence
                .write()
                .await
                .register_session(session_id.clone(), super::presence::SessionSender::new(tx));
            presence_rx = Some(rx);
            presence_registered = true;
        }

        if let Some(ref mut rx) = presence_rx {
            while let Ok(bytes) = rx.try_recv() {
                if ws
                    .send(Message::Binary((*bytes).clone().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }

        if let Some(ref mut rx) = array_delivery_rx {
            while let Ok(frame_bytes) = rx.try_recv() {
                if ws.send(Message::Binary(frame_bytes.into())).await.is_err() {
                    break;
                }
            }
        }

        if let Some(ref mut rx) = definition_sync_rx {
            while let Ok(frame_bytes) = rx.try_recv() {
                if ws.send(Message::Binary(frame_bytes.into())).await.is_err() {
                    break;
                }
            }
        }

        if let Some(ref mut rx) = crdt_control_rx {
            while let Ok(frame) = rx.try_recv() {
                if ws
                    .send(Message::Binary(frame.to_bytes().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }

        if let Some(ref mut rx) = crdt_delivery_rx {
            while let Ok(delta) = rx.try_recv() {
                let push_msg = nodedb_types::sync::wire::DeltaPushMsg {
                    collection: delta.collection,
                    document_id: delta.document_id,
                    delta: delta.payload,
                    peer_id: delta.peer_id,
                    mutation_id: delta.sequence,
                    checksum: 0,
                    device_valid_time_ms: None,
                };
                if let Some(frame) = nodedb_types::sync::wire::SyncFrame::new_msgpack(
                    nodedb_types::sync::wire::SyncMessageType::DeltaPush,
                    &push_msg,
                ) && ws
                    .send(Message::Binary(frame.to_bytes().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }

        if session.idle_secs() > state.config.idle_timeout_secs {
            info!(session = %session_id, "sync: idle timeout, closing");
            break;
        }
    }

    if crdt_registered && let Some(shared) = shared.as_ref() {
        shared.crdt_sync_delivery.unregister(&session_id);
    }

    if array_delivery_registered && let Some(shared) = shared.as_ref() {
        shared.array_delivery.unregister(&session_id);
        shared.array_subscriber_cursors.remove_session(&session_id);
    }

    if definition_sync_registered && let Some(shared) = shared.as_ref() {
        shared.definition_sync_fanout.unregister(&session_id);
    }

    if presence_registered && let Some(shared) = shared.as_ref() {
        let mut mgr = shared.presence.write().await;
        let outbound = mgr.unregister_session(&session_id);
        let senders = mgr.senders().clone();
        drop(mgr);
        outbound.send_all(&senders);
    }

    info!(
        session = %session_id,
        mutations = session.mutations_processed,
        rejected = session.mutations_rejected,
        silent_dropped = session.mutations_silent_dropped,
        uptime_secs = session.uptime_secs(),
        "sync: session closed"
    );
}

async fn dispatch_array_frame(
    frame: &super::wire::SyncFrame,
    inbound: &crate::control::array_sync::OriginArrayInbound,
) -> Option<super::wire::SyncFrame> {
    match frame.msg_type {
        SyncMessageType::ArrayDelta => {
            if let Some(msg) = frame.decode_body::<ArrayDeltaMsg>() {
                match inbound.handle_delta(&msg).await {
                    Ok(_) => None,
                    Err(Some(r)) => {
                        super::wire::SyncFrame::try_encode(SyncMessageType::ArrayReject, &r)
                    }
                    Err(None) => None,
                }
            } else {
                None
            }
        }
        SyncMessageType::ArrayDeltaBatch => {
            if let Some(msg) = frame.decode_body::<ArrayDeltaBatchMsg>() {
                let outcomes = inbound.handle_delta_batch(&msg).await;
                outcomes.into_iter().find_map(|r| match r {
                    Err(Some(reject)) => {
                        super::wire::SyncFrame::try_encode(SyncMessageType::ArrayReject, &reject)
                    }
                    _ => None,
                })
            } else {
                None
            }
        }
        SyncMessageType::ArraySnapshot => {
            if let Some(msg) = frame.decode_body::<ArraySnapshotMsg>() {
                match inbound.handle_snapshot_header(&msg) {
                    Ok(_) => None,
                    Err(Some(r)) => {
                        super::wire::SyncFrame::try_encode(SyncMessageType::ArrayReject, &r)
                    }
                    Err(None) => None,
                }
            } else {
                None
            }
        }
        SyncMessageType::ArraySnapshotChunk => {
            if let Some(msg) = frame.decode_body::<ArraySnapshotChunkMsg>() {
                match inbound.handle_snapshot_chunk(&msg).await {
                    Ok(_) => None,
                    Err(Some(r)) => {
                        super::wire::SyncFrame::try_encode(SyncMessageType::ArrayReject, &r)
                    }
                    Err(None) => None,
                }
            } else {
                None
            }
        }
        SyncMessageType::ArraySchema => {
            if let Some(msg) = frame.decode_body::<ArraySchemaSyncMsg>() {
                match inbound.handle_schema(&msg).await {
                    Ok(_) => None,
                    Err(Some(r)) => {
                        super::wire::SyncFrame::try_encode(SyncMessageType::ArrayReject, &r)
                    }
                    Err(None) => None,
                }
            } else {
                None
            }
        }
        SyncMessageType::ArrayAck => {
            if let Some(msg) = frame.decode_body::<ArrayAckMsg>() {
                let _ = inbound.handle_ack(&msg);
            }
            None
        }
        SyncMessageType::ArrayCatchupRequest => {
            if let Some(msg) = frame.decode_body::<ArrayCatchupRequestMsg>() {
                // session_id not available here; pass empty string (used only for logging)
                let _ = inbound.handle_catchup_request(&msg, "");
            }
            None
        }
        _ => None,
    }
}
