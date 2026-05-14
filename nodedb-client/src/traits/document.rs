// SPDX-License-Identifier: Apache-2.0

//! Supporting types for document and collection-lifecycle operations.

use std::sync::Arc;

/// Event passed to `NodeDb::on_collection_purged` handlers.
///
/// Emitted on the sync client when Origin pushes a `CollectionPurged`
/// wire message and on Lite after local hard-delete completes, so
/// application code can flush UI caches, drop derived indexes, etc.
/// Handler callsites must not block — the dispatch path is on the
/// sync client's receive loop.
#[derive(Debug, Clone)]
pub struct CollectionPurgedEvent {
    pub tenant_id: u64,
    pub name: String,
    /// WAL LSN at which the purge was applied. Handlers can compare
    /// this against locally-observed LSNs for resume/replay logic.
    pub purge_lsn: u64,
}

/// Handler registered via `NodeDb::on_collection_purged`. Fn-ref
/// (not FnMut) so the same handler can fire from multiple threads
/// without interior mutability ceremony at every call site.
pub type CollectionPurgedHandler = Arc<dyn Fn(CollectionPurgedEvent) + Send + Sync + 'static>;
