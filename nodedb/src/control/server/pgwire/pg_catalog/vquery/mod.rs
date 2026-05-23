// SPDX-License-Identifier: BUSL-1.1

//! In-process SQL evaluator for virtual catalog tables.
//!
//! Virtual tables (`_system.*`, `pg_catalog.*`) are Control-Plane synthetic
//! relations whose backing data lives entirely in `SharedState`. They never
//! cross the SPSC bridge, so the full planner / Data Plane path is the wrong
//! model for them. Instead, this module:
//!
//! 1. Materializes the virtual table as a typed [`VTable`] (`value`, `table`).
//! 2. Parses the client SELECT into a [`VSelect`] (`select`).
//! 3. Evaluates WHERE / projection / aggregate / ORDER BY / LIMIT against the
//!    row set (`expr`, `exec`).
//! 4. Encodes the result back to pgwire (`encode`).
//!
//! The previous interceptor returned raw rows with ad-hoc regex pushdown of
//! `SEQ` / `LIMIT`, silently dropping every other SQL clause. This module
//! honors full SQL semantics for the surface that ships through
//! `try_pg_catalog`.

pub mod encode;
pub mod exec;
pub mod expr;
pub mod select;
pub mod table;
pub mod value;

pub use exec::execute;
pub use select::parse_select;
pub use table::VTable;
