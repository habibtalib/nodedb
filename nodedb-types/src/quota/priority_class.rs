// SPDX-License-Identifier: Apache-2.0

//! Per-database WAL group-commit priority class.
//!
//! Three classes map to three independent WAL fsync groups. A database at
//! `Critical` commits in its own group ahead of all others; `Bulk` is batched
//! with an extended timeout and lower fsync rate.  `Standard` is the default
//! for all newly created databases.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Priority class for WAL group-commit scheduling and weighted-fair SPSC dispatch.
///
/// Set per database via `ALTER DATABASE … SET QUOTA (priority_class = '...')`.
/// Determines which of the three WAL fsync groups a database's writes join and
/// what deficit weight it receives in the weighted-fair bridge queue.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub enum PriorityClass {
    /// Dedicated WAL fsync group; committed before `Standard` and `Bulk`.
    /// Highest dispatch weight in the weighted-fair bridge queue.
    Critical,
    /// Default. Batched normally; receives unit dispatch weight.
    #[default]
    Standard,
    /// Extended fsync timeout; lowest dispatch weight. Suited for batch imports
    /// and background analytics that can tolerate higher commit latency.
    Bulk,
}

impl fmt::Display for PriorityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PriorityClass::Critical => f.write_str("critical"),
            PriorityClass::Standard => f.write_str("standard"),
            PriorityClass::Bulk => f.write_str("bulk"),
        }
    }
}

/// Error returned when a string cannot be parsed as a [`PriorityClass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityClassParseError {
    pub input: String,
}

impl fmt::Display for PriorityClassParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown priority class '{}'; valid values are: critical, standard, bulk",
            self.input
        )
    }
}

impl std::error::Error for PriorityClassParseError {}

impl FromStr for PriorityClass {
    type Err = PriorityClassParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "critical" => Ok(PriorityClass::Critical),
            "standard" => Ok(PriorityClass::Standard),
            "bulk" => Ok(PriorityClass::Bulk),
            other => Err(PriorityClassParseError {
                input: other.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_standard() {
        assert_eq!(PriorityClass::default(), PriorityClass::Standard);
    }

    #[test]
    fn display_round_trips() {
        for cls in [
            PriorityClass::Critical,
            PriorityClass::Standard,
            PriorityClass::Bulk,
        ] {
            let s = cls.to_string();
            let parsed: PriorityClass = s.parse().unwrap();
            assert_eq!(parsed, cls);
        }
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            "CRITICAL".parse::<PriorityClass>().unwrap(),
            PriorityClass::Critical
        );
        assert_eq!(
            "Bulk".parse::<PriorityClass>().unwrap(),
            PriorityClass::Bulk
        );
    }

    #[test]
    fn unknown_returns_err() {
        let err = "ultrafast".parse::<PriorityClass>().unwrap_err();
        assert!(err.to_string().contains("ultrafast"));
    }
}
