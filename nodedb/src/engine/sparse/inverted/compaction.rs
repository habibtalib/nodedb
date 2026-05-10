// SPDX-License-Identifier: BUSL-1.1

//! LSM compaction support on the Origin inverted index.
//!
//! Exposes the atomic `compact_commit` (write merged + remove sources in one
//! redb transaction) and a backend-driven enumeration of FTS-bearing
//! `(tenant, collection)` pairs used by the maintenance cycle.

use nodedb_types::TenantId;

use super::core::InvertedIndex;

impl InvertedIndex {
    /// Atomically write a new merged FTS segment and remove the source segments
    /// that were merged into it, in one redb write transaction.
    ///
    /// Crash-safe: if the process terminates before the commit, the original
    /// segments remain intact for the next maintenance pass. The single
    /// transaction also guarantees that no reader observes both the new and
    /// the old segments simultaneously.
    pub fn compact_commit(
        &self,
        tid: TenantId,
        collection: &str,
        new_segment_id: &str,
        new_segment_data: &[u8],
        merged_ids: &[String],
    ) -> crate::Result<()> {
        self.inner.backend().compact_commit(
            tid.as_u64(),
            collection,
            new_segment_id,
            new_segment_data,
            merged_ids,
        )
    }

    /// Enumerate every `(TenantId, collection)` pair that has at least one
    /// FTS segment in the backing store.
    ///
    /// Used by the maintenance cycle to discover compaction candidates
    /// without requiring a separate in-memory registry of FTS-indexed
    /// collections.
    pub fn list_all_fts_collections(&self) -> crate::Result<Vec<(TenantId, String)>> {
        self.inner
            .backend()
            .list_all_fts_collections()
            .map(|pairs| {
                pairs
                    .into_iter()
                    .map(|(t, c)| (TenantId::new(t), c))
                    .collect()
            })
    }
}
