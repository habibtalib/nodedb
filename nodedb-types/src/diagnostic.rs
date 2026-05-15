// SPDX-License-Identifier: Apache-2.0

//! Canonical diagnostic-layer taxonomy for end-to-end probes.
//!
//! Smoke probes and QA harnesses assert against these names so a layer
//! mismatch is unambiguous regardless of which engine emitted it.

use serde::{Deserialize, Serialize};

/// Stable layer identifier surfaced in error spans, structured logs, and
/// probe diagnostic reports. New variants are additive — readers must
/// not rely on the variant set being closed.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
#[msgpack(c_enum)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLayer {
    /// The persistent edge store (per `(tenant, collection)`).
    EdgeStore,
    /// The in-memory CSR adjacency partitions on Data Plane cores.
    Csr,
    /// The serialization shape returned to clients (the bug class from
    /// the original graph-traverse wire-shape investigation).
    WireShape,
    /// The end-to-end write path from client through WAL to commit.
    WritePath,
    /// Cross-node replication / Raft log application.
    Replication,
}

impl DiagnosticLayer {
    /// Stable snake_case name suitable for logs, span tags, and JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EdgeStore => "edge_store",
            Self::Csr => "csr",
            Self::WireShape => "wire_shape",
            Self::WritePath => "write_path",
            Self::Replication => "replication",
        }
    }
}

impl std::fmt::Display for DiagnosticLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_names_are_stable() {
        assert_eq!(DiagnosticLayer::EdgeStore.as_str(), "edge_store");
        assert_eq!(DiagnosticLayer::Csr.as_str(), "csr");
        assert_eq!(DiagnosticLayer::WireShape.as_str(), "wire_shape");
        assert_eq!(DiagnosticLayer::WritePath.as_str(), "write_path");
        assert_eq!(DiagnosticLayer::Replication.as_str(), "replication");
    }

    #[test]
    fn display_matches_as_str() {
        for v in [
            DiagnosticLayer::EdgeStore,
            DiagnosticLayer::Csr,
            DiagnosticLayer::WireShape,
            DiagnosticLayer::WritePath,
            DiagnosticLayer::Replication,
        ] {
            assert_eq!(format!("{v}"), v.as_str());
        }
    }

    #[test]
    fn serde_round_trip_snake_case() {
        let v = DiagnosticLayer::EdgeStore;
        let s = sonic_rs::to_string(&v).unwrap();
        assert_eq!(s, "\"edge_store\"");
        let back: DiagnosticLayer = sonic_rs::from_str(&s).unwrap();
        assert_eq!(back, v);
    }
}
