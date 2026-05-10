// SPDX-License-Identifier: Apache-2.0

//! Authentication method types.

use serde::{Deserialize, Serialize};

/// Authentication method in an `Auth` request.
#[derive(
    Debug, Clone, Serialize, Deserialize, zerompk::ToMessagePack, zerompk::FromMessagePack,
)]
#[serde(tag = "method", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthMethod {
    #[serde(rename = "trust")]
    Trust {
        #[serde(default = "default_username")]
        username: String,
    },
    #[serde(rename = "password")]
    Password { username: String, password: String },
    #[serde(rename = "api_key")]
    ApiKey { token: String },
    /// OIDC bearer token (native / HTTP clients only; NOT pgwire).
    #[serde(rename = "oidc_bearer")]
    OidcBearer {
        token: String,
        /// Optional provider name hint. When absent the provider is resolved by
        /// the `iss` claim in the token.
        #[serde(default)]
        provider: Option<String>,
    },
}

fn default_username() -> String {
    "admin".into()
}

/// Successful auth response payload.
#[derive(
    Debug, Clone, Serialize, Deserialize, zerompk::ToMessagePack, zerompk::FromMessagePack,
)]
pub struct AuthResponse {
    pub username: String,
    pub tenant_id: u64,
}
