// SPDX-License-Identifier: Apache-2.0

//! `NativeClient` struct definition and connection/session helpers.

use nodedb_types::error::{ErrorDetails, NodeDbError, NodeDbResult};
use nodedb_types::result::QueryResult;

use super::super::pool::{Pool, PoolConfig};

/// Native protocol client for NodeDB.
///
/// Connects via the binary MessagePack protocol. Supports all operations:
/// SQL, DDL, direct Data Plane ops, transactions, session parameters.
pub struct NativeClient {
    pub(super) pool: Pool,
}

impl NativeClient {
    /// Create a client with the given pool configuration.
    pub fn new(config: PoolConfig) -> Self {
        Self {
            pool: Pool::new(config),
        }
    }

    /// Connect to a NodeDB server with default settings.
    pub fn connect(addr: &str) -> Self {
        Self::new(PoolConfig {
            addr: addr.to_string(),
            ..Default::default()
        })
    }

    /// Execute a SQL query and return structured results.
    ///
    /// Retries once with a fresh connection on I/O failure.
    pub async fn query(&self, sql: &str) -> NodeDbResult<QueryResult> {
        let mut conn = self.pool.acquire().await?;
        match conn.execute_sql(sql).await {
            Ok(r) => Ok(r),
            Err(e) if is_connection_error(&e) => {
                drop(conn);
                let mut conn = self.pool.acquire().await?;
                conn.execute_sql(sql).await
            }
            Err(e) => Err(e),
        }
    }

    /// Execute a DDL command.
    pub async fn ddl(&self, sql: &str) -> NodeDbResult<QueryResult> {
        let mut conn = self.pool.acquire().await?;
        match conn.execute_ddl(sql).await {
            Ok(r) => Ok(r),
            Err(e) if is_connection_error(&e) => {
                drop(conn);
                let mut conn = self.pool.acquire().await?;
                conn.execute_ddl(sql).await
            }
            Err(e) => Err(e),
        }
    }

    /// Begin a transaction.
    pub async fn begin(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.begin().await
    }

    /// Commit the current transaction.
    pub async fn commit(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.commit().await
    }

    /// Rollback the current transaction.
    pub async fn rollback(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.rollback().await
    }

    /// Set a session parameter.
    pub async fn set_parameter(&self, key: &str, value: &str) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.set_parameter(key, value).await
    }

    /// Show a session parameter.
    pub async fn show_parameter(&self, key: &str) -> NodeDbResult<String> {
        let mut conn = self.pool.acquire().await?;
        conn.show_parameter(key).await
    }

    /// Ping the server.
    pub async fn ping(&self) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.ping().await
    }
}

/// Check if an error is a connection-level failure (worth retrying).
pub(super) fn is_connection_error(e: &NodeDbError) -> bool {
    matches!(
        e.details(),
        ErrorDetails::SyncConnectionFailed | ErrorDetails::Storage { .. }
    )
}

/// Quote a SQL identifier (collection / column name) by doubling any
/// internal double-quotes and wrapping the result in double-quotes —
/// the SQL standard rule that PostgreSQL applies under
/// `standard_conforming_strings=on`. Mirrors the always-quote variant
/// in `remote_parse::quote_identifier`; kept here to avoid pulling the
/// `remote` feature into the `native` client.
pub(super) fn sql_quote_identifier(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Render a `&str` as a SQL string literal: single-quote-doubled and
/// wrapped in single quotes. Matches `standard_conforming_strings=on`
/// behavior (PG 9.1+ default) which is the only mode the server runs in.
/// Centralizes the escape so call sites can't drift into raw `format!`s
/// without going through it.
pub(super) fn sql_quote_string_literal(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_quote_identifier_wraps_and_escapes_double_quotes() {
        assert_eq!(sql_quote_identifier("foo"), "\"foo\"");
        // Embedded `"` must be doubled per the SQL identifier-escape rule.
        assert_eq!(sql_quote_identifier("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn sql_quote_string_literal_escapes_single_quotes() {
        assert_eq!(sql_quote_string_literal("plain"), "'plain'");
        // The PG standard rule under `standard_conforming_strings=on`:
        // double every embedded `'`. A `O'Reilly` literal that lost its
        // escape would close the SQL string early and inject the rest.
        assert_eq!(sql_quote_string_literal("O'Reilly"), "'O''Reilly'");
        assert_eq!(
            sql_quote_string_literal("'; DROP TABLE x; --"),
            "'''; DROP TABLE x; --'"
        );
    }

    #[test]
    fn sql_quote_string_literal_passes_through_json() {
        // The metadata path renders sonic_rs JSON and then quotes it as
        // a SQL string. JSON already escapes its own `"` and `\`, so
        // only the outer `'` needs SQL escaping.
        let json = r#"{"name":"O'Reilly","ok":true}"#;
        let quoted = sql_quote_string_literal(json);
        assert_eq!(quoted, "'{\"name\":\"O''Reilly\",\"ok\":true}'");
    }
}
