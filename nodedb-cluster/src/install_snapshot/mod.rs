//! Chunked `InstallSnapshot` transport — leader-side sender and follower-side receiver.
//!
//! # Module layout
//!
//! - [`sender`] — leader chunked send loop; slices snapshot bytes into framed
//!   RPC chunks and emits one `InstallSnapshotRequest` per chunk.
//! - [`receiver`] — follower `PartialSnapshotState` accumulator; writes chunk
//!   bytes to `<data_dir>/recv_snapshots/<group_id>.partial` and validates
//!   the running CRC.
//! - [`finalize`] — atomic rename + CRC-full validation + Raft log boundary
//!   advance.
//! - [`gc`] — orphan `.partial` file sweeper; removes stale partials that
//!   predate `orphan_partial_max_age_secs`.

pub mod finalize;
pub mod gc;
pub mod receiver;
pub mod sender;
pub mod state;

pub use gc::sweep_orphans;
pub use receiver::{ChunkOutcome, PartialSnapshotMap, handle_chunk};
pub use sender::{SendChunkedParams, send_chunked};
pub use state::PartialSnapshotState;
