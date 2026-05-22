// SPDX-License-Identifier: BUSL-1.1

//! Credential store — one concern per file.
//!
//! `CredentialStore` lives in `core.rs` alongside its constructors
//! and private helpers. Each sibling file adds an `impl
//! CredentialStore` block for a single concern:
//!
//! - [`auth`] — password/SCRAM/MD5 verification + identity builder
//! - [`crud`] — create/deactivate/update user + role mutations
//! - [`list`] — list / is_empty / catalog accessors
//! - [`replication`] — cluster-replication hooks (`prepare_user`,
//!   `install_replicated_*`, `prepare_user_update`)
//!
//! Struct fields are `pub(super)` so every sibling can reach them
//! without exposing internals beyond the `credential` module.

pub mod auth;
pub mod core;
pub mod crud;
pub mod list;
pub mod replication;

#[cfg(test)]
mod tests;

pub use auth::{AuthRejection, PasswordVerification, ScramCredentials, ScramLookup};
pub use core::CredentialStore;
pub(super) use core::{read_lock, write_lock};
