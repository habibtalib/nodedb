// SPDX-License-Identifier: Apache-2.0

//! Mirror catalog types shared between the catalog layer and the control plane.
//!
//! A mirror database is a continuously-updated read-only replica of a source
//! database in another cluster. Promotion to writable is one-way and permanent.
//! Exhaustive matches are required everywhere these enums are matched — no
//! `_ =>` arms.

use serde::{Deserialize, Serialize};

use crate::{DatabaseId, Lsn};

/// Identifies the source of a mirror database.
///
/// Stored on `DatabaseDescriptor.mirror_origin`; the read planner and write
/// rejector consult it to enforce mirror semantics.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
#[msgpack(map)]
pub struct MirrorOrigin {
    /// The cluster that the source database lives in.
    pub source_cluster: String,
    /// The source database being mirrored.
    pub source_database: DatabaseId,
    /// Replication mode: whether the source waits for mirror ack.
    pub mode: MirrorMode,
    /// WAL LSN last applied on this mirror.
    pub last_applied: Lsn,
    /// Current mirror lifecycle status.
    pub status: MirrorStatus,
}

/// Replication mode for a mirror database.
///
/// Exhaustive matches are required everywhere this enum is matched — no
/// `_ =>` arms.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub enum MirrorMode {
    /// Source waits for mirror ack before commit. Strict latency cost;
    /// not recommended cross-region.
    Sync,
    /// Mirror trails source; lag is observable via `MirrorStatus::Degraded`.
    Async,
}

/// Lifecycle status of a mirror database.
///
/// Exhaustive matches are required everywhere this enum is matched — no
/// `_ =>` arms.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub enum MirrorStatus {
    /// Initial snapshot transfer is in progress.
    Bootstrapping {
        /// Bytes of snapshot data received so far.
        bytes_done: u64,
        /// Total snapshot size in bytes (0 = unknown).
        bytes_total: u64,
    },
    /// Log replication is active; mirror is caught up within normal lag bounds.
    Following,
    /// Mirror is receiving entries but has fallen behind the lag threshold.
    Degraded {
        /// Observed replication lag in milliseconds.
        lag_ms: u64,
    },
    /// Source is unreachable; mirror is serving stale reads with growing lag.
    Disconnected,
    /// Mirror was promoted to a writable database. Source link is severed.
    /// `mirror_origin` is retained as a historical lineage record.
    Promoted,
}

/// Per-mirror lag record persisted in `_system.mirror_lag`.
///
/// Read by the metrics collector and the `SHOW DATABASE MIRROR STATUS` handler
/// to produce the observable replication lag.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
#[msgpack(map)]
pub struct MirrorLagRecord {
    /// WAL LSN of the last entry successfully applied on this mirror.
    pub last_applied_lsn: Lsn,
    /// Wall-clock milliseconds (UNIX epoch) when `last_applied_lsn` was applied.
    pub last_apply_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_msgpack<T>(val: &T) -> T
    where
        T: zerompk::ToMessagePack + for<'a> zerompk::FromMessagePack<'a>,
    {
        let bytes = zerompk::to_msgpack_vec(val).expect("serialize");
        zerompk::from_msgpack(&bytes).expect("deserialize")
    }

    fn round_trip_serde<T>(val: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = sonic_rs::to_string(val).expect("serde serialize");
        sonic_rs::from_str(&json).expect("serde deserialize")
    }

    fn sample_origin() -> MirrorOrigin {
        MirrorOrigin {
            source_cluster: "prod-us".to_string(),
            source_database: DatabaseId::DEFAULT,
            mode: MirrorMode::Async,
            last_applied: Lsn::new(12_345),
            status: MirrorStatus::Following,
        }
    }

    #[test]
    fn mirror_mode_msgpack_roundtrip() {
        assert_eq!(round_trip_msgpack(&MirrorMode::Sync), MirrorMode::Sync);
        assert_eq!(round_trip_msgpack(&MirrorMode::Async), MirrorMode::Async);
    }

    #[test]
    fn mirror_mode_serde_roundtrip() {
        assert_eq!(round_trip_serde(&MirrorMode::Sync), MirrorMode::Sync);
        assert_eq!(round_trip_serde(&MirrorMode::Async), MirrorMode::Async);
    }

    #[test]
    fn mirror_status_following_msgpack() {
        let s = MirrorStatus::Following;
        assert_eq!(round_trip_msgpack(&s), s);
    }

    #[test]
    fn mirror_status_bootstrapping_msgpack() {
        let s = MirrorStatus::Bootstrapping {
            bytes_done: 1024,
            bytes_total: 4096,
        };
        assert_eq!(round_trip_msgpack(&s), s);
    }

    #[test]
    fn mirror_status_degraded_msgpack() {
        let s = MirrorStatus::Degraded { lag_ms: 7500 };
        assert_eq!(round_trip_msgpack(&s), s);
    }

    #[test]
    fn mirror_status_disconnected_msgpack() {
        let s = MirrorStatus::Disconnected;
        assert_eq!(round_trip_msgpack(&s), s);
    }

    #[test]
    fn mirror_status_promoted_msgpack() {
        let s = MirrorStatus::Promoted;
        assert_eq!(round_trip_msgpack(&s), s);
    }

    #[test]
    fn mirror_status_serde_roundtrip() {
        for s in [
            MirrorStatus::Following,
            MirrorStatus::Bootstrapping {
                bytes_done: 0,
                bytes_total: 0,
            },
            MirrorStatus::Degraded { lag_ms: 5001 },
            MirrorStatus::Disconnected,
            MirrorStatus::Promoted,
        ] {
            assert_eq!(round_trip_serde(&s), s);
        }
    }

    #[test]
    fn mirror_origin_msgpack_roundtrip() {
        let o = sample_origin();
        assert_eq!(round_trip_msgpack(&o), o);
    }

    #[test]
    fn mirror_origin_serde_roundtrip() {
        let o = sample_origin();
        assert_eq!(round_trip_serde(&o), o);
    }
}
