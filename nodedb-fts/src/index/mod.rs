pub mod analyzer_config;
pub mod error;
pub mod fieldnorm;
pub mod stats;
pub mod synonym_groups;
pub mod writer;

pub use error::{FtsIndexError, MAX_INDEXABLE_SURROGATE};
pub use synonym_groups::SynonymGroupRecord;
pub use writer::FtsIndex;
