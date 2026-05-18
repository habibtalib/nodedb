// SPDX-License-Identifier: Apache-2.0

pub mod codec;
pub mod codecs;
pub mod gating;
pub mod pipeline;
pub mod recall;
pub mod sidecar;
pub mod types;

pub use codec::{CodecName, PreparedQuery, RerankCodec};
pub use codecs::{BinaryRerank, Sq8Rerank};
pub use gating::{IndexShape, validate_options};
pub use pipeline::rerank;
pub use recall::recall_scale;
pub use sidecar::CodecSidecar;
pub use types::{Candidate, Ranked, RerankError};
