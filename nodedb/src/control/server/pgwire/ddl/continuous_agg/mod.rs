// SPDX-License-Identifier: BUSL-1.1

//! DDL handlers for continuous aggregates.
//!
//! - `CREATE CONTINUOUS AGGREGATE <name> ON <source> BUCKET '5m'
//!   AGGREGATE sum(col) AS alias [, ...] [GROUP BY col, ...] [WITH (...)]`
//! - `DROP CONTINUOUS AGGREGATE <name>`
//! - `SHOW CONTINUOUS AGGREGATES [FOR <source>]`
//!
//! The catalog row (`CatalogEntry::PutContinuousAggregate`) is the
//! source of truth: it carries the parent-replicated definition + owner
//! row that the integrity verifier and startup replay rely on. Every
//! node's `post_apply` async-dispatch path re-issues
//! `MetaOp::RegisterContinuousAggregate` from the persisted bytes, so
//! the runtime manager observes the registration symmetrically on
//! leaders and followers. The create handler additionally creates a
//! schemaless target collection of the aggregate's name so that
//! `SELECT … FROM <ca_name>` resolves like any other collection.
//!
//! Module split:
//! - `parse` — `CREATE CONTINUOUS AGGREGATE` SQL parsing helpers.
//! - `create` — `CREATE` handler + `CreateContinuousAggregateRequest`.
//! - `drop` — `DROP CONTINUOUS AGGREGATE` handler.
//! - `show` — `SHOW CONTINUOUS AGGREGATES [FOR <source>]` handler.
//! - `register` — startup replay of catalog-persisted aggregates.

pub mod create;
pub mod drop;
pub mod parse;
pub mod register;
pub mod show;

pub use create::{CreateContinuousAggregateRequest, create_continuous_aggregate};
pub use drop::drop_continuous_aggregate;
pub use register::register_persisted_continuous_aggregates;
pub use show::show_continuous_aggregates;
