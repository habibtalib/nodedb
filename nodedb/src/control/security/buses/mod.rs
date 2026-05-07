// SPDX-License-Identifier: BUSL-1.1

//! In-process broadcast buses for security-relevant events (Control Plane only).

pub mod consumer;
pub mod session_invalidation;
pub mod user_change;

pub use consumer::spawn_bus_consumer;
pub use session_invalidation::{
    SessionInvalidated, SessionInvalidationBus, SessionInvalidationReason,
};
pub use user_change::{UserChangeBus, UserChanged};
