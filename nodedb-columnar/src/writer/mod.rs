//! Segment writer: drains a memtable into a compressed columnar segment.

mod block;
mod encode;
mod segment_writer;
mod stats;

pub use segment_writer::{
    PROFILE_PLAIN, PROFILE_SPATIAL, PROFILE_TIMESERIES, SegmentWriter, select_codec_for_profile,
};
