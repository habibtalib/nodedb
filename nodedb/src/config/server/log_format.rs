// SPDX-License-Identifier: BUSL-1.1

//! Log output format selection (text vs JSON).

use serde::{Deserialize, Serialize};

/// Log output format selection.
///
/// Serializes as lowercase strings `"text"` and `"json"`. Any other value is
/// rejected by serde at deserialization time — there is no silent fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable, coloured output (default).
    #[default]
    Text,
    /// Structured JSON lines, suitable for log aggregators.
    Json,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_text() {
        assert_eq!(LogFormat::default(), LogFormat::Text);
    }

    #[test]
    fn parses_text() {
        let v: LogFormat = toml::from_str("v = \"text\"\n")
            .map(|t: toml::Table| t["v"].clone().try_into().unwrap())
            .unwrap();
        assert_eq!(v, LogFormat::Text);
    }

    #[test]
    fn parses_json() {
        let v: LogFormat = toml::from_str("v = \"json\"\n")
            .map(|t: toml::Table| t["v"].clone().try_into().unwrap())
            .unwrap();
        assert_eq!(v, LogFormat::Json);
    }

    #[test]
    fn unknown_rejected() {
        let result: Result<LogFormat, _> = toml::Value::String("yaml".into()).try_into();
        assert!(result.is_err());
    }
}
