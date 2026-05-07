// SPDX-License-Identifier: BUSL-1.1

//! Active session tracking (registry, cap enforcement, revocation).

pub mod registry;

pub use registry::{SessionCapExceeded, SessionInfo, SessionParams, SessionRegistry};
