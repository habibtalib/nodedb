//! Final snapshot commit: CRC validation → atomic rename → Raft log boundary advance.
//!
//! Called only when the last chunk (`done == true`) has been written to the
//! `.partial` file. Performs three operations in sequence:
//!
//! 1. **CRC validation** — re-reads the assembled file and recomputes the
//!    CRC32C. If it disagrees with the running CRC accumulated during chunk
//!    writes, the partial file is left in place and `SnapshotCrcMismatch` is
//!    returned. The partial file is intentionally *not* deleted on CRC failure
//!    so the operator can inspect it.
//!
//! 2. **Atomic rename** — the `.partial` file is renamed to `<group_id>.snap`.
//!    The rename is atomic on POSIX filesystems (same directory, same inode
//!    table). If the process crashes between steps 1 and 2, the partial file
//!    survives; the GC sweeper will remove it after `orphan_partial_max_age_secs`.
//!
//! 3. **Raft log boundary advance** — calls
//!    `MultiRaft::handle_install_snapshot` to advance the Raft log pointer to
//!    `last_included_index` / `last_included_term`. This is the same call the
//!    existing stub in `handle_rpc.rs` made; we now call it only here, after
//!    CRC validation, to prevent advancing Raft state on corrupt data.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nodedb_raft::{InstallSnapshotRequest, InstallSnapshotResponse};

use crate::error::ClusterError;
use crate::install_snapshot::state::PartialSnapshotState;
use crate::multi_raft::MultiRaft;

/// Validate, rename, and advance Raft state after the last chunk.
///
/// Returns the `InstallSnapshotResponse` produced by
/// `MultiRaft::handle_install_snapshot` so callers can propagate the
/// Raft term back to the leader.
pub async fn commit(
    state: PartialSnapshotState,
    multi_raft: &Arc<Mutex<MultiRaft>>,
) -> Result<InstallSnapshotResponse, ClusterError> {
    let group_id = state.group_id;
    let partial_path = state.partial_path.clone();
    let expected_crc = state.running_crc;

    // Flush and close the partial file before reading it back.
    // `state.partial_file` may be `None` if the snapshot had zero bytes
    // (bootstrap stub). In that case skip the I/O validation.
    if let Some(file) = state.partial_file {
        tokio::task::spawn_blocking(move || -> std::io::Result<()> { file.sync_all() })
            .await
            .map_err(|e| ClusterError::PartialSnapshotCorrupt {
                group_id,
                detail: format!("spawn_blocking join error on sync: {e}"),
            })?
            .map_err(|e| ClusterError::Storage {
                detail: format!("sync partial file for group {group_id}: {e}"),
            })?;
    }

    // CRC validation: re-read the file and compare against running CRC.
    // If the file is empty (bootstrap stub), skip.
    let file_bytes = tokio::task::spawn_blocking({
        let path = partial_path.clone();
        move || std::fs::read(&path)
    })
    .await
    .map_err(|e| ClusterError::PartialSnapshotCorrupt {
        group_id,
        detail: format!("spawn_blocking join error on read: {e}"),
    })?
    .map_err(|e| ClusterError::Storage {
        detail: format!("read partial file for group {group_id}: {e}"),
    })?;

    if !file_bytes.is_empty() {
        let computed = crc32c::crc32c(&file_bytes);
        if computed != expected_crc {
            return Err(ClusterError::SnapshotCrcMismatch {
                group_id,
                stored: expected_crc,
                computed,
            });
        }
    }

    // Atomic rename: .partial → .snap
    let snap_path = snap_path_for(&partial_path);
    tokio::task::spawn_blocking({
        let from = partial_path.clone();
        let to = snap_path.clone();
        move || std::fs::rename(&from, &to)
    })
    .await
    .map_err(|e| ClusterError::PartialSnapshotCorrupt {
        group_id,
        detail: format!("spawn_blocking join error on rename: {e}"),
    })?
    .map_err(|e| ClusterError::Storage {
        detail: format!("rename partial to snap for group {group_id}: {e}"),
    })?;

    // Advance Raft log boundary. Build a minimal InstallSnapshotRequest
    // that satisfies `handle_install_snapshot` — `data` is the assembled
    // bytes (may be empty for the bootstrap stub), `done` is always `true`.
    let req = InstallSnapshotRequest {
        term: state.term,
        leader_id: state.leader_id,
        last_included_index: state.last_included_index,
        last_included_term: state.last_included_term,
        offset: 0,
        data: file_bytes,
        done: true,
        group_id,
        total_size: 0,
    };

    let mut mr = multi_raft.lock().unwrap_or_else(|p| p.into_inner());
    let resp = mr.handle_install_snapshot(&req)?;
    Ok(resp)
}

/// Derive the `.snap` path from the `.partial` path (same directory, stem only).
fn snap_path_for(partial: &std::path::Path) -> PathBuf {
    let parent = partial
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let stem = partial
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("unknown"));
    parent.join(format!("{}.snap", stem.to_string_lossy()))
}
