// SPDX-License-Identifier: BUSL-1.1

//! Tracing subscriber initialisation (format + filter).

use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::ServerConfig;

/// Initialise the global tracing subscriber based on the config log format.
///
/// Uses `RUST_LOG` env var for the filter if set; otherwise defaults to `warn`
/// for a clean startup. Must be called after config is loaded and before any
/// `tracing::info!` / `tracing::warn!` calls are expected to emit.
pub fn init_tracing(config: &ServerConfig) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    if config.server.log_format == crate::config::LogFormat::Json {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .json()
                    .flatten_event(true)
                    .with_filter(filter),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(filter),
            )
            .init();
    }
}
