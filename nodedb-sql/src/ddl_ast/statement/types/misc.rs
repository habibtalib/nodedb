// SPDX-License-Identifier: Apache-2.0

//! Miscellaneous DDL/DML statements.

use crate::ddl_ast::statement::maintenance::{CopyFormat, CopyToSource};

#[derive(Debug, Clone, PartialEq)]
pub enum MiscStmt {
    // ── Miscellaneous ────────────────────────────────────────────
    ShowAuditLog,
    ShowConstraints {
        collection: String,
    },
    ShowTypeGuards {
        collection: String,
    },

    // ── Bulk import ──────────────────────────────────────────────
    /// `COPY <collection> FROM '<path>' [WITH (FORMAT ..., DELIMITER ..., HEADER ...)]`
    ///
    /// Server-side file-path bulk import. Does not handle STDIN streaming
    /// (that is a different protocol path) or COPY ... TO.
    CopyFromFile {
        collection: String,
        path: String,
        format: Option<CopyFormat>,
        delimiter: Option<char>,
        header: bool,
    },

    // ── Bulk export ──────────────────────────────────────────────
    /// `COPY <collection> TO '<path>' [WITH (FORMAT ..., DELIMITER ..., HEADER ...)]`
    /// `COPY (SELECT ...) TO '<path>' [WITH (...)]`
    ///
    /// Server-side file-path bulk export. Streams scan results to a file.
    CopyToFile {
        /// The source: either a bare collection name or a SELECT query.
        source: CopyToSource,
        path: String,
        format: Option<CopyFormat>,
        delimiter: Option<char>,
        header: bool,
    },
}
