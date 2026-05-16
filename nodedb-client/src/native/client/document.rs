// SPDX-License-Identifier: Apache-2.0

//! Document operation implementations for `NativeClient`.

use std::collections::HashMap;

use nodedb_types::document::Document;
use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::protocol::{OpCode, TextFields};

use super::core::NativeClient;
use nodedb_types::conversion::json_to_value;

impl NativeClient {
    pub(super) async fn document_get_impl(
        &self,
        collection: &str,
        id: &str,
    ) -> NodeDbResult<Option<Document>> {
        let mut conn = self.pool.acquire().await?;
        let resp = conn
            .send(
                OpCode::PointGet,
                TextFields {
                    collection: Some(collection.to_string()),
                    document_id: Some(id.to_string()),
                    ..Default::default()
                },
            )
            .await?;

        let rows = resp.rows.unwrap_or_default();
        if rows.is_empty() {
            return Ok(None);
        }

        let json_text = rows[0].first().and_then(|v| v.as_str()).unwrap_or("{}");
        let obj: HashMap<String, serde_json::Value> =
            sonic_rs::from_str(json_text).map_err(|e| {
                NodeDbError::serialization(
                    "json",
                    format!("document_get response for id '{id}': {e}"),
                )
            })?;
        let mut doc = Document::new(id);
        for (k, v) in obj {
            doc.set(&k, json_to_value(v));
        }
        Ok(Some(doc))
    }

    pub(super) async fn document_put_impl(
        &self,
        collection: &str,
        doc: Document,
    ) -> NodeDbResult<()> {
        let data = sonic_rs::to_vec(&doc.fields)
            .map_err(|e| NodeDbError::serialization("json", format!("doc serialize: {e}")))?;
        let mut conn = self.pool.acquire().await?;
        conn.send(
            OpCode::PointPut,
            TextFields {
                collection: Some(collection.to_string()),
                document_id: Some(doc.id.clone()),
                data: Some(data),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    pub(super) async fn document_delete_impl(
        &self,
        collection: &str,
        id: &str,
    ) -> NodeDbResult<()> {
        let mut conn = self.pool.acquire().await?;
        conn.send(
            OpCode::PointDelete,
            TextFields {
                collection: Some(collection.to_string()),
                document_id: Some(id.to_string()),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }
}
