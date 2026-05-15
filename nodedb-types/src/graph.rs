// SPDX-License-Identifier: Apache-2.0

//! Shared graph types used by both Origin and Lite CSR engines.

use serde::{Deserialize, Serialize};

/// Edge traversal direction.
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
pub enum Direction {
    /// Outgoing edges only.
    Out,
    /// Incoming edges only.
    In,
    /// Both directions.
    Both,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Out => "out",
            Self::In => "in",
            Self::Both => "both",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Direction {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "out" | "outgoing" => Ok(Self::Out),
            "in" | "incoming" => Ok(Self::In),
            "both" | "any" => Ok(Self::Both),
            other => Err(format!("unknown direction: '{other}'")),
        }
    }
}

/// Aggregated graph statistics for a single collection.
///
/// Mirrors the row shape returned by `SHOW GRAPH STATS '<collection>'`
/// on Origin and the equivalent direct-engine read on Lite. Values are
/// the global counts after cross-core aggregation; `labels` is sorted
/// ascending by label name.
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
pub struct GraphStats {
    pub collection: String,
    pub node_count: u64,
    pub edge_count: u64,
    pub distinct_label_count: u64,
    pub labels: Vec<(String, u64)>,
}

impl GraphStats {
    pub fn zero(collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            node_count: 0,
            edge_count: 0,
            distinct_label_count: 0,
            labels: Vec::new(),
        }
    }

    /// Parse the wire shape produced by `SHOW GRAPH STATS '<collection>'`
    /// into a typed `GraphStats`. Used by both the native and the pgwire
    /// remote clients — keeping a single parser here is what makes the
    /// `wire_shape:` smoke-probe error messages a meaningful diagnostic
    /// rather than a coincidence between two copies.
    ///
    /// Expected columns: `(collection, node_count, edge_count,
    /// distinct_label_count, labels)`, where `labels` is a JSON array of
    /// `{"label": str, "count": u64}` objects.
    ///
    /// Both wire paths can deliver count cells as `Value::Integer`
    /// (extended-query / native typed) or `Value::String` (pgwire simple
    /// query text protocol); both shapes are accepted.
    ///
    /// Column-shape mismatches surface as errors (this is exactly the
    /// bug class the smoke probe is designed to trap, so no fallback);
    /// empty columns + empty rows return a zero-stats row keyed on the
    /// requested collection (matches Lite when nothing has been written).
    pub fn parse_show_stats_response(
        requested_collection: &str,
        columns: &[String],
        rows: &[Vec<crate::value::Value>],
    ) -> crate::error::NodeDbResult<Self> {
        use crate::error::NodeDbError;

        const EXPECTED: [&str; 5] = [
            "collection",
            "node_count",
            "edge_count",
            "distinct_label_count",
            "labels",
        ];

        if columns.len() != EXPECTED.len()
            || columns.iter().zip(EXPECTED.iter()).any(|(a, b)| a != b)
        {
            if !columns.is_empty() {
                return Err(NodeDbError::storage(format!(
                    "wire_shape: SHOW GRAPH STATS returned unexpected columns: {columns:?}"
                )));
            }
            return Ok(Self::zero(requested_collection));
        }

        let row = match rows.first() {
            Some(r) => r,
            None => return Ok(Self::zero(requested_collection)),
        };

        let coll_name = row
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                NodeDbError::storage("wire_shape: SHOW GRAPH STATS: missing collection cell")
            })?
            .to_string();
        let node_count = row.get(1).and_then(parse_u64_cell).ok_or_else(|| {
            NodeDbError::storage("wire_shape: SHOW GRAPH STATS: missing node_count")
        })?;
        let edge_count = row.get(2).and_then(parse_u64_cell).ok_or_else(|| {
            NodeDbError::storage("wire_shape: SHOW GRAPH STATS: missing edge_count")
        })?;
        let distinct_label_count = row.get(3).and_then(parse_u64_cell).ok_or_else(|| {
            NodeDbError::storage("wire_shape: SHOW GRAPH STATS: missing distinct_label_count")
        })?;
        let labels_json = row.get(4).and_then(|v| v.as_str()).unwrap_or("[]");
        let parsed: Vec<sonic_rs::Value> = sonic_rs::from_str(labels_json)
            .map_err(|e| NodeDbError::storage(format!("wire_shape: labels JSON parse: {e}")))?;
        let mut labels: Vec<(String, u64)> = Vec::with_capacity(parsed.len());
        for entry in &parsed {
            use sonic_rs::JsonValueTrait;
            let label = entry
                .get("label")
                .and_then(|v| v.as_str())
                .ok_or_else(|| NodeDbError::storage("wire_shape: labels entry missing 'label'"))?
                .to_string();
            let count = entry
                .get("count")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| NodeDbError::storage("wire_shape: labels entry missing 'count'"))?;
            labels.push((label, count));
        }
        Ok(Self {
            collection: coll_name,
            node_count,
            edge_count,
            distinct_label_count,
            labels,
        })
    }
}

/// Parse a count cell that may arrive typed (`Value::Integer`) or as
/// pgwire text (`Value::String`).
fn parse_u64_cell(v: &crate::value::Value) -> Option<u64> {
    match v {
        crate::value::Value::Integer(i) => Some(*i as u64),
        crate::value::Value::String(s) => s.parse::<u64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_roundtrip() {
        for dir in [Direction::Out, Direction::In, Direction::Both] {
            let s = dir.as_str();
            let parsed: Direction = s.parse().unwrap();
            assert_eq!(dir, parsed);
        }
    }

    #[test]
    fn direction_display() {
        assert_eq!(Direction::Out.to_string(), "out");
    }

    #[test]
    fn graph_stats_zero() {
        let s = GraphStats::zero("my_coll");
        assert_eq!(s.collection, "my_coll");
        assert_eq!(s.node_count, 0);
        assert_eq!(s.edge_count, 0);
        assert_eq!(s.distinct_label_count, 0);
        assert!(s.labels.is_empty());
    }

    #[test]
    fn graph_stats_serde_round_trip() {
        let s = GraphStats {
            collection: "coll".into(),
            node_count: 10,
            edge_count: 5,
            distinct_label_count: 2,
            labels: vec![("KNOWS".into(), 3), ("OWNS".into(), 2)],
        };
        let json = sonic_rs::to_string(&s).unwrap();
        let back: GraphStats = sonic_rs::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn parse_show_stats_well_formed_row() {
        use crate::value::Value;
        let columns = vec![
            "collection".to_string(),
            "node_count".to_string(),
            "edge_count".to_string(),
            "distinct_label_count".to_string(),
            "labels".to_string(),
        ];
        let labels_json = r#"[{"label":"KNOWS","count":3},{"label":"OWNS","count":2}]"#;
        let rows = vec![vec![
            Value::String("social".into()),
            // pgwire simple-query text protocol arrives as strings;
            // the native protocol arrives as integers — both shapes are accepted.
            Value::String("10".into()),
            Value::Integer(5),
            Value::String("2".into()),
            Value::String(labels_json.into()),
        ]];
        let stats = GraphStats::parse_show_stats_response("social", &columns, &rows).unwrap();
        assert_eq!(stats.collection, "social");
        assert_eq!(stats.node_count, 10);
        assert_eq!(stats.edge_count, 5);
        assert_eq!(stats.distinct_label_count, 2);
        assert_eq!(stats.labels, vec![("KNOWS".into(), 3), ("OWNS".into(), 2),]);
    }

    #[test]
    fn parse_show_stats_empty_rows_returns_zero() {
        let columns = vec![
            "collection".to_string(),
            "node_count".to_string(),
            "edge_count".to_string(),
            "distinct_label_count".to_string(),
            "labels".to_string(),
        ];
        let stats = GraphStats::parse_show_stats_response("social", &columns, &[]).unwrap();
        assert_eq!(stats.collection, "social");
        assert_eq!(stats.edge_count, 0);
    }

    #[test]
    fn parse_show_stats_wrong_columns_errors() {
        let columns = vec!["id".to_string(), "count".to_string()];
        let err = GraphStats::parse_show_stats_response("social", &columns, &[]).unwrap_err();
        assert!(
            err.to_string().contains("unexpected columns"),
            "error should mention unexpected columns: {err}"
        );
    }

    #[test]
    fn parse_show_stats_no_columns_no_rows_returns_zero() {
        let stats = GraphStats::parse_show_stats_response("social", &[], &[]).unwrap();
        assert_eq!(stats.collection, "social");
        assert_eq!(stats.edge_count, 0);
    }

    #[test]
    fn graph_stats_msgpack_round_trip() {
        let s = GraphStats {
            collection: "coll".into(),
            node_count: 7,
            edge_count: 3,
            distinct_label_count: 1,
            labels: vec![("FOLLOWS".into(), 3)],
        };
        let bytes = zerompk::to_msgpack_vec(&s).unwrap();
        let back: GraphStats = zerompk::from_msgpack(&bytes).unwrap();
        assert_eq!(back, s);
    }
}
