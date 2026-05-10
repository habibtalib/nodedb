// SPDX-License-Identifier: BUSL-1.1

//! Named checkpoint records persisted in the system catalog.

/// A named checkpoint: captures a version vector at a point in time.
#[derive(zerompk::ToMessagePack, zerompk::FromMessagePack, Debug, Clone)]
pub struct CheckpointRecord {
    pub tenant_id: u64,
    pub collection: String,
    pub doc_id: String,
    pub checkpoint_name: String,
    pub version_vector_json: String,
    pub created_by: String,
    pub created_at: u64,
}

impl CheckpointRecord {
    pub fn catalog_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.tenant_id, self.collection, self.doc_id, self.checkpoint_name
        )
    }

    pub fn doc_prefix(tenant_id: u64, collection: &str, doc_id: &str) -> String {
        format!("{tenant_id}:{collection}:{doc_id}:")
    }
}
