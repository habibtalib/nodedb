// SPDX-License-Identifier: BUSL-1.1

/// Returns the server version string in the canonical `"NodeDB <semver>"`
/// form, sourced from `CARGO_PKG_VERSION` at compile time.
///
/// Used by the pgwire startup parameter, `SHOW server_version`, and the
/// RESP `INFO` command so all wire surfaces report the same value as the
/// binary that was built.
pub fn server_version_string() -> String {
    format!("NodeDB {}", env!("CARGO_PKG_VERSION"))
}

pub mod admission;
pub mod broadcast;
pub mod conn_stream;
pub mod dispatch_utils;
pub mod graph_dispatch;
pub mod http;
pub mod ilp_listener;
pub mod listener;
pub mod native;
pub mod pgwire;
pub mod post_aggregate;
pub mod resp;
pub mod response_translate;
pub mod session;
pub mod session_auth;
pub mod sync;
pub mod tls_reload;
pub mod wal_dispatch;
