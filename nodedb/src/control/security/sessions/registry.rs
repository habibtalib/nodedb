// SPDX-License-Identifier: BUSL-1.1

//! Active session registry.
//!
//! Tracks every authenticated connection (native, pgwire, HTTP) with its
//! kill signal and credential version at bind time.  Provides:
//!
//! - Bounded capacity (`max_active_sessions`; 0 = unlimited).  Over-cap
//!   returns [`SessionCapExceeded`] — no LRU eviction of live sessions.
//! - Per-user hard-revoke (`kill_sessions_for_user`) for DROP USER /
//!   deactivation paths.
//! - Per-IP kill for IP blacklist.
//! - `list_all` for `SHOW SESSIONS`.

use std::collections::HashMap;
use std::sync::{RwLock, atomic::AtomicU64};

use tokio::sync::watch;

use crate::control::security::time::now_secs;

/// Error returned when `register` would exceed `max_active_sessions`.
#[derive(Debug, thiserror::Error)]
#[error("max_active_sessions ({cap}) exceeded — rejecting new login")]
pub struct SessionCapExceeded {
    pub cap: usize,
}

/// Parameters for registering a new session.
pub struct SessionParams {
    pub user_id: u64,
    pub username: String,
    pub db_user: String,
    pub peer_addr: String,
    pub protocol: String,
    pub auth_method: String,
    pub tenant_id: u64,
    /// Per-user credential version at the time of authentication.
    pub credential_version: u64,
}

/// A registered session.
struct RegisteredSession {
    user_id: u64,
    db_user: String,
    peer_addr: String,
    protocol: String,
    auth_method: String,
    tenant_id: u64,
    connected_at: u64,
    last_active: AtomicU64,
    /// Send `true` to signal the session to terminate.
    kill_tx: watch::Sender<bool>,
    /// Credential version at bind time; used for `SHOW SESSIONS`.
    credential_version: u64,
}

/// Thread-safe registry of active authenticated sessions.
pub struct SessionRegistry {
    /// session_id → registered session.
    sessions: RwLock<HashMap<String, RegisteredSession>>,
    /// 0 = unlimited.
    max_sessions: usize,
}

impl SessionRegistry {
    /// Create an unbounded registry.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_sessions: 0,
        }
    }

    /// Create a registry with a session cap.  0 = unlimited.
    pub fn with_cap(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_sessions,
        }
    }

    /// Register a new session.
    ///
    /// Returns a kill-signal receiver the session loop must check at each
    /// request boundary.  Returns [`SessionCapExceeded`] when the registry
    /// is full.
    pub fn register(
        &self,
        session_id: &str,
        params: &SessionParams,
    ) -> Result<watch::Receiver<bool>, SessionCapExceeded> {
        let now = now_secs();
        let (kill_tx, kill_rx) = watch::channel(false);
        let entry = RegisteredSession {
            user_id: params.user_id,
            db_user: params.db_user.clone(),
            peer_addr: params.peer_addr.clone(),
            protocol: params.protocol.clone(),
            auth_method: params.auth_method.clone(),
            tenant_id: params.tenant_id,
            connected_at: now,
            last_active: AtomicU64::new(now),
            kill_tx,
            credential_version: params.credential_version,
        };

        let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
        if self.max_sessions > 0 && sessions.len() >= self.max_sessions {
            return Err(SessionCapExceeded {
                cap: self.max_sessions,
            });
        }
        sessions.insert(session_id.to_string(), entry);
        Ok(kill_rx)
    }

    /// Unregister a session on disconnect.
    pub fn unregister(&self, session_id: &str) {
        let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
        sessions.remove(session_id);
    }

    /// Kill all sessions for a specific user (hard revoke).
    /// Returns the number of sessions signalled.
    pub fn kill_sessions_for_user(&self, user_id: u64) -> usize {
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        let mut killed = 0;
        for s in sessions.values() {
            if s.user_id == user_id {
                let _ = s.kill_tx.send(true);
                killed += 1;
            }
        }
        killed
    }

    /// Kill all sessions matching a username string (for blacklist / emergency
    /// paths that identify users by name rather than numeric ID).
    pub fn kill_sessions_for_username(&self, username: &str) -> usize {
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        let mut killed = 0;
        for s in sessions.values() {
            if s.db_user == username {
                let _ = s.kill_tx.send(true);
                killed += 1;
            }
        }
        killed
    }

    /// Kill all sessions matching a peer-address prefix (IP blacklist).
    pub fn kill_sessions_for_ip(&self, peer_addr_prefix: &str) -> usize {
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        let mut killed = 0;
        for s in sessions.values() {
            if s.peer_addr.starts_with(peer_addr_prefix) {
                let _ = s.kill_tx.send(true);
                killed += 1;
            }
        }
        killed
    }

    /// Count active sessions, optionally filtered by numeric user ID.
    pub fn count(&self, user_filter: Option<u64>) -> usize {
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        match user_filter {
            Some(uid) => sessions.values().filter(|s| s.user_id == uid).count(),
            None => sessions.len(),
        }
    }

    /// Update last-active timestamp (called at each request boundary).
    pub fn touch(&self, session_id: &str) {
        let now = now_secs();
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = sessions.get(session_id) {
            s.last_active
                .store(now, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// List all active sessions for `SHOW SESSIONS`.
    pub fn list_all(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        sessions
            .iter()
            .map(|(id, s)| SessionInfo {
                session_id: id.clone(),
                user_id: s.user_id,
                db_user: s.db_user.clone(),
                auth_method: s.auth_method.clone(),
                connected_at: s.connected_at,
                last_active: s.last_active.load(std::sync::atomic::Ordering::Relaxed),
                client_ip: s.peer_addr.clone(),
                protocol: s.protocol.clone(),
                tenant_id: s.tenant_id,
                credential_version: s.credential_version,
            })
            .collect()
    }

    /// Returns the maximum session cap (0 = unlimited).
    pub fn cap(&self) -> usize {
        self.max_sessions
    }
}

impl From<SessionCapExceeded> for crate::Error {
    fn from(e: SessionCapExceeded) -> Self {
        crate::Error::SessionCapExceeded { cap: e.cap }
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Session info for `SHOW SESSIONS` output.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub user_id: u64,
    pub db_user: String,
    pub auth_method: String,
    pub connected_at: u64,
    pub last_active: u64,
    pub client_ip: String,
    pub protocol: String,
    pub tenant_id: u64,
    pub credential_version: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(user_id: u64, addr: &str, proto: &str) -> SessionParams {
        SessionParams {
            user_id,
            username: format!("user_{user_id}"),
            db_user: format!("user_{user_id}"),
            peer_addr: addr.to_string(),
            protocol: proto.to_string(),
            auth_method: "password".to_string(),
            tenant_id: 1,
            credential_version: 0,
        }
    }

    #[test]
    fn active_sessions_register_unregister() {
        let reg = SessionRegistry::new();
        let rx = reg
            .register("s1", &params(42, "10.0.0.1:5000", "native"))
            .unwrap();
        assert!(!rx.has_changed().unwrap_or(false));
        assert_eq!(reg.count(None), 1);
        assert_eq!(reg.count(Some(42)), 1);
        reg.unregister("s1");
        assert_eq!(reg.count(None), 0);
    }

    #[test]
    fn max_active_sessions_over_cap_rejects() {
        let reg = SessionRegistry::with_cap(2);

        reg.register("s1", &params(1, "10.0.0.1:5000", "native"))
            .unwrap();
        reg.register("s2", &params(2, "10.0.0.2:5000", "native"))
            .unwrap();

        // Third registration must fail.
        let result = reg.register("s3", &params(3, "10.0.0.3:5000", "native"));
        assert!(
            result.is_err(),
            "over-cap registration must return SessionCapExceeded"
        );

        // Existing sessions still alive.
        assert_eq!(reg.count(None), 2);
    }

    #[test]
    fn session_hard_revoke_close() {
        let reg = SessionRegistry::new();
        let mut rx = reg
            .register("s1", &params(99, "10.0.0.1:5000", "native"))
            .unwrap();
        assert!(!rx.has_changed().unwrap_or(false));

        let killed = reg.kill_sessions_for_user(99);
        assert_eq!(killed, 1);
        assert!(rx.has_changed().unwrap_or(false));
        assert!(*rx.borrow_and_update());
    }

    #[test]
    fn kill_by_ip() {
        let reg = SessionRegistry::new();
        let _rx1 = reg
            .register("s1", &params(1, "10.0.0.1:5000", "native"))
            .unwrap();
        let _rx2 = reg
            .register("s2", &params(2, "10.0.0.1:5001", "pgwire"))
            .unwrap();
        let _rx3 = reg
            .register("s3", &params(3, "192.168.1.1:5000", "http"))
            .unwrap();

        let killed = reg.kill_sessions_for_ip("10.0.0.1");
        assert_eq!(killed, 2);
    }

    #[test]
    fn show_sessions_lists_active() {
        let reg = SessionRegistry::new();
        reg.register("sess-abc", &params(7, "127.0.0.1:1234", "pgwire"))
            .unwrap();

        let all = reg.list_all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_id, "sess-abc");
        assert_eq!(all[0].user_id, 7);
        assert_eq!(all[0].protocol, "pgwire");
    }
}
