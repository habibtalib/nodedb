// SPDX-License-Identifier: Apache-2.0

/// A pre-rerank candidate returned by the index-level search (HNSW/Vamana/...).
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: u32,
    pub index_distance: f32,
}

/// Final rerank output.
#[derive(Debug, Clone)]
pub struct Ranked {
    pub id: u32,
    pub distance: f32,
}

#[derive(thiserror::Error, Debug)]
pub enum RerankError {
    #[error("bad input: {0}")]
    BadInput(String),
    #[error("codec not trained: {0}")]
    NotTrained(String),
    #[error("codec training failed: {0}")]
    Train(String),
    // Additional variants added in later sub-tasks (CodecMissing, CodecMismatch, ...).
}
