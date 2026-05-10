// SPDX-License-Identifier: BUSL-1.1

//! Cross-cluster snapshot transfer for mirror bootstrap.
//!
//! When a mirror database is first created (or re-created after a data loss
//! event) it must obtain a full consistent snapshot from the source cluster
//! before it can stream AppendEntries.  This module provides:
//!
//! - [`CrossClusterSnapshotEnvelope`]: the wire envelope that wraps an
//!   individual snapshot chunk.  It nests directly into the existing
//!   [`InstallSnapshotRequest`] data field, so the existing in-cluster
//!   receiver machinery handles chunk reassembly.  The envelope adds
//!   `source_cluster_id`, `database_id`, `snapshot_lsn`, and `total_crc32c`
//!   so the receiver can verify provenance and detect bit-rot.
//!
//! - [`MirrorBootstrapReceiver`]: the mirror-side chunk accumulator.
//!   Writes chunks to a `.partial` file (same directory convention as the
//!   in-cluster receiver), maintains a running CRC32C, and validates it
//!   against `total_crc32c` on the final chunk.  Updates
//!   `MirrorStatus::Bootstrapping { bytes_done, bytes_total }` via a
//!   provided callback every `PROGRESS_REPORT_CHUNK_BYTES` (~1 MiB), and
//!   transitions to `MirrorStatus::Following` once the final chunk is
//!   committed.
//!
//! # Resume semantics
//!
//! On mirror restart mid-bootstrap, the receiver discards any existing
//! `.partial` file and requests a fresh snapshot from the source (via
//! the link reconnect path in [`super::link`]).  This avoids the need to
//! re-hash the partial file to rebuild a running CRC, at the cost of one
//! extra snapshot round-trip.

mod envelope;
mod receiver;

pub use envelope::{
    BootstrapChunkOutcome, CrossClusterSnapshotEnvelope, PROGRESS_REPORT_CHUNK_BYTES,
    ProgressCallback,
};
pub use receiver::MirrorBootstrapReceiver;
