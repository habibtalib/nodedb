//! Observability configuration: PromQL, OTLP receiver, OTLP export.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// Top-level observability configuration.
///
/// ```toml
/// [observability.promql]
/// enabled = true
///
/// [observability.otlp.receiver]
/// enabled = true
/// http_listen = "0.0.0.0:4318"
/// grpc_listen = "0.0.0.0:4317"
///
/// [observability.otlp.export]
/// enabled = false
/// endpoint = "http://localhost:4318"
/// metrics_interval_secs = 15
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// PromQL engine and `/obsv/api/v1/*` endpoints.
    #[serde(default)]
    pub promql: PromqlConfig,

    /// OpenTelemetry Protocol configuration.
    #[serde(default)]
    pub otlp: OtlpConfig,

    /// Master gate for the `/cluster/debug/*` HTTP endpoints. Disabled
    /// by default — operators must opt in per deployment because the
    /// endpoints expose raft internals, transport connection caches,
    /// descriptor leases, and the full metadata cache. Even when
    /// enabled the handlers still require a superuser identity. (J.5)
    #[serde(default)]
    pub debug_endpoints_enabled: bool,
}

/// PromQL engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromqlConfig {
    /// Enable PromQL endpoints (`/obsv/api/v1/*`).
    /// Requires the `promql` cargo feature at compile time.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for PromqlConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// OpenTelemetry Protocol configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OtlpConfig {
    /// OTLP receiver (ingest from external services).
    #[serde(default)]
    pub receiver: OtlpReceiverConfig,

    /// OTLP export (push NodeDB's own telemetry to a collector).
    #[serde(default)]
    pub export: OtlpExportConfig,
}

/// OTLP receiver configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtlpReceiverConfig {
    /// Enable OTLP ingest endpoints.
    /// Requires the `otel` cargo feature at compile time.
    #[serde(default)]
    pub enabled: bool,

    /// OTLP/HTTP listen address (default: 0.0.0.0:4318).
    #[serde(default = "default_otlp_http_listen")]
    pub http_listen: SocketAddr,

    /// OTLP/gRPC listen address (default: 0.0.0.0:4317).
    #[serde(default = "default_otlp_grpc_listen")]
    pub grpc_listen: SocketAddr,
}

impl Default for OtlpReceiverConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http_listen: default_otlp_http_listen(),
            grpc_listen: default_otlp_grpc_listen(),
        }
    }
}

/// OTLP export configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtlpExportConfig {
    /// Enable OTLP export of NodeDB's own metrics/traces.
    #[serde(default)]
    pub enabled: bool,

    /// OTLP collector endpoint (e.g., "http://localhost:4318").
    #[serde(default)]
    pub endpoint: String,

    /// Metrics push interval in seconds (default: 15).
    #[serde(default = "default_metrics_interval")]
    pub metrics_interval_secs: u64,
}

impl Default for OtlpExportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            metrics_interval_secs: 15,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_otlp_http_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 4318))
}

/// Validate that enabled features are available at compile time.
///
/// Call after loading config + env overrides. Returns an error message
/// if the user enabled a feature that wasn't compiled in.
pub fn validate_feature_availability(config: &ObservabilityConfig) -> crate::Result<()> {
    if config.promql.enabled {
        #[cfg(not(feature = "promql"))]
        return Err(crate::Error::Config {
            detail: "observability.promql.enabled = true, but this binary was built without \
                     the `promql` feature. Rebuild with `--features promql` or \
                     `--features monitoring`, or set enabled = false."
                .to_string(),
        });
    }

    if config.otlp.receiver.enabled {
        #[cfg(not(feature = "otel"))]
        return Err(crate::Error::Config {
            detail:
                "observability.otlp.receiver.enabled = true, but this binary was built without \
                     the `otel` feature. Rebuild with `--features otel` or \
                     `--features monitoring`, or set enabled = false."
                    .to_string(),
        });
    }

    if config.otlp.export.enabled {
        #[cfg(not(feature = "otel"))]
        return Err(crate::Error::Config {
            detail: "observability.otlp.export.enabled = true, but this binary was built without \
                     the `otel` feature. Rebuild with `--features otel` or \
                     `--features monitoring`, or set enabled = false."
                .to_string(),
        });
    }

    Ok(())
}

fn default_otlp_grpc_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 4317))
}

fn default_metrics_interval() -> u64 {
    15
}

/// Apply observability-related environment variable overrides.
///
/// Variables:
/// - `NODEDB_PROMQL_ENABLED`         — "true"/"false"
/// - `NODEDB_OTLP_RECEIVER_ENABLED`  — "true"/"false"
/// - `NODEDB_OTLP_HTTP_LISTEN`       — SocketAddr
/// - `NODEDB_OTLP_GRPC_LISTEN`       — SocketAddr
/// - `NODEDB_OTLP_EXPORT_ENABLED`    — "true"/"false"
/// - `NODEDB_OTLP_EXPORT_ENDPOINT`   — URL string
/// - `NODEDB_OTLP_EXPORT_INTERVAL`   — seconds (u64)
pub fn apply_observability_env(config: &mut ObservabilityConfig) {
    if let Ok(val) = std::env::var("NODEDB_PROMQL_ENABLED")
        && let Ok(b) = val.parse::<bool>()
    {
        tracing::info!(
            env_var = "NODEDB_PROMQL_ENABLED",
            value = b,
            "override applied"
        );
        config.promql.enabled = b;
    }

    if let Ok(val) = std::env::var("NODEDB_OTLP_RECEIVER_ENABLED")
        && let Ok(b) = val.parse::<bool>()
    {
        tracing::info!(
            env_var = "NODEDB_OTLP_RECEIVER_ENABLED",
            value = b,
            "override applied"
        );
        config.otlp.receiver.enabled = b;
    }

    if let Ok(val) = std::env::var("NODEDB_OTLP_HTTP_LISTEN")
        && let Ok(addr) = val.parse::<SocketAddr>()
    {
        tracing::info!(env_var = "NODEDB_OTLP_HTTP_LISTEN", value = %val, "override applied");
        config.otlp.receiver.http_listen = addr;
    }

    if let Ok(val) = std::env::var("NODEDB_OTLP_GRPC_LISTEN")
        && let Ok(addr) = val.parse::<SocketAddr>()
    {
        tracing::info!(env_var = "NODEDB_OTLP_GRPC_LISTEN", value = %val, "override applied");
        config.otlp.receiver.grpc_listen = addr;
    }

    if let Ok(val) = std::env::var("NODEDB_OTLP_EXPORT_ENABLED")
        && let Ok(b) = val.parse::<bool>()
    {
        tracing::info!(
            env_var = "NODEDB_OTLP_EXPORT_ENABLED",
            value = b,
            "override applied"
        );
        config.otlp.export.enabled = b;
    }

    if let Ok(val) = std::env::var("NODEDB_OTLP_EXPORT_ENDPOINT") {
        tracing::info!(env_var = "NODEDB_OTLP_EXPORT_ENDPOINT", value = %val, "override applied");
        config.otlp.export.endpoint = val;
    }

    if let Ok(val) = std::env::var("NODEDB_OTLP_EXPORT_INTERVAL")
        && let Ok(secs) = val.parse::<u64>()
    {
        tracing::info!(
            env_var = "NODEDB_OTLP_EXPORT_INTERVAL",
            value = secs,
            "override applied"
        );
        config.otlp.export.metrics_interval_secs = secs;
    }

    if let Ok(val) = std::env::var("NODEDB_DEBUG_ENDPOINTS_ENABLED")
        && let Ok(b) = val.parse::<bool>()
    {
        tracing::info!(
            env_var = "NODEDB_DEBUG_ENDPOINTS_ENABLED",
            value = b,
            "override applied"
        );
        config.debug_endpoints_enabled = b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = ObservabilityConfig::default();
        assert!(cfg.promql.enabled);
        assert!(!cfg.otlp.receiver.enabled);
        assert!(!cfg.otlp.export.enabled);
        assert_eq!(cfg.otlp.receiver.http_listen.port(), 4318);
        assert_eq!(cfg.otlp.receiver.grpc_listen.port(), 4317);
        assert_eq!(cfg.otlp.export.metrics_interval_secs, 15);
    }

    #[test]
    fn validate_default_config() {
        // Default config has promql.enabled = true.
        // With the promql feature compiled in, this should pass.
        // Without it, this should fail — but that case is only testable
        // when building without the feature.
        let cfg = ObservabilityConfig::default();
        let result = validate_feature_availability(&cfg);
        #[cfg(feature = "promql")]
        assert!(result.is_ok());
        #[cfg(not(feature = "promql"))]
        assert!(result.is_err());
    }

    #[test]
    fn validate_disabled_always_passes() {
        let mut cfg = ObservabilityConfig::default();
        cfg.promql.enabled = false;
        cfg.otlp.receiver.enabled = false;
        cfg.otlp.export.enabled = false;
        assert!(validate_feature_availability(&cfg).is_ok());
    }

    #[test]
    fn toml_roundtrip() {
        let cfg = ObservabilityConfig::default();
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let _parsed: ObservabilityConfig = toml::from_str(&toml_str).unwrap();
    }
}
