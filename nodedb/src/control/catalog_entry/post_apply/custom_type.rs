//! Custom type post-apply side effects — sync the in-memory registry.

use std::sync::Arc;

use crate::control::security::catalog::StoredCustomType;
use crate::control::state::SharedState;

pub fn put(stored: StoredCustomType, shared: Arc<SharedState>) {
    shared.custom_type_registry.register(stored);
}

pub fn delete(tenant_id: u64, name: String, shared: Arc<SharedState>) {
    shared.custom_type_registry.unregister(tenant_id, &name);
}
