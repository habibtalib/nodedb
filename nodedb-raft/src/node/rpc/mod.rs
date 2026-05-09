// SPDX-License-Identifier: BUSL-1.1

//! RPC handlers for incoming Raft messages.
//!
//! Split by RPC family — each submodule adds its own `impl<S: LogStorage>
//! RaftNode<S>` block:
//! - [`append_entries`]: `AppendEntries` request + response handlers.
//! - [`request_vote`]:   `RequestVote`   request + response handlers.
//! - [`install_snapshot`]: `InstallSnapshot` request handler.

pub mod append_entries;
pub mod install_snapshot;
pub mod request_vote;

#[cfg(test)]
mod test_helpers;
