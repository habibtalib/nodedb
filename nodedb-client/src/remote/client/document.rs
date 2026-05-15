// SPDX-License-Identifier: Apache-2.0

//! Document operation implementations for `NodeDbRemote`.

use std::collections::HashMap;

use nodedb_types::document::Document;
use nodedb_types::error::{NodeDbError, NodeDbResult};
use nodedb_types::value::Value;

use crate::remote_parse::json_to_value;
use crate::sql_escape::quote_identifier;

use super::core::NodeDbRemote;

impl NodeDbRemote {
    pub(super) async fn document_get_impl(
        &self,
        collection: &str,
        id: &str,
    ) -> NodeDbResult<Option<Document>> {
        let collection = quote_identifier(collection);
        let sql = format!("SELECT id, data FROM {collection} WHERE id = $1");
        let (_, rows) = self.query_raw(&sql, &[&id]).await?;

        if let Some(row) = rows.first() {
            let doc_id = row
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();

            let mut doc = Document::new(doc_id);

            // If the second column is JSON, parse it into fields.
            if let Some(Value::Object(fields)) = row.get(1) {
                for (k, v) in fields {
                    doc.set(k.clone(), v.clone());
                }
            } else if let Some(Value::String(json_str)) = row.get(1)
                && let Ok(parsed) =
                    sonic_rs::from_str::<HashMap<String, serde_json::Value>>(json_str)
            {
                for (k, v) in &parsed {
                    doc.set(k.clone(), json_to_value(v));
                }
            }

            Ok(Some(doc))
        } else {
            Ok(None)
        }
    }

    pub(super) async fn document_put_impl(
        &self,
        collection: &str,
        doc: Document,
    ) -> NodeDbResult<()> {
        let collection = quote_identifier(collection);
        let data_json = sonic_rs::to_string(&doc.fields)
            .map_err(|e| NodeDbError::storage(format!("document serialization: {e}")))?;
        // NodeDB's SQL planner accepts JSON text values directly into
        // the document `data` column — no `::jsonb` cast on the
        // expression side, which the planner currently rejects as an
        // "unsupported value expression". The server interprets the
        // string literal as document JSON when the target column is the
        // doc-engine `data` column.
        let sql = format!(
            "INSERT INTO {collection} (id, data) VALUES ($1, $2) \
             ON CONFLICT (id) DO UPDATE SET data = $2"
        );
        self.execute_raw(&sql, &[&doc.id, &data_json]).await?;
        Ok(())
    }

    pub(super) async fn document_delete_impl(
        &self,
        collection: &str,
        id: &str,
    ) -> NodeDbResult<()> {
        let collection = quote_identifier(collection);
        let sql = format!("DELETE FROM {collection} WHERE id = $1");
        self.execute_raw(&sql, &[&id]).await?;
        Ok(())
    }
}
