// SPDX-License-Identifier: Apache-2.0

//! MsgPack decoding for [`ErrorDetails`].

use zerompk::{FromMessagePack, Read};

use crate::error::details::ErrorDetails;
use crate::sync::compensation::CompensationHint;

use super::constants::*;

/// Read the 2-element outer array and return `(tag, field_count)`.
#[inline]
fn read_header<'a, R: Read<'a>>(reader: &mut R) -> zerompk::Result<(u16, usize)> {
    let outer = reader.read_array_len()?;
    if outer != 2 {
        return Err(zerompk::Error::ArrayLengthMismatch {
            expected: 2,
            actual: outer,
        });
    }
    let tag = reader.read_u16()?;
    let field_count = reader.read_map_len()?;
    Ok((tag, field_count))
}

/// Skip all remaining fields in a variant payload map.
#[inline]
fn skip_fields<'a, R: Read<'a>>(reader: &mut R, count: usize) -> zerompk::Result<()> {
    for _ in 0..count {
        reader.read_u8()?;
        reader.skip_value()?;
    }
    Ok(())
}

/// Skip one arbitrary MessagePack value.
#[inline]
fn skip_one<'a, R: Read<'a>>(reader: &mut R) -> zerompk::Result<()> {
    reader.skip_value()
}

fn read_u8_field<'a, R: Read<'a>>(reader: &mut R, field_count: usize) -> zerompk::Result<u8> {
    if field_count < 1 {
        return Err(zerompk::Error::InvalidMarker(0));
    }
    let _k = reader.read_u8()?;
    let v = reader.read_u8()?;
    for _ in 1..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok(v)
}

fn read1_str<'a, R: Read<'a>>(reader: &mut R, field_count: usize) -> zerompk::Result<(String,)> {
    if field_count < 1 {
        return Err(zerompk::Error::InvalidMarker(0));
    }
    let _k = reader.read_u8()?;
    let v = reader.read_string()?.into_owned();
    for _ in 1..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((v,))
}

fn read2_str<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<(String, String)> {
    if field_count < 2 {
        return Err(zerompk::Error::InvalidMarker(0));
    }
    let _k1 = reader.read_u8()?;
    let v1 = reader.read_string()?.into_owned();
    let _k2 = reader.read_u8()?;
    let v2 = reader.read_string()?.into_owned();
    for _ in 2..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((v1, v2))
}

fn read_collection_deactivated<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<(String, u64, String)> {
    if field_count < 3 {
        return Err(zerompk::Error::InvalidMarker(0));
    }
    let _k1 = reader.read_u8()?;
    let collection = reader.read_string()?.into_owned();
    let _k2 = reader.read_u8()?;
    let retention_expires_at_ns = reader.read_u64()?;
    let _k3 = reader.read_u8()?;
    let undrop_hint = reader.read_string()?.into_owned();
    for _ in 3..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((collection, retention_expires_at_ns, undrop_hint))
}

/// Read two `u32` fields, tolerating `field_count < 2` by substituting `0`.
fn read2_u32<'a, R: Read<'a>>(reader: &mut R, field_count: usize) -> zerompk::Result<(u32, u32)> {
    let v1 = if field_count >= 1 {
        reader.read_u8()?;
        reader.read_u32()?
    } else {
        0
    };
    let v2 = if field_count >= 2 {
        reader.read_u8()?;
        reader.read_u32()?
    } else {
        0
    };
    for _ in 2..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((v1, v2))
}

/// Read two `u64` fields, tolerating `field_count < 2` by substituting `0`.
fn read2_u64<'a, R: Read<'a>>(reader: &mut R, field_count: usize) -> zerompk::Result<(u64, u64)> {
    let v1 = if field_count >= 1 {
        reader.read_u8()?;
        reader.read_u64()?
    } else {
        0
    };
    let v2 = if field_count >= 2 {
        reader.read_u8()?;
        reader.read_u64()?
    } else {
        0
    };
    for _ in 2..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((v1, v2))
}

/// Read a single array field containing strings.
/// The field layout is: `{key: u8, value: [str, ...]}`.
fn read_string_vec<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<Vec<String>> {
    if field_count < 1 {
        return Ok(Vec::new());
    }
    reader.read_u8()?; // key
    let len = reader.read_array_len()?;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        out.push(reader.read_string()?.into_owned());
    }
    for _ in 1..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok(out)
}

fn read_fan_out<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<(u16, u16)> {
    if field_count < 2 {
        return Err(zerompk::Error::InvalidMarker(0));
    }
    let _k1 = reader.read_u8()?;
    let shards_touched = reader.read_u16()?;
    let _k2 = reader.read_u8()?;
    let limit = reader.read_u16()?;
    for _ in 2..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((shards_touched, limit))
}

/// Read 2 string fields, tolerating `field_count < 2` by filling missing
/// fields with `"unspecified"`.
fn read2_str_tolerant<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<(String, String)> {
    let v1 = if field_count >= 1 {
        reader.read_u8()?;
        reader.read_string()?.into_owned()
    } else {
        "unspecified".into()
    };
    let v2 = if field_count >= 2 {
        reader.read_u8()?;
        reader.read_string()?.into_owned()
    } else {
        "unspecified".into()
    };
    for _ in 2..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((v1, v2))
}

/// Read 3 string fields, tolerating `field_count < 3` by filling missing
/// fields with `"unspecified"`.
fn read3_str_tolerant<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<(String, String, String)> {
    let v1 = if field_count >= 1 {
        reader.read_u8()?;
        reader.read_string()?.into_owned()
    } else {
        "unspecified".into()
    };
    let v2 = if field_count >= 2 {
        reader.read_u8()?;
        reader.read_string()?.into_owned()
    } else {
        "unspecified".into()
    };
    let v3 = if field_count >= 3 {
        reader.read_u8()?;
        reader.read_string()?.into_owned()
    } else {
        "unspecified".into()
    };
    for _ in 3..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((v1, v2, v3))
}

/// Read `SegmentCorrupted` fields: (segment_id: u64, corruption: String, detail: String).
/// Tolerates `field_count < 3`; missing `segment_id` defaults to `0`.
fn read_segment_corrupted_tolerant<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<(u64, String, String)> {
    let segment_id = if field_count >= 1 {
        reader.read_u8()?;
        reader.read_u64()?
    } else {
        0
    };
    let corruption = if field_count >= 2 {
        reader.read_u8()?;
        reader.read_string()?.into_owned()
    } else {
        "unspecified".into()
    };
    let detail = if field_count >= 3 {
        reader.read_u8()?;
        reader.read_string()?.into_owned()
    } else {
        "unspecified".into()
    };
    for _ in 3..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok((segment_id, corruption, detail))
}

fn read_sync_delta_rejected<'a, R: Read<'a>>(
    reader: &mut R,
    field_count: usize,
) -> zerompk::Result<Option<CompensationHint>> {
    if field_count < 1 {
        return Ok(None);
    }
    let _k = reader.read_u8()?;
    let compensation = Option::<CompensationHint>::read(reader)?;
    for _ in 1..field_count {
        reader.read_u8()?;
        skip_one(reader)?;
    }
    Ok(compensation)
}

impl<'a> FromMessagePack<'a> for ErrorDetails {
    fn read<R: Read<'a>>(reader: &mut R) -> zerompk::Result<Self> {
        let (tag, field_count) = read_header(reader)?;
        match tag {
            TAG_CONSTRAINT_VIOLATION => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::ConstraintViolation { collection })
            }
            TAG_WRITE_CONFLICT => {
                let (collection, document_id) = read2_str(reader, field_count)?;
                Ok(ErrorDetails::WriteConflict {
                    collection,
                    document_id,
                })
            }
            TAG_DEADLINE_EXCEEDED => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::DeadlineExceeded)
            }
            TAG_PREVALIDATION_REJECTED => {
                let (constraint,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::PrevalidationRejected { constraint })
            }
            TAG_APPEND_ONLY_VIOLATION => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::AppendOnlyViolation { collection })
            }
            TAG_BALANCE_VIOLATION => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::BalanceViolation { collection })
            }
            TAG_PERIOD_LOCKED => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::PeriodLocked { collection })
            }
            TAG_STATE_TRANSITION_VIOLATION => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::StateTransitionViolation { collection })
            }
            TAG_TRANSITION_CHECK_VIOLATION => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::TransitionCheckViolation { collection })
            }
            TAG_TYPE_GUARD_VIOLATION => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::TypeGuardViolation { collection })
            }
            TAG_RETENTION_VIOLATION => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::RetentionViolation { collection })
            }
            TAG_LEGAL_HOLD_ACTIVE => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::LegalHoldActive { collection })
            }
            TAG_TYPE_MISMATCH => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::TypeMismatch { collection })
            }
            TAG_OVERFLOW => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::Overflow { collection })
            }
            TAG_INSUFFICIENT_BALANCE => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::InsufficientBalance { collection })
            }
            TAG_RATE_EXCEEDED => {
                let (gate,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::RateExceeded { gate })
            }
            TAG_COLLECTION_NOT_FOUND => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::CollectionNotFound { collection })
            }
            TAG_DOCUMENT_NOT_FOUND => {
                let (collection, document_id) = read2_str(reader, field_count)?;
                Ok(ErrorDetails::DocumentNotFound {
                    collection,
                    document_id,
                })
            }
            TAG_COLLECTION_DRAINING => {
                let (collection,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::CollectionDraining { collection })
            }
            TAG_COLLECTION_DEACTIVATED => {
                let (collection, retention_expires_at_ns, undrop_hint) =
                    read_collection_deactivated(reader, field_count)?;
                Ok(ErrorDetails::CollectionDeactivated {
                    collection,
                    retention_expires_at_ns,
                    undrop_hint,
                })
            }
            TAG_PLAN_ERROR => {
                let (phase, detail) = read2_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::PlanError { phase, detail })
            }
            TAG_FAN_OUT_EXCEEDED => {
                let (shards_touched, limit) = read_fan_out(reader, field_count)?;
                Ok(ErrorDetails::FanOutExceeded {
                    shards_touched,
                    limit,
                })
            }
            TAG_SQL_NOT_ENABLED => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::SqlNotEnabled)
            }
            TAG_AUTHORIZATION_DENIED => {
                let (resource,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::AuthorizationDenied { resource })
            }
            TAG_AUTH_EXPIRED => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::AuthExpired)
            }
            TAG_SYNC_CONNECTION_FAILED => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::SyncConnectionFailed)
            }
            TAG_SYNC_DELTA_REJECTED => {
                let compensation = read_sync_delta_rejected(reader, field_count)?;
                Ok(ErrorDetails::SyncDeltaRejected { compensation })
            }
            TAG_SHAPE_SUBSCRIPTION_FAILED => {
                let (shape_id,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::ShapeSubscriptionFailed { shape_id })
            }
            TAG_STORAGE => {
                let (component, op, detail) = read3_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::Storage {
                    component,
                    op,
                    detail,
                })
            }
            TAG_SEGMENT_CORRUPTED => {
                let (segment_id, corruption, detail) =
                    read_segment_corrupted_tolerant(reader, field_count)?;
                Ok(ErrorDetails::SegmentCorrupted {
                    segment_id,
                    corruption,
                    detail,
                })
            }
            TAG_COLD_STORAGE => {
                let (backend, op, detail) = read3_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::ColdStorage {
                    backend,
                    op,
                    detail,
                })
            }
            TAG_WAL => {
                let (stage, detail) = read2_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::Wal { stage, detail })
            }
            TAG_SERIALIZATION => {
                let (format,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::Serialization { format })
            }
            TAG_CODEC => {
                let (codec, op, detail) = read3_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::Codec { codec, op, detail })
            }
            TAG_CONFIG => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::Config)
            }
            TAG_BAD_REQUEST => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::BadRequest)
            }
            TAG_NO_LEADER => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::NoLeader)
            }
            TAG_NOT_LEADER => {
                let (leader_addr,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::NotLeader { leader_addr })
            }
            TAG_MIGRATION_IN_PROGRESS => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::MigrationInProgress)
            }
            TAG_NODE_UNREACHABLE => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::NodeUnreachable)
            }
            TAG_CLUSTER => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::Cluster)
            }
            TAG_MEMORY_EXHAUSTED => {
                let (engine,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::MemoryExhausted { engine })
            }
            TAG_ENCRYPTION => {
                let (cipher, detail) = read2_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::Encryption { cipher, detail })
            }
            TAG_ARRAY => {
                let (array,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::Array { array })
            }
            TAG_BRIDGE => {
                let (plane, op, detail) = read3_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::Bridge { plane, op, detail })
            }
            TAG_DISPATCH => {
                let (stage, detail) = read2_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::Dispatch { stage, detail })
            }
            TAG_HANDSHAKE_FAILED => {
                let server_code = read_u8_field(reader, field_count)?;
                Ok(ErrorDetails::HandshakeFailed { server_code })
            }
            TAG_UNSUPPORTED_OPCODE => {
                // Legacy tag — no matching variant. Treat as Internal.
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::Internal {
                    component: "opcode".into(),
                    detail: "unsupported opcode".into(),
                })
            }
            TAG_INTERNAL => {
                let (component, detail) = read2_str_tolerant(reader, field_count)?;
                Ok(ErrorDetails::Internal { component, detail })
            }
            TAG_TENANT_VECTOR_DIM_EXCEEDED => {
                let (dim, limit) = read2_u32(reader, field_count)?;
                Ok(ErrorDetails::TenantVectorDimExceeded { dim, limit })
            }
            TAG_TENANT_GRAPH_DEPTH_EXCEEDED => {
                let (depth, limit) = read2_u32(reader, field_count)?;
                Ok(ErrorDetails::TenantGraphDepthExceeded { depth, limit })
            }
            TAG_QUOTA_OVERCOMMIT => {
                let (field,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::QuotaOvercommit { field })
            }
            TAG_QUOTA_EXCEEDED => {
                let (scope,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::QuotaExceeded { scope })
            }
            TAG_SERVER_OVERLOAD => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::ServerOverload)
            }
            TAG_CLONE_DEPTH_EXCEEDED => {
                let (depth, limit) = read2_u32(reader, field_count)?;
                Ok(ErrorDetails::CloneDepthExceeded { depth, limit })
            }
            TAG_CANNOT_CLONE_MIRROR => {
                let (database,) = read1_str(reader, field_count)?;
                Ok(ErrorDetails::CannotCloneMirror { database })
            }
            TAG_CLONE_DEPENDENCY => {
                // field_count fields: the array of dependents.
                let dependents = read_string_vec(reader, field_count)?;
                Ok(ErrorDetails::CloneDependency { dependents })
            }
            TAG_CLONE_PREDATES_QUERY_TIME => {
                let (as_of_lsn, created_at_lsn) = read2_u64(reader, field_count)?;
                Ok(ErrorDetails::ClonePredatesQueryTime {
                    as_of_lsn,
                    created_at_lsn,
                })
            }
            _unknown => {
                skip_fields(reader, field_count)?;
                Ok(ErrorDetails::Internal {
                    component: "unspecified".into(),
                    detail: "unspecified".into(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::sync::compensation::CompensationHint;

    use super::*;

    fn roundtrip(d: &ErrorDetails) -> ErrorDetails {
        let bytes = zerompk::to_msgpack_vec(d).expect("encode");
        zerompk::from_msgpack(&bytes).expect("decode")
    }

    #[test]
    fn unit_variant_roundtrip() {
        for v in [
            ErrorDetails::DeadlineExceeded,
            ErrorDetails::SqlNotEnabled,
            ErrorDetails::AuthExpired,
            ErrorDetails::SyncConnectionFailed,
            ErrorDetails::Config,
            ErrorDetails::BadRequest,
            ErrorDetails::NoLeader,
            ErrorDetails::MigrationInProgress,
            ErrorDetails::NodeUnreachable,
            ErrorDetails::Cluster,
        ] {
            assert_eq!(roundtrip(&v), v, "unit variant roundtrip failed: {v:?}");
        }
    }

    #[test]
    fn storage_enriched_roundtrip() {
        let v = ErrorDetails::Storage {
            component: "redb".into(),
            op: "write".into(),
            detail: "disk full".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn segment_corrupted_enriched_roundtrip() {
        let v = ErrorDetails::SegmentCorrupted {
            segment_id: 42,
            corruption: "crc_mismatch".into(),
            detail: "footer checksum invalid".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn cold_storage_enriched_roundtrip() {
        let v = ErrorDetails::ColdStorage {
            backend: "s3".into(),
            op: "get_object".into(),
            detail: "403 forbidden".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn wal_enriched_roundtrip() {
        let v = ErrorDetails::Wal {
            stage: "fsync".into(),
            detail: "io_uring submission failed".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn codec_enriched_roundtrip() {
        let v = ErrorDetails::Codec {
            codec: "alp".into(),
            op: "encode".into(),
            detail: "unsupported exponent range".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn encryption_enriched_roundtrip() {
        let v = ErrorDetails::Encryption {
            cipher: "aes_gcm".into(),
            detail: "authentication tag mismatch".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn plan_error_enriched_roundtrip() {
        let v = ErrorDetails::PlanError {
            phase: "logical".into(),
            detail: "ambiguous column reference".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn bridge_enriched_roundtrip() {
        let v = ErrorDetails::Bridge {
            plane: "data".into(),
            op: "dispatch".into(),
            detail: "ring buffer full".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn dispatch_enriched_roundtrip() {
        let v = ErrorDetails::Dispatch {
            stage: "route".into(),
            detail: "vshard not found".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn internal_enriched_roundtrip() {
        let v = ErrorDetails::Internal {
            component: "compaction".into(),
            detail: "unreachable state".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    fn decode_zero_fields(tag: u16) -> ErrorDetails {
        // Manual MessagePack: [fixarray(2), uint16(tag), fixmap(0)]
        let buf = [0x92, 0xcd, (tag >> 8) as u8, (tag & 0xff) as u8, 0x80];
        zerompk::from_msgpack(&buf).expect("decode")
    }

    #[test]
    fn storage_compat_zero_fields() {
        let v = decode_zero_fields(TAG_STORAGE);
        assert_eq!(
            v,
            ErrorDetails::Storage {
                component: "unspecified".into(),
                op: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn segment_corrupted_compat_zero_fields() {
        let v = decode_zero_fields(TAG_SEGMENT_CORRUPTED);
        assert_eq!(
            v,
            ErrorDetails::SegmentCorrupted {
                segment_id: 0,
                corruption: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn cold_storage_compat_zero_fields() {
        let v = decode_zero_fields(TAG_COLD_STORAGE);
        assert_eq!(
            v,
            ErrorDetails::ColdStorage {
                backend: "unspecified".into(),
                op: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn wal_compat_zero_fields() {
        let v = decode_zero_fields(TAG_WAL);
        assert_eq!(
            v,
            ErrorDetails::Wal {
                stage: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn codec_compat_zero_fields() {
        let v = decode_zero_fields(TAG_CODEC);
        assert_eq!(
            v,
            ErrorDetails::Codec {
                codec: "unspecified".into(),
                op: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn encryption_compat_zero_fields() {
        let v = decode_zero_fields(TAG_ENCRYPTION);
        assert_eq!(
            v,
            ErrorDetails::Encryption {
                cipher: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn plan_error_compat_zero_fields() {
        let v = decode_zero_fields(TAG_PLAN_ERROR);
        assert_eq!(
            v,
            ErrorDetails::PlanError {
                phase: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn bridge_compat_zero_fields() {
        let v = decode_zero_fields(TAG_BRIDGE);
        assert_eq!(
            v,
            ErrorDetails::Bridge {
                plane: "unspecified".into(),
                op: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn dispatch_compat_zero_fields() {
        let v = decode_zero_fields(TAG_DISPATCH);
        assert_eq!(
            v,
            ErrorDetails::Dispatch {
                stage: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn internal_compat_zero_fields() {
        let v = decode_zero_fields(TAG_INTERNAL);
        assert_eq!(
            v,
            ErrorDetails::Internal {
                component: "unspecified".into(),
                detail: "unspecified".into(),
            }
        );
    }

    #[test]
    fn single_string_field_roundtrip() {
        let variants = vec![
            ErrorDetails::ConstraintViolation {
                collection: "orders".into(),
            },
            ErrorDetails::AppendOnlyViolation {
                collection: "ledger".into(),
            },
            ErrorDetails::CollectionNotFound {
                collection: "users".into(),
            },
            ErrorDetails::AuthorizationDenied {
                resource: "orders.*".into(),
            },
            ErrorDetails::MemoryExhausted {
                engine: "vector".into(),
            },
            ErrorDetails::Array {
                array: "arr1".into(),
            },
            ErrorDetails::NotLeader {
                leader_addr: "10.0.0.1:6432".into(),
            },
        ];
        for v in variants {
            assert_eq!(roundtrip(&v), v, "single-string roundtrip failed: {v:?}");
        }
    }

    #[test]
    fn two_string_field_roundtrip() {
        let v = ErrorDetails::WriteConflict {
            collection: "orders".into(),
            document_id: "ord-42".into(),
        };
        assert_eq!(roundtrip(&v), v);

        let v2 = ErrorDetails::DocumentNotFound {
            collection: "users".into(),
            document_id: "u-99".into(),
        };
        assert_eq!(roundtrip(&v2), v2);
    }

    #[test]
    fn collection_deactivated_roundtrip() {
        let v = ErrorDetails::CollectionDeactivated {
            collection: "old_logs".into(),
            retention_expires_at_ns: 1_700_000_000_000_u64,
            undrop_hint: "UNDROP COLLECTION old_logs".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn fan_out_exceeded_roundtrip() {
        let v = ErrorDetails::FanOutExceeded {
            shards_touched: 100,
            limit: 50,
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn sync_delta_rejected_with_hint_roundtrip() {
        let v = ErrorDetails::SyncDeltaRejected {
            compensation: Some(CompensationHint::UniqueViolation {
                field: "email".into(),
                conflicting_value: "a@b.com".into(),
            }),
        };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn sync_delta_rejected_no_hint_roundtrip() {
        let v = ErrorDetails::SyncDeltaRejected { compensation: None };
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn serialization_roundtrip() {
        let v = ErrorDetails::Serialization {
            format: "msgpack".into(),
        };
        assert_eq!(roundtrip(&v), v);
    }
}
