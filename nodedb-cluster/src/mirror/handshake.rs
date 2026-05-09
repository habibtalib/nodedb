// SPDX-License-Identifier: BUSL-1.1

//! Cross-cluster mirror handshake wire protocol.
//!
//! When a mirror cluster opens a QUIC connection to the source cluster it
//! sends a [`MirrorHello`] on the first bidi stream.  The source replies
//! with a [`MirrorHelloAck`].  Only after this exchange succeeds does the
//! link transition to entry-streaming / snapshot-transfer mode.
//!
//! # Authentication
//!
//! The `source_cluster` field in [`MirrorHello`] is the cluster-id string
//! the mirror declares.  The source verifies it matches its own cluster-id
//! and rejects the connection otherwise (error code
//! [`MIRROR_HELLO_ERR_CLUSTER_ID`]).
//!
//! # Observer-role enforcement
//!
//! The source tracks this connection as `PeerRole::Observer`.  Any attempt
//! by the mirror to send a voter-class RPC (RequestVote, ConfChange) over
//! the same connection is rejected with [`MIRROR_HELLO_ERR_OBSERVER_ONLY`].
//!
//! # Wire format
//!
//! Both messages use zerompk (MessagePack) encoding, length-prefixed with
//! a 4-byte big-endian frame length, matching the existing rpc_codec
//! framing convention.  The discriminant byte precedes the MessagePack
//! payload so the decoder can branch without buffering the full payload.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::error::MirrorError;

/// Discriminant byte for [`MirrorHello`].
pub const MIRROR_HELLO: u8 = 0x01;
/// Discriminant byte for [`MirrorHelloAck`].
pub const MIRROR_HELLO_ACK: u8 = 0x02;

/// Error code: source cluster-id mismatch.
pub const MIRROR_HELLO_ERR_CLUSTER_ID: u8 = 0x01;
/// Error code: the peer attempted a voter operation; only observer RPCs allowed.
pub const MIRROR_HELLO_ERR_OBSERVER_ONLY: u8 = 0x02;
/// Error code: the mirror declared a wire protocol version this source does
/// not implement.
pub const MIRROR_HELLO_ERR_BAD_VERSION: u8 = 0x03;

/// Maximum size (bytes) of a [`MirrorHello`] or [`MirrorHelloAck`] payload.
///
/// Bounds the read buffer so a malicious source cannot force unbounded
/// allocation on the mirror side.
const MAX_HANDSHAKE_PAYLOAD: usize = 4096;

/// Opening handshake sent by the mirror to the source.
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
pub struct MirrorHello {
    /// Cluster-id the mirror is connecting to (i.e., the source cluster-id).
    ///
    /// The source verifies this matches its own id and rejects the connection
    /// on mismatch.
    pub source_cluster: String,
    /// The database id on the *source* cluster being mirrored.
    pub source_database_id: String,
    /// The WAL LSN the mirror last applied.  Drives the source's decision on
    /// whether to start from the last snapshot or stream log from this LSN.
    pub last_applied_lsn: u64,
    /// Wire protocol version for this cross-cluster link.
    pub protocol_version: u16,
}

/// Acknowledgement sent by the source to the mirror.
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
pub struct MirrorHelloAck {
    /// Whether the source accepted the connection.
    pub accepted: bool,
    /// On `accepted = false`, an error code from the `MIRROR_HELLO_ERR_*`
    /// constants.
    pub error_code: u8,
    /// Human-readable explanation (empty string on success).
    pub error_detail: String,
    /// The source's own cluster-id, included so the mirror can verify it
    /// has connected to the right cluster.
    pub source_cluster_id: String,
    /// LSN of the snapshot the source is about to send, or `u64::MAX` if the
    /// source will stream from `last_applied_lsn + 1` without a fresh
    /// snapshot.
    pub snapshot_lsn: u64,
    /// Total snapshot size in bytes (0 if no snapshot will be sent).
    pub snapshot_bytes_total: u64,
}

/// Current cross-cluster wire protocol version.
pub const MIRROR_PROTOCOL_VERSION: u16 = 1;

/// Send a [`MirrorHello`] frame to `writer`.
pub async fn send_hello<W: AsyncWrite + Unpin>(
    writer: &mut W,
    hello: &MirrorHello,
) -> Result<(), MirrorError> {
    let payload = zerompk::to_msgpack_vec(hello).map_err(|e| MirrorError::HandshakeCodec {
        detail: format!("encode MirrorHello: {e}"),
    })?;
    write_framed(writer, MIRROR_HELLO, &payload).await
}

/// Read a [`MirrorHello`] frame from `reader`.
pub async fn recv_hello<R: AsyncRead + Unpin>(reader: &mut R) -> Result<MirrorHello, MirrorError> {
    let (discriminant, payload) = read_framed(reader).await?;
    if discriminant != MIRROR_HELLO {
        return Err(MirrorError::HandshakeCodec {
            detail: format!(
                "expected MirrorHello discriminant {MIRROR_HELLO:#04x}, got {discriminant:#04x}"
            ),
        });
    }
    zerompk::from_msgpack(&payload).map_err(|e| MirrorError::HandshakeCodec {
        detail: format!("decode MirrorHello: {e}"),
    })
}

/// Send a [`MirrorHelloAck`] frame to `writer`.
pub async fn send_ack<W: AsyncWrite + Unpin>(
    writer: &mut W,
    ack: &MirrorHelloAck,
) -> Result<(), MirrorError> {
    let payload = zerompk::to_msgpack_vec(ack).map_err(|e| MirrorError::HandshakeCodec {
        detail: format!("encode MirrorHelloAck: {e}"),
    })?;
    write_framed(writer, MIRROR_HELLO_ACK, &payload).await
}

/// Read a [`MirrorHelloAck`] frame from `reader`.
pub async fn recv_ack<R: AsyncRead + Unpin>(reader: &mut R) -> Result<MirrorHelloAck, MirrorError> {
    let (discriminant, payload) = read_framed(reader).await?;
    if discriminant != MIRROR_HELLO_ACK {
        return Err(MirrorError::HandshakeCodec {
            detail: format!(
                "expected MirrorHelloAck discriminant {MIRROR_HELLO_ACK:#04x}, \
                 got {discriminant:#04x}"
            ),
        });
    }
    zerompk::from_msgpack(&payload).map_err(|e| MirrorError::HandshakeCodec {
        detail: format!("decode MirrorHelloAck: {e}"),
    })
}

/// Write a framed message: `[discriminant u8][len u32 BE][payload bytes]`.
async fn write_framed<W: AsyncWrite + Unpin>(
    writer: &mut W,
    discriminant: u8,
    payload: &[u8],
) -> Result<(), MirrorError> {
    let len = payload.len() as u32;
    let header = [
        discriminant,
        (len >> 24) as u8,
        (len >> 16) as u8,
        (len >> 8) as u8,
        len as u8,
    ];
    writer
        .write_all(&header)
        .await
        .map_err(|e| MirrorError::Transport {
            detail: format!("write framed header: {e}"),
        })?;
    writer
        .write_all(payload)
        .await
        .map_err(|e| MirrorError::Transport {
            detail: format!("write framed payload: {e}"),
        })?;
    Ok(())
}

/// Read a framed message: `[discriminant u8][len u32 BE][payload bytes]`.
async fn read_framed<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(u8, Vec<u8>), MirrorError> {
    let mut header = [0u8; 5];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|e| MirrorError::Transport {
            detail: format!("read framed header: {e}"),
        })?;
    let discriminant = header[0];
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;

    if len > MAX_HANDSHAKE_PAYLOAD {
        return Err(MirrorError::HandshakeCodec {
            detail: format!("handshake payload {len} bytes exceeds max {MAX_HANDSHAKE_PAYLOAD}"),
        });
    }

    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|e| MirrorError::Transport {
            detail: format!("read framed payload: {e}"),
        })?;
    Ok((discriminant, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_roundtrip() {
        let hello = MirrorHello {
            source_cluster: "prod-us".into(),
            source_database_id: "db_01JTEST".into(),
            last_applied_lsn: 12345,
            protocol_version: MIRROR_PROTOCOL_VERSION,
        };
        let mut buf = Vec::<u8>::new();
        send_hello(&mut buf, &hello).await.unwrap();
        let decoded = recv_hello(&mut buf.as_slice()).await.unwrap();
        assert_eq!(decoded, hello);
    }

    #[tokio::test]
    async fn ack_roundtrip() {
        let ack = MirrorHelloAck {
            accepted: true,
            error_code: 0,
            error_detail: String::new(),
            source_cluster_id: "prod-us".into(),
            snapshot_lsn: 42,
            snapshot_bytes_total: 1024 * 1024,
        };
        let mut buf = Vec::<u8>::new();
        send_ack(&mut buf, &ack).await.unwrap();
        let decoded = recv_ack(&mut buf.as_slice()).await.unwrap();
        assert_eq!(decoded, ack);
    }

    #[tokio::test]
    async fn wrong_discriminant_rejected() {
        let ack = MirrorHelloAck {
            accepted: false,
            error_code: MIRROR_HELLO_ERR_CLUSTER_ID,
            error_detail: "bad cluster".into(),
            source_cluster_id: "wrong".into(),
            snapshot_lsn: 0,
            snapshot_bytes_total: 0,
        };
        let mut buf = Vec::<u8>::new();
        // encode as Ack but try to read as Hello
        send_ack(&mut buf, &ack).await.unwrap();
        let err = recv_hello(&mut buf.as_slice()).await.unwrap_err();
        assert!(
            matches!(err, MirrorError::HandshakeCodec { .. }),
            "expected HandshakeCodec, got: {err:?}"
        );
    }
}
