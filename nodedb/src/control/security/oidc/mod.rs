// SPDX-License-Identifier: BUSL-1.1

//! OIDC bearer-token login flow.
//!
//! This module handles validation of OIDC bearer tokens for native and HTTP
//! clients. pgwire does NOT support OIDC (SCRAM-SHA-256 only; use native
//! protocol or HTTP for OIDC).

pub mod claim_mapping;
pub mod verify;

pub use claim_mapping::{ClaimMappingResult, apply_claim_mapping};
pub use verify::verify_bearer_token;
