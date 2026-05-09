// SPDX-License-Identifier: BUSL-1.1

//! Wire envelope and result types for cross-cluster snapshot transfer.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use nodedb_types::{Lsn, MirrorStatus};

/// Progress callback invoked by [`super::MirrorBootstrapReceiver`] every
/// `PROGRESS_REPORT_CHUNK_BYTES` to update `MirrorStatus::Bootstrapping`.
pub type ProgressCallback = Arc<dyn Fn(MirrorStatus) + Send + Sync + 'static>;

/// Granularity at which the receiver reports progress: ~1 MiB.
pub const PROGRESS_REPORT_CHUNK_BYTES: u64 = 1024 * 1024;

/// Wire envelope that wraps a cross-cluster snapshot chunk.
///
/// Encoded with zerompk (MessagePack) and placed in the `data` field of the
/// existing [`nodedb_raft::InstallSnapshotRequest`].  The in-cluster Raft
/// machinery transfers the bytes unchanged; the mirror receiver unwraps this
/// envelope.
///
/// # Integrity
///
/// `total_crc32c` is the CRC32C of the *entire* snapshot payload (concatenation
/// of every chunk's `data` in offset order).  The source computes it once over
/// the snapshot file and stamps the same value into every envelope of the
/// transfer.  The receiver maintains a running CRC32C as chunks arrive and
/// validates it against `total_crc32c` when `done = true`; mismatch raises
/// [`super::super::error::MirrorError::SnapshotCrcMismatch`] and the partial
/// file is discarded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossClusterSnapshotEnvelope {
    /// Cluster-id of the source cluster that produced this snapshot.
    pub source_cluster_id: String,
    /// Database id on the source cluster being mirrored.
    pub source_database_id: String,
    /// WAL LSN at which this snapshot was taken.  After the snapshot is
    /// fully applied the mirror sets `last_applied` to this value and
    /// begins streaming AppendEntries from `snapshot_lsn + 1`.
    pub snapshot_lsn: u64,
    /// Total snapshot size in bytes (same value in every chunk of the same
    /// snapshot transfer).
    pub total_bytes: u64,
    /// CRC32C over the entire snapshot payload (same value in every chunk of
    /// the same snapshot transfer).  Validated by the receiver on the final
    /// chunk.
    pub total_crc32c: u32,
    /// Byte offset within the snapshot for this chunk.
    pub offset: u64,
    /// Chunk payload bytes.
    pub data: Vec<u8>,
    /// True on the final chunk.
    pub done: bool,
}

/// Outcome of processing a single incoming snapshot chunk.
#[derive(Debug)]
pub enum BootstrapChunkOutcome {
    /// More chunks expected; `bytes_done` is the total received so far.
    Pending { bytes_done: u64 },
    /// All chunks received, CRC validated, file committed.
    /// The mirror should now set `status = MirrorStatus::Following` and
    /// begin streaming AppendEntries from `snapshot_lsn + 1`.
    Committed {
        snapshot_lsn: Lsn,
        snapshot_path: PathBuf,
    },
}
