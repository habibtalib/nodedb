// SPDX-License-Identifier: BUSL-1.1

//! `GRANT` / `REVOKE` DDL handlers.
//!
//! Statements are parsed into typed `AuthStmt` variants by the `grant`
//! parser family in `nodedb-sql` and dispatched here by the AST router:
//!
//! - `GRANT/REVOKE <role> TO/FROM <grantee>` — see [`role`]. A user
//!   grantee has the roles added to its set (reusing
//!   `CatalogEntry::PutUser`); a role grantee has its inheritance parent
//!   updated (role-to-role membership).
//! - `GRANT/REVOKE <perm> ON <collection|FUNCTION|PROCEDURE name>
//!   TO/FROM <grantee>` — see [`permission`]. Proposes
//!   `CatalogEntry::{PutPermission, DeletePermission}` so every
//!   follower's `PermissionStore` and `OWNERS` redb stay in sync.
//! - `GRANT/REVOKE <priv> ON DATABASE <name> TO/FROM <user>` — see
//!   [`database_permission`].

pub mod database_permission;
pub mod permission;
pub mod role;
