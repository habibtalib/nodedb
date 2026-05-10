// SPDX-License-Identifier: Apache-2.0

//! Fluent builder for `NativeClient` connections.
//!
//! ```rust,ignore
//! let client = ConnectionBuilder::new("127.0.0.1:6433")
//!     .username("alice")
//!     .password("s3cr3t")
//!     .database("analytics")
//!     .max_connections(20)
//!     .build();
//! ```

use std::time::Duration;

use nodedb_types::protocol::AuthMethod;

use super::client::NativeClient;
use super::connection::TlsConfig;
use super::pool::PoolConfig;

/// Fluent builder for a [`NativeClient`] connection.
///
/// Call [`build`](Self::build) to construct the client once all options
/// have been set.
#[derive(Debug, Default)]
pub struct ConnectionBuilder {
    addr: Option<String>,
    username: Option<String>,
    password: Option<String>,
    api_key: Option<String>,
    database: Option<String>,
    max_connections: Option<usize>,
    connect_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
    tls: Option<TlsConfig>,
}

impl ConnectionBuilder {
    /// Start building a connection to `addr` (e.g. `"127.0.0.1:6433"`).
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: Some(addr.into()),
            ..Default::default()
        }
    }

    /// Set the username for trust or password authentication.
    pub fn username(mut self, username: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self
    }

    /// Set the password (enables SCRAM-SHA-256 / cleartext authentication).
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Set an API key token (enables API key authentication).
    pub fn api_key(mut self, token: impl Into<String>) -> Self {
        self.api_key = Some(token.into());
        self
    }

    /// Set the target database name.
    ///
    /// The database name is sent in the auth handshake frame so every
    /// connection in the pool executes within this database context.
    /// Equivalent to `psql -d <name>` for the native protocol.
    pub fn database(mut self, name: impl Into<String>) -> Self {
        self.database = Some(name.into());
        self
    }

    /// Set the maximum number of pooled connections (default: 10).
    pub fn max_connections(mut self, n: usize) -> Self {
        self.max_connections = Some(n);
        self
    }

    /// Set the connection timeout (default: 5 seconds).
    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = Some(d);
        self
    }

    /// Set the idle connection timeout (default: 5 minutes).
    pub fn idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = Some(d);
        self
    }

    /// Configure TLS.
    pub fn tls(mut self, tls: TlsConfig) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Build the `NativeClient`.
    ///
    /// Falls back to sensible defaults for any unset option.
    pub fn build(self) -> NativeClient {
        let addr = self.addr.unwrap_or_else(|| "127.0.0.1:6433".to_string());

        let auth = if let Some(token) = self.api_key {
            AuthMethod::ApiKey { token }
        } else if let Some(password) = self.password {
            let username = self.username.unwrap_or_else(|| "admin".to_string());
            AuthMethod::Password { username, password }
        } else {
            let username = self.username.unwrap_or_else(|| "admin".to_string());
            AuthMethod::Trust { username }
        };

        let default_config = PoolConfig::default();

        let config = PoolConfig {
            addr,
            auth,
            database: self.database,
            max_size: self.max_connections.unwrap_or(default_config.max_size),
            connect_timeout: self
                .connect_timeout
                .unwrap_or(default_config.connect_timeout),
            idle_timeout: self.idle_timeout.unwrap_or(default_config.idle_timeout),
            tls: self.tls.unwrap_or_default(),
        };

        NativeClient::new(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults() {
        let client = ConnectionBuilder::new("127.0.0.1:6433").build();
        let _ = client; // just verify it compiles
    }

    #[test]
    fn builder_with_database() {
        // Smoke test: verify the builder accepts a database name without panic.
        let _client = ConnectionBuilder::new("127.0.0.1:6433")
            .username("alice")
            .database("analytics")
            .build();
    }

    #[test]
    fn builder_password_auth() {
        let _client = ConnectionBuilder::new("127.0.0.1:6433")
            .username("bob")
            .password("secret")
            .database("prod")
            .build();
    }
}
