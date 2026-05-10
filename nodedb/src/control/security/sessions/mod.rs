// SPDX-License-Identifier: BUSL-1.1

//! Active session tracking: registry, cap enforcement, revocation, idle sweep.
//!
//! ## Lifecycle
//!
//! `SessionRegistry` is constructed once during server startup and shared
//! via `SharedState::session_registry`. Every authenticated connection
//! (native, pgwire, HTTP) calls `register` on bind and `unregister` on drop.
//!
//! `spawn_idle_sweep_loop` MUST be called exactly once at server startup,
//! after `SharedState` is fully built. It registers a Tokio task with the
//! shared `loop_registry` (so it drains on shutdown) and ticks every five
//! seconds, signalling `KillReason::IdleTimeout` or `KillReason::TokenExpired`
//! to sessions whose per-database idle cap or OIDC token expiry has elapsed.
//! Calling it more than once would spawn duplicate sweepers — there is no
//! internal idempotency guard, so the call site (currently
//! `nodedb::bootstrap::background_loops`) is responsible for the
//! exactly-once invariant.
//!
//! Callers never invoke `unregister` from the sweep path; the sweep loop
//! only sends on `kill_tx`, and the session's own drop path removes the
//! row.

pub mod idle_sweep;
pub mod registry;

pub use idle_sweep::spawn_idle_sweep_loop;
pub use registry::{
    KillReason, SessionCapExceeded, SessionInfo, SessionParams, SessionRegistry, SweepEntry,
};
