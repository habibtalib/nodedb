// SPDX-License-Identifier: Apache-2.0

//! SQL execution and collection lifecycle implementations for `NativeClient`.

use nodedb_types::error::NodeDbResult;
use nodedb_types::result::QueryResult;
use nodedb_types::value::Value;

use super::core::{NativeClient, is_connection_error};

impl NativeClient {
    pub(super) async fn execute_sql_impl(
        &self,
        query: &str,
        params: &[Value],
    ) -> NodeDbResult<QueryResult> {
        // Bound parameters travel through `TextFields::sql_params` as a
        // zerompk-MessagePack `Vec<Value>`. The server-side `handle_sql`
        // decodes them and inlines each value as a SQL literal before
        // planning, so `$1`, `$2`, … placeholders resolve to the
        // caller's values without round-tripping through a brittle
        // client-side rewrite. Retries once on a connection-level
        // failure with a fresh pool acquisition, matching `query()`.
        let mut conn = self.pool.acquire().await?;
        match conn.execute_sql_with_params(query, params).await {
            Ok(r) => Ok(r),
            Err(e) if is_connection_error(&e) => {
                drop(conn);
                let mut conn = self.pool.acquire().await?;
                conn.execute_sql_with_params(query, params).await
            }
            Err(e) => Err(e),
        }
    }
}
