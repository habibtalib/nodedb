// SPDX-License-Identifier: BUSL-1.1

//! [`DefinitionSyncFanout`] — broadcast outbound `DefinitionSync` (0x70)
//! frames to every connected Lite session.
//!
//! Architecture mirrors [`ArrayDeliveryRegistry`]: each authenticated
//! session registers a bounded `mpsc` receiver here; the WebSocket send
//! loop drains it. On DDL commit, the Origin DDL handler calls
//! [`DefinitionSyncFanout::broadcast`] which fan-outs the encoded frame
//! to every registered session via `try_send` (back-pressure: drop and
//! let Lite re-sync on reconnect).
//!
//! This lives entirely on the Control Plane (Tokio, `Send + Sync`).

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use nodedb_types::sync::wire::{DefinitionSyncMsg, SyncFrame, SyncMessageType};

/// Capacity of each session's outbound definition-sync channel.
///
/// Definitions are rare compared to delta ops, so a small buffer is
/// sufficient. If a session's channel is full the frame is dropped;
/// the Lite device will re-sync its catalog on reconnect (resync
/// request round-trip is cheap for definitions).
const CHANNEL_CAPACITY: usize = 256;

/// A pre-encoded binary frame ready to write to the WebSocket.
type DefinitionFrame = Vec<u8>;

/// Fan-out registry for outbound `DefinitionSync` (0x70) frames.
///
/// Thread-safe: `register` / `unregister` from the sync listener task;
/// `broadcast` from DDL handlers after a successful catalog commit.
pub struct DefinitionSyncFanout {
    sessions: RwLock<HashMap<String, mpsc::Sender<DefinitionFrame>>>,
    /// Monotonic count of sessions registered since startup.
    pub sessions_registered: AtomicU64,
    /// Monotonic count of frames dropped due to back-pressure.
    pub frames_dropped: AtomicU64,
}

impl Default for DefinitionSyncFanout {
    fn default() -> Self {
        Self::new()
    }
}

impl DefinitionSyncFanout {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            sessions_registered: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
        }
    }

    /// Register a session and return the `Receiver` end of its delivery
    /// channel. The sync listener's send loop drains this on each iteration.
    pub fn register(&self, session_id: String) -> mpsc::Receiver<DefinitionFrame> {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
        sessions.insert(session_id.clone(), tx);
        self.sessions_registered.fetch_add(1, Ordering::Relaxed);
        info!(session = %session_id, "definition_sync_fanout: session registered");
        rx
    }

    /// Unregister a disconnected session and drop its sender.
    pub fn unregister(&self, session_id: &str) {
        let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
        if sessions.remove(session_id).is_some() {
            debug!(session = %session_id, "definition_sync_fanout: session unregistered");
        }
    }

    /// Encode and broadcast a `DefinitionSyncMsg` to all registered sessions.
    ///
    /// Uses `try_send` so callers are never blocked. If a session's channel is
    /// full, the frame is dropped for that session (`frames_dropped` is
    /// incremented). The Lite device recovers by re-requesting the catalog on
    /// the next reconnect.
    ///
    /// If encoding fails, the broadcast is silently skipped and an error is
    /// logged — the DDL commit itself has already succeeded, so we must not
    /// roll it back.
    pub fn broadcast(&self, msg: &DefinitionSyncMsg) {
        let frame = match SyncFrame::try_encode(SyncMessageType::DefinitionSync, msg) {
            Some(f) => f.to_bytes(),
            None => {
                // Logging already done by try_encode.
                return;
            }
        };

        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        for (session_id, tx) in sessions.iter() {
            match tx.try_send(frame.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.frames_dropped.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        session = %session_id,
                        definition_type = %msg.definition_type,
                        name = %msg.name,
                        "definition_sync_fanout: channel full — frame dropped; Lite will re-sync on reconnect"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!(
                        session = %session_id,
                        "definition_sync_fanout: session channel closed (disconnected)"
                    );
                }
            }
        }
    }

    /// Number of currently registered sessions.
    pub fn active_sessions(&self) -> usize {
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_receive() {
        let fanout = DefinitionSyncFanout::new();
        let mut rx = fanout.register("s1".into());

        let msg = DefinitionSyncMsg {
            definition_type: "function".into(),
            name: "my_fn".into(),
            action: "put".into(),
            payload: vec![],
        };
        fanout.broadcast(&msg);

        let frame_bytes = rx.recv().await.expect("should receive frame");
        let frame = SyncFrame::from_bytes(&frame_bytes).expect("decode frame");
        assert_eq!(frame.msg_type, SyncMessageType::DefinitionSync);
        let decoded: DefinitionSyncMsg = frame.decode_body().expect("decode body");
        assert_eq!(decoded.name, "my_fn");
    }

    #[tokio::test]
    async fn unregister_drops_sender() {
        let fanout = DefinitionSyncFanout::new();
        let mut rx = fanout.register("s1".into());
        fanout.unregister("s1");

        let msg = DefinitionSyncMsg {
            definition_type: "function".into(),
            name: "x".into(),
            action: "put".into(),
            payload: vec![],
        };
        fanout.broadcast(&msg); // No-op: session gone.
        assert_eq!(rx.recv().await, None); // Channel closed.
    }

    #[test]
    fn broadcast_unknown_session_is_noop() {
        let fanout = DefinitionSyncFanout::new();
        let msg = DefinitionSyncMsg {
            definition_type: "procedure".into(),
            name: "p".into(),
            action: "delete".into(),
            payload: vec![],
        };
        fanout.broadcast(&msg); // Should not panic.
        assert_eq!(fanout.frames_dropped.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn broadcast_to_multiple_sessions() {
        let fanout = DefinitionSyncFanout::new();
        let mut rx1 = fanout.register("s1".into());
        let mut rx2 = fanout.register("s2".into());

        let msg = DefinitionSyncMsg {
            definition_type: "trigger".into(),
            name: "t1".into(),
            action: "put".into(),
            payload: vec![1, 2, 3],
        };
        fanout.broadcast(&msg);

        // Both sessions receive the frame.
        let f1 = rx1.recv().await.expect("s1 should receive frame");
        let f2 = rx2.recv().await.expect("s2 should receive frame");
        assert_eq!(f1, f2); // Same encoded bytes.
    }
}
