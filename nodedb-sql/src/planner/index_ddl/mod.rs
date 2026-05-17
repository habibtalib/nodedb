// SPDX-License-Identifier: Apache-2.0

//! Planners for `CREATE INDEX` / `DROP INDEX` parsed via sqlparser.

pub mod create;
pub mod drop;

pub use create::plan_create_index;
pub use drop::plan_drop_index;
