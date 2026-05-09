// SPDX-License-Identifier: BUSL-1.1

//! Cross-cluster mirror transport: QUIC link, cluster-id handshake,
//! send-rate throttle, and snapshot bootstrap.
//!
//! # Module layout
//!
//! - [`error`] — [`MirrorError`] enum; wires into [`crate::error::ClusterError`].
//! - [`handshake`] — [`MirrorHello`] / [`MirrorHelloAck`] wire types and codec.
//! - [`link`] — [`CrossClusterLink`]: outbound QUIC connection with exponential
//!   backoff reconnect and cluster-id authentication.
//! - [`throttle`] — [`SendThrottle`]: per-mirror bytes-in-flight cap; source
//!   pauses when the observer falls behind.
//! - [`bootstrap`] — [`MirrorBootstrapReceiver`]: cross-cluster snapshot transfer,
//!   `Bootstrapping → Following` status transitions.
//! - [`source_handler`] — [`handle_mirror_connection`]: source-side validation of
//!   incoming mirror connections (cluster-id check, observer-only enforcement).

pub mod bootstrap;
pub mod error;
pub mod handshake;
pub mod link;
pub mod source_handler;
pub mod throttle;

pub use bootstrap::{
    BootstrapChunkOutcome, CrossClusterSnapshotEnvelope, MirrorBootstrapReceiver,
    PROGRESS_REPORT_CHUNK_BYTES,
};
pub use error::MirrorError;
pub use handshake::{
    MIRROR_HELLO_ERR_BAD_VERSION, MIRROR_HELLO_ERR_CLUSTER_ID, MIRROR_HELLO_ERR_OBSERVER_ONLY,
    MIRROR_PROTOCOL_VERSION, MirrorHello, MirrorHelloAck,
};
pub use link::CrossClusterLink;
pub use source_handler::{HandshakeOutcome, SourceHandlerParams, handle_mirror_connection};
pub use throttle::{DEFAULT_CAP_BYTES, SendThrottle};
