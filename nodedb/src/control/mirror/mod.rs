// SPDX-License-Identifier: BUSL-1.1

//! Mirror subsystem for the Control Plane.
//!
//! - [`registry`] — [`MirrorLinkRegistry`]: tracks active [`CrossClusterLink`]
//!   handles per database; used by `ALTER DATABASE PROMOTE` to tear down the
//!   source link before the catalog mutation lands.
//! - [`observer`] — lag-transition logic and the
//!   `nodedb_database_mirror_lag_ms` metric update path.
//! - [`restart`] — enumerates mirror databases on server start and
//!   returns the set that need their observer link re-established.

pub mod observer;
pub mod registry;
pub mod restart;

pub use observer::{
    LAG_DEGRADED_MS, LAG_DISCONNECTED_MS, LagTransition, compute_lag_transition, record_apply,
    update_lag_status,
};
pub use registry::MirrorLinkRegistry;
pub use restart::{MirrorRestartDecision, enumerate_resumable_mirrors};
