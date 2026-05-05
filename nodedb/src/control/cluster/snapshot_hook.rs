//! `RaftSnapshotQuarantineHook` — bridges cluster snapshot events to the
//! in-process `QuarantineRegistry`.

use std::sync::Arc;

/// `nodedb`-side implementation of [`nodedb_cluster::SnapshotQuarantineHook`].
///
/// All snapshot chunks are keyed under collection `"_raft_snapshot"` with
/// segment id `"group=<g>:index=<i>"`.
pub(super) struct RaftSnapshotQuarantineHook {
    pub(super) registry: Arc<crate::storage::quarantine::QuarantineRegistry>,
}

impl nodedb_cluster::SnapshotQuarantineHook for RaftSnapshotQuarantineHook {
    fn is_quarantined(&self, group_id: u64, last_included_index: u64) -> bool {
        let key = crate::storage::quarantine::SegmentKey {
            engine: crate::storage::quarantine::QuarantineEngine::Raft,
            collection: "_raft_snapshot".to_string(),
            segment_id: format!("group={group_id}:index={last_included_index}"),
        };
        self.registry.is_quarantined(&key)
    }

    fn record_success(&self, group_id: u64, last_included_index: u64) {
        let key = crate::storage::quarantine::SegmentKey {
            engine: crate::storage::quarantine::QuarantineEngine::Raft,
            collection: "_raft_snapshot".to_string(),
            segment_id: format!("group={group_id}:index={last_included_index}"),
        };
        self.registry.record_success(&key);
    }

    fn record_failure(&self, group_id: u64, last_included_index: u64, error: &str) -> bool {
        let key = crate::storage::quarantine::SegmentKey {
            engine: crate::storage::quarantine::QuarantineEngine::Raft,
            collection: "_raft_snapshot".to_string(),
            segment_id: format!("group={group_id}:index={last_included_index}"),
        };
        // record_failure returns Err(SegmentQuarantined) on the second strike.
        self.registry.record_failure(key, error, None).is_err()
    }
}
