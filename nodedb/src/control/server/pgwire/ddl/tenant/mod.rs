// SPDX-License-Identifier: BUSL-1.1

//! Tenant DDL handlers.
//!
//! - [`create`] — `CREATE TENANT` (proposes `CatalogEntry::PutTenant`).
//! - [`alter`] — `ALTER TENANT SET QUOTA` (in-memory; quota is not
//!   part of `StoredTenant` — quota replication is a separate concern).
//! - [`alter_quota`] — `ALTER TENANT <name> IN DATABASE <db> SET QUOTA (...)` —
//!   persists quota to `_system.tenant_quotas`.
//! - [`drop`] — `DROP TENANT` (proposes `DeleteTenant`).
//! - [`purge`] — `PURGE TENANT <id> CONFIRM` (Data Plane meta op).
//! - [`show`] — `SHOW TENANT USAGE` / `SHOW TENANT QUOTA` reads.
//! - [`show_in_database`] — `SHOW TENANT QUOTA/USAGE FOR <name> IN DATABASE <db>`.

pub mod alter;
pub mod alter_quota;
pub mod create;
pub mod drop;
pub mod move_tenant;
pub mod purge;
pub mod show;
pub mod show_in_database;

pub use alter::alter_tenant;
pub use alter_quota::handle_alter_tenant_quota;
pub use create::create_tenant;
pub use drop::drop_tenant;
pub use move_tenant::handle_move_tenant;
pub use purge::purge_tenant;
pub use show::{show_tenant_quota, show_tenant_usage};
pub use show_in_database::{
    handle_show_tenant_quota_in_database, handle_show_tenant_usage_in_database,
};
