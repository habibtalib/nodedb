// SPDX-License-Identifier: BUSL-1.1

//! Audit-log consumer for the session-invalidation and user-change buses.
//!
//! `spawn_bus_consumer` launches a single Tokio task that:
//!
//! 1. Selects across the two broadcast receivers.
//! 2. On `SessionInvalidated`: writes an `AuditEvent::SessionRevoked` row
//!    (carrying the reason verbatim) **before** any connection-close logic
//!    so the row is durable even if the close fails.
//! 3. On `UserChanged`: writes an `AuditEvent::PrivilegeChange` row
//!    (credential mutations are privilege changes).
//! 4. On receiver lag (`RecvError::Lagged`): writes an
//!    `AuditEvent::AuditBusLagged` row so operators can detect dropped events.
//! 5. On `RecvError::Closed` (both senders dropped): exits cleanly.
//!
//! The returned `JoinHandle` is stored as `SharedState::bus_consumer_handle`
//! following the `array_gc_handle` pattern.  Graceful shutdown drops the
//! `SessionInvalidationBus` / `UserChangeBus` senders, which eventually
//! closes both receivers and causes the task to exit.

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::control::security::audit::event::AuditEvent;
use crate::control::security::audit::log::AuditLog;

use super::session_invalidation::SessionInvalidated;
use super::user_change::UserChanged;

/// Spawn the bus consumer task.
///
/// - `si_rx`: pre-subscribed receiver for the session-invalidation bus.
/// - `uc_rx`: pre-subscribed receiver for the user-change bus.
/// - `audit`: shared audit log.
///
/// Returns a `JoinHandle` suitable for storing in `SharedState::bus_consumer_handle`.
pub fn spawn_bus_consumer(
    si_rx: broadcast::Receiver<SessionInvalidated>,
    uc_rx: broadcast::Receiver<UserChanged>,
    audit: Arc<Mutex<AuditLog>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_bus_consumer(si_rx, uc_rx, audit))
}

async fn run_bus_consumer(
    mut si_rx: broadcast::Receiver<SessionInvalidated>,
    mut uc_rx: broadcast::Receiver<UserChanged>,
    audit: Arc<Mutex<AuditLog>>,
) {
    loop {
        tokio::select! {
            biased;

            result = si_rx.recv() => {
                match result {
                    Ok(event) => {
                        handle_session_invalidated(&audit, &event);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        record_lagged(
                            &audit,
                            &format!("session_invalidation_bus lagged; dropped={n}"),
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Session-invalidation sender was dropped; wait for
                        // the user-change bus to also close before exiting.
                        drain_user_change_to_closed(&mut uc_rx, &audit).await;
                        return;
                    }
                }
            }

            result = uc_rx.recv() => {
                match result {
                    Ok(event) => {
                        handle_user_changed(&audit, &event);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        record_lagged(
                            &audit,
                            &format!("user_change_bus lagged; dropped={n}"),
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // User-change sender was dropped; drain the remaining
                        // session-invalidation events then exit.
                        drain_session_invalidation_to_closed(&mut si_rx, &audit).await;
                        return;
                    }
                }
            }
        }
    }
}

/// Drain `si_rx` until closed, then return.
async fn drain_session_invalidation_to_closed(
    si_rx: &mut broadcast::Receiver<SessionInvalidated>,
    audit: &Arc<Mutex<AuditLog>>,
) {
    loop {
        match si_rx.recv().await {
            Ok(event) => handle_session_invalidated(audit, &event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                record_lagged(
                    audit,
                    &format!("session_invalidation_bus lagged; dropped={n}"),
                );
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Drain `uc_rx` until closed, then return.
async fn drain_user_change_to_closed(
    uc_rx: &mut broadcast::Receiver<UserChanged>,
    audit: &Arc<Mutex<AuditLog>>,
) {
    loop {
        match uc_rx.recv().await {
            Ok(event) => handle_user_changed(audit, &event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                record_lagged(audit, &format!("user_change_bus lagged; dropped={n}"));
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn handle_session_invalidated(audit: &Arc<Mutex<AuditLog>>, event: &SessionInvalidated) {
    let detail = format!("user_id={} reason={}", event.user_id, event.reason.as_str(),);
    if let Ok(mut log) = audit.lock() {
        log.record(
            AuditEvent::SessionRevoked,
            None,
            "session_invalidation_bus",
            &detail,
        );
    }
}

fn handle_user_changed(audit: &Arc<Mutex<AuditLog>>, event: &UserChanged) {
    let detail = format!("user_id={} credential_mutation", event.user_id);
    if let Ok(mut log) = audit.lock() {
        log.record(
            AuditEvent::PrivilegeChange,
            None,
            "user_change_bus",
            &detail,
        );
    }
}

fn record_lagged(audit: &Arc<Mutex<AuditLog>>, detail: &str) {
    if let Ok(mut log) = audit.lock() {
        log.record(AuditEvent::AuditBusLagged, None, "bus_consumer", detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::security::buses::session_invalidation::{
        SessionInvalidationBus, SessionInvalidationReason,
    };
    use crate::control::security::buses::user_change::UserChangeBus;

    fn make_audit() -> Arc<Mutex<AuditLog>> {
        Arc::new(Mutex::new(AuditLog::new(1_000)))
    }

    /// Produce a SessionInvalidated event, spin the consumer for one tick,
    /// then confirm the audit row is visible.
    #[tokio::test]
    async fn session_invalidated_produces_audit_row() {
        let audit = make_audit();
        let (si_bus, si_rx) = SessionInvalidationBus::new();
        let (uc_bus, uc_rx) = UserChangeBus::new();

        let handle = spawn_bus_consumer(si_rx, uc_rx, Arc::clone(&audit));

        si_bus.publish(SessionInvalidated {
            user_id: 99,
            reason: SessionInvalidationReason::UserDropped,
        });

        // Drop senders so consumer exits after processing the event.
        drop(si_bus);
        drop(uc_bus);
        handle.await.expect("consumer task panicked");

        let log = audit.lock().unwrap();
        let rows = log.query_by_event(&AuditEvent::SessionRevoked);
        assert_eq!(rows.len(), 1, "expected exactly one SessionRevoked row");
        assert!(
            rows[0].detail.contains("user_id=99"),
            "detail must contain user_id"
        );
        assert!(
            rows[0].detail.contains("UserDropped"),
            "detail must carry reason verbatim"
        );
    }

    /// Produce a UserChanged event, spin the consumer, confirm PrivilegeChange row.
    #[tokio::test]
    async fn user_changed_produces_audit_row() {
        let audit = make_audit();
        let (si_bus, si_rx) = SessionInvalidationBus::new();
        let (uc_bus, uc_rx) = UserChangeBus::new();

        let handle = spawn_bus_consumer(si_rx, uc_rx, Arc::clone(&audit));

        uc_bus.publish(UserChanged { user_id: 55 });

        drop(si_bus);
        drop(uc_bus);
        handle.await.expect("consumer task panicked");

        let log = audit.lock().unwrap();
        let rows = log.query_by_event(&AuditEvent::PrivilegeChange);
        assert_eq!(rows.len(), 1, "expected exactly one PrivilegeChange row");
        assert!(
            rows[0].detail.contains("user_id=55"),
            "detail must contain user_id"
        );
    }

    /// Verify that both buses produce audit rows in the same consumer run.
    #[tokio::test]
    async fn both_buses_produce_rows() {
        let audit = make_audit();
        let (si_bus, si_rx) = SessionInvalidationBus::new();
        let (uc_bus, uc_rx) = UserChangeBus::new();

        let handle = spawn_bus_consumer(si_rx, uc_rx, Arc::clone(&audit));

        si_bus.publish(SessionInvalidated {
            user_id: 1,
            reason: SessionInvalidationReason::UserDeactivated,
        });
        uc_bus.publish(UserChanged { user_id: 2 });

        drop(si_bus);
        drop(uc_bus);
        handle.await.expect("consumer task panicked");

        let log = audit.lock().unwrap();
        assert_eq!(
            log.query_by_event(&AuditEvent::SessionRevoked).len(),
            1,
            "one SessionRevoked row"
        );
        assert_eq!(
            log.query_by_event(&AuditEvent::PrivilegeChange).len(),
            1,
            "one PrivilegeChange row"
        );
    }
}
