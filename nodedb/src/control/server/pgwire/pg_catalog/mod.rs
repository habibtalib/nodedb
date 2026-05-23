// SPDX-License-Identifier: BUSL-1.1

//! Minimal `pg_catalog` virtual-table emulation.
//!
//! Generic Postgres clients (DBeaver, pgAdmin, SQLAlchemy, psql's
//! `\dt`) issue `SELECT` queries against `pg_catalog.*` tables to
//! discover schemas, types, and tables. Without a response they
//! either error out or show an empty catalog. This module intercepts
//! those queries and returns rows synthesised from NodeDB's own
//! `SystemCatalog` and credential store.
//!
//! The interception is pattern-based: we extract the first
//! `pg_catalog.<table>` (or bare `pg_<table>`) reference from the
//! `FROM` clause and materialize the matching virtual table. The client
//! SELECT is then parsed and evaluated against the materialized rows by
//! [`vquery`] — WHERE, aggregates, projection, ORDER BY, and LIMIT all
//! observe normal SQL semantics. Virtual tables never cross the SPSC
//! bridge: they're Control-Plane synthetic relations whose data lives
//! entirely in `SharedState`.

pub mod audit_log;
pub mod dispatch;
pub mod dropped_collections;
pub mod l2_cleanup_queue;
pub mod oid;
pub mod tables;
pub mod vquery;

pub use dispatch::{
    extract_pg_catalog_table, pg_catalog_projected_schema, pg_catalog_schema, try_pg_catalog,
    try_pg_catalog_with_params,
};
pub use oid::{stable_collection_oid, stable_index_oid};
