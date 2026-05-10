// SPDX-License-Identifier: Apache-2.0

//! Recursive-descent parser for NodeDB database DDL/admin statements.
//!
//! Handles `CREATE DATABASE`, `DROP DATABASE`, `ALTER DATABASE`,
//! `SHOW DATABASES`, `USE DATABASE`, and the stub forms
//! `CLONE DATABASE`, `MIRROR DATABASE`, `MOVE TENANT`,
//! `BACKUP DATABASE`, `RESTORE DATABASE`.
//!
//! `try_parse_database_statement` returns `None` for any other input so
//! the caller can fall through to the standard sqlparser path.

pub mod parse;

pub use parse::try_parse_database_statement;
