//! Per-group partial-snapshot accumulation state.

use std::fs::File;

/// In-memory state for an in-progress `InstallSnapshot` receive.
///
/// One entry lives in [`crate::raft_loop::loop_core::RaftLoop::partial_snapshots`]
/// per group that is currently receiving a snapshot. Dropped on finalization or
/// on offset regression (partial reset with leader retransmit from offset 0).
pub struct PartialSnapshotState {
    /// Raft group ID that owns this partial.
    pub group_id: u64,
    /// The leader sending us this snapshot.
    pub leader_id: u64,
    /// Leader term for the snapshot.
    pub term: u64,
    /// The `last_included_index` carried by the snapshot.
    pub last_included_index: u64,
    /// The `last_included_term` carried by the snapshot.
    pub last_included_term: u64,
    /// Next byte offset we expect. Validated against `req.offset` on each
    /// incoming chunk; a mismatch triggers a `SnapshotOffsetRegression` error.
    pub next_expected_offset: u64,
    /// Running CRC32C across all written chunk bytes (not the framing header —
    /// only the raw chunk payload bytes as they arrive). Validated against the
    /// expected CRC at finalization.
    pub running_crc: u32,
    /// Set to `true` after the first non-empty chunk is written. Used to select
    /// between `crc32c::crc32c` (first block) and `crc32c::crc32c_append`
    /// (subsequent blocks) so the running CRC is computed correctly.
    pub running_crc_initialized: bool,
    /// The `.partial` file open for append. Created (or truncated) when we see
    /// `offset == 0`. Wrapped in `Option` to allow taking ownership during
    /// finalization.
    pub partial_file: Option<File>,
    /// Path to the `.partial` file on disk.
    pub partial_path: std::path::PathBuf,
}
