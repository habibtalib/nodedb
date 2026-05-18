// SPDX-License-Identifier: Apache-2.0

//! Self-describing envelope used by quantization codecs that persist their
//! calibrated parameters (BBQ, RaBitQ, …).
//!
//! Wire layout: `magic` (5 bytes) + `version` (1 byte) + MessagePack body
//! produced by `zerompk`. Codecs choose their own 5-byte magic (e.g.
//! `b"NDBBQ"`, `b"NDRBQ"`) so a mismatched buffer fails fast with a typed
//! [`CodecError`] instead of an opaque MessagePack decode error.

use crate::error::CodecError;
use zerompk::{FromMessagePack, ToMessagePack};

/// Length of the magic prefix.
pub const MAGIC_LEN: usize = 5;

/// Total length of the envelope header (`magic` + `version`).
pub const HEADER_LEN: usize = MAGIC_LEN + 1;

/// Serialize `value` into a versioned, magic-tagged byte buffer.
pub fn encode<T: ToMessagePack>(
    magic: &[u8; MAGIC_LEN],
    version: u8,
    value: &T,
) -> Result<Vec<u8>, CodecError> {
    let body = zerompk::to_msgpack_vec(value).map_err(|e| CodecError::Corrupt {
        detail: e.to_string(),
    })?;
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(magic);
    out.push(version);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Validate the envelope header and deserialize the body into `T`.
///
/// `expected_version` is checked exactly — bumping a codec's on-disk format
/// is an explicit decision; silently accepting older bodies has bitten us
/// before. Callers that need a window of compatible versions should call
/// [`peek_version`] and dispatch.
pub fn decode<T: for<'de> FromMessagePack<'de>>(
    magic: &[u8; MAGIC_LEN],
    expected_version: u8,
    buf: &[u8],
) -> Result<T, CodecError> {
    if buf.len() < HEADER_LEN {
        return Err(CodecError::Truncated {
            expected: HEADER_LEN,
            actual: buf.len(),
        });
    }
    if &buf[..MAGIC_LEN] != magic {
        return Err(CodecError::Corrupt {
            detail: "bad magic".into(),
        });
    }
    let got = buf[MAGIC_LEN];
    if got != expected_version {
        return Err(CodecError::Corrupt {
            detail: format!("unsupported version {got}"),
        });
    }
    zerompk::from_msgpack(&buf[HEADER_LEN..]).map_err(|e| CodecError::Corrupt {
        detail: e.to_string(),
    })
}

/// Read the version byte without decoding the body. Returns `None` if the
/// buffer is too short or the magic does not match.
pub fn peek_version(magic: &[u8; MAGIC_LEN], buf: &[u8]) -> Option<u8> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    if &buf[..MAGIC_LEN] != magic {
        return None;
    }
    Some(buf[MAGIC_LEN])
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: &[u8; MAGIC_LEN] = b"NDTST";

    #[test]
    fn roundtrip() {
        let payload: Vec<i32> = vec![1, -2, 3, -4];
        let bytes = encode(MAGIC, 7, &payload).unwrap();
        let restored: Vec<i32> = decode(MAGIC, 7, &bytes).unwrap();
        assert_eq!(restored, payload);
    }

    #[test]
    fn rejects_short_buffer() {
        let err = decode::<Vec<u8>>(MAGIC, 1, &[0u8; HEADER_LEN - 1]).unwrap_err();
        matches!(err, CodecError::Truncated { .. });
    }

    #[test]
    fn rejects_bad_magic() {
        let bytes = encode(b"NDOTH", 1, &0u8).unwrap();
        let err = decode::<u8>(MAGIC, 1, &bytes).unwrap_err();
        matches!(err, CodecError::Corrupt { .. });
    }

    #[test]
    fn rejects_version_mismatch() {
        let bytes = encode(MAGIC, 1, &0u8).unwrap();
        let err = decode::<u8>(MAGIC, 2, &bytes).unwrap_err();
        matches!(err, CodecError::Corrupt { .. });
    }

    #[test]
    fn peek_version_returns_version_byte() {
        let bytes = encode(MAGIC, 9, &0u8).unwrap();
        assert_eq!(peek_version(MAGIC, &bytes), Some(9));
        assert_eq!(peek_version(b"NDOTH", &bytes), None);
        assert_eq!(peek_version(MAGIC, &[]), None);
    }
}
