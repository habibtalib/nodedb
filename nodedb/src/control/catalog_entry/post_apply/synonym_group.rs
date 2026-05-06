//! Synonym group post-apply side effects — sync the in-memory registry.

use std::sync::Arc;

use crate::control::security::catalog::StoredSynonymGroup;
use crate::control::state::SharedState;

pub fn put(stored: StoredSynonymGroup, shared: Arc<SharedState>) {
    shared.synonym_registry.register(stored);
}

pub fn delete(tenant_id: u64, name: String, shared: Arc<SharedState>) {
    shared.synonym_registry.unregister(tenant_id, &name);
}
