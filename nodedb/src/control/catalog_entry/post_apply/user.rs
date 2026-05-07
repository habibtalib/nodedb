// SPDX-License-Identifier: BUSL-1.1

//! User post-apply side effects — upsert / deactivate the
//! in-memory `credentials` cache. Follower nodes accept the
//! leader's pre-computed `StoredUser` verbatim.

use std::sync::Arc;

use crate::control::security::buses::SessionInvalidationReason;
use crate::control::security::catalog::StoredUser;
use crate::control::state::SharedState;

pub fn put(
    stored: StoredUser,
    shared: Arc<SharedState>,
    invalidation: Option<SessionInvalidationReason>,
) {
    shared
        .credentials
        .install_replicated_user(&stored, invalidation);
}

pub fn deactivate(username: String, shared: Arc<SharedState>) {
    shared.credentials.install_replicated_deactivate(&username);
}
