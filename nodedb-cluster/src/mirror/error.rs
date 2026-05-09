// SPDX-License-Identifier: BUSL-1.1

//! Error types for cross-cluster mirror transport and bootstrap.

use thiserror::Error;

/// Errors produced by cross-cluster mirror operations.
///
/// These wrap into [`crate::error::ClusterError::Mirror`] at the public
/// boundary so callers have a single error type to handle.
#[derive(Debug, Error)]
pub enum MirrorError {
    /// The remote cluster refused our connection because the `source_cluster`
    /// we declared does not match their own cluster id.
    #[error(
        "cluster-id mismatch: we declared source_cluster={declared:?}, \
         remote reports its id as {remote:?}"
    )]
    ClusterIdMismatch { declared: String, remote: String },

    /// The remote cluster rejected the mirror link because the connecting
    /// peer presented `Observer` role credentials but tried to perform a
    /// voter operation (vote request, conf change, etc.).
    #[error("observer-role violation: peer attempted voter operation: {detail}")]
    ObserverRoleViolation { detail: String },

    /// Snapshot transfer was aborted because a chunk arrived out of order.
    ///
    /// The receiver resets and requests a fresh snapshot from the source.
    #[error(
        "snapshot offset regression for database {database_id:?}: \
         expected {expected}, got {actual}"
    )]
    SnapshotOffsetRegression {
        database_id: String,
        expected: u64,
        actual: u64,
    },

    /// Snapshot CRC validation failed at the final chunk.
    #[error(
        "snapshot CRC mismatch for database {database_id:?}: \
         stored {stored:#010x}, computed {computed:#010x}"
    )]
    SnapshotCrcMismatch {
        database_id: String,
        stored: u32,
        computed: u32,
    },

    /// The cross-cluster handshake wire message could not be decoded.
    #[error("cross-cluster handshake codec error: {detail}")]
    HandshakeCodec { detail: String },

    /// The mirror declared a wire protocol version the source does not
    /// implement (or vice versa).  Surfaced immediately without retry — the
    /// peers must be upgraded in lockstep.
    #[error("cross-cluster protocol version mismatch: local={local}, remote_detail={detail:?}")]
    ProtocolVersionMismatch { local: u16, detail: String },

    /// QUIC transport error during cross-cluster operations.
    #[error("cross-cluster transport error: {detail}")]
    Transport { detail: String },

    /// Observer-side bytes-in-flight cap was exceeded; the source should
    /// pause sending until the observer drains.
    #[error(
        "bytes-in-flight cap exceeded: in_flight={in_flight}, cap={cap} for mirror {database_id:?}"
    )]
    BytesInFlightCapExceeded {
        database_id: String,
        in_flight: u64,
        cap: u64,
    },

    /// The mirror is in `Promoted` state and can no longer accept replication.
    #[error("mirror {database_id:?} is promoted and no longer accepts replication")]
    MirrorPromoted { database_id: String },
}
