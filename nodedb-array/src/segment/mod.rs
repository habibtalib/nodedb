// SPDX-License-Identifier: BUSL-1.1

pub(crate) mod encrypt;
pub mod format;
pub mod mbr_index;
pub mod reader;
pub mod writer;

pub use format::{
    FOOTER_MAGIC, FORMAT_VERSION, HEADER_MAGIC, SegmentFooter, SegmentHeader, TileEntry, TileKind,
};
pub use mbr_index::{HilbertPackedRTree, MbrQueryPredicate};
pub use reader::{SegmentReader, TilePayload, extract_cell_bytes};
pub use writer::SegmentWriter;
