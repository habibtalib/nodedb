// SPDX-License-Identifier: BUSL-1.1

pub mod permit;
pub mod registry;

pub use permit::ConnectionPermit;
pub use registry::{AdmissionError, AdmissionRegistry};
