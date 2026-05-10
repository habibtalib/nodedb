// SPDX-License-Identifier: BUSL-1.1

//! SIEM export: CDC stream for audit_log + auth_events, webhook with HMAC.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::audit::AuditEntry;

/// SIEM export configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiemConfig {
    /// Export destinations: "splunk", "datadog", "webhook".
    #[serde(default)]
    pub destinations: Vec<String>,
    /// Webhook URL for audit events.
    #[serde(default)]
    pub webhook_url: String,
    /// HMAC secret for webhook signature (hex-encoded).
    #[serde(default)]
    pub webhook_hmac_secret: String,
    /// Maximum events to buffer before dropping oldest.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
    /// Per-request timeout (seconds) for the webhook POST.
    #[serde(default = "default_webhook_timeout_secs")]
    pub webhook_timeout_secs: u64,
}

fn default_buffer_size() -> usize {
    10_000
}

fn default_webhook_timeout_secs() -> u64 {
    10
}

impl Default for SiemConfig {
    fn default() -> Self {
        Self {
            destinations: Vec::new(),
            webhook_url: String::new(),
            webhook_hmac_secret: String::new(),
            buffer_size: default_buffer_size(),
            webhook_timeout_secs: default_webhook_timeout_secs(),
        }
    }
}

/// SIEM export adapter: buffers events for CDC streaming and webhook delivery.
pub struct SiemExporter {
    config: SiemConfig,
    /// Shared HTTP client — constructed once at startup and reused across
    /// every webhook flush so we don't rebuild the connection pool and
    /// TLS session cache per call.
    client: Arc<reqwest::Client>,
    /// Buffered audit events for CDC consumers.
    audit_buffer: RwLock<VecDeque<AuditEntry>>,
    /// Buffered auth events for CDC consumers.
    auth_buffer: RwLock<VecDeque<AuditEntry>>,
}

impl SiemExporter {
    pub fn new(config: SiemConfig) -> Self {
        Self::with_client(config, Arc::new(reqwest::Client::new()))
    }

    /// Construct with an existing shared HTTP client.
    pub fn with_client(config: SiemConfig, client: Arc<reqwest::Client>) -> Self {
        let cap = config.buffer_size;
        Self {
            config,
            client,
            audit_buffer: RwLock::new(VecDeque::with_capacity(cap.min(10_000))),
            auth_buffer: RwLock::new(VecDeque::with_capacity(cap.min(10_000))),
        }
    }

    /// Push an audit event to the export buffer.
    pub fn push_audit(&self, entry: AuditEntry) {
        let mut buf = self.audit_buffer.write().unwrap_or_else(|p| p.into_inner());
        if buf.len() >= self.config.buffer_size {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    /// Push an auth event to the export buffer.
    pub fn push_auth(&self, entry: AuditEntry) {
        let mut buf = self.auth_buffer.write().unwrap_or_else(|p| p.into_inner());
        if buf.len() >= self.config.buffer_size {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    /// Drain audit events for CDC consumption (Splunk, Datadog, etc.).
    pub fn drain_audit(&self) -> Vec<AuditEntry> {
        let mut buf = self.audit_buffer.write().unwrap_or_else(|p| p.into_inner());
        buf.drain(..).collect()
    }

    /// Drain auth events for CDC consumption.
    pub fn drain_auth(&self) -> Vec<AuditEntry> {
        let mut buf = self.auth_buffer.write().unwrap_or_else(|p| p.into_inner());
        buf.drain(..).collect()
    }

    /// Build a webhook payload with HMAC signature.
    ///
    /// Returns `(json_body, hmac_signature_hex)`.
    pub fn build_webhook_payload(&self, events: &[AuditEntry]) -> (String, String) {
        let body = serde_json::json!({
            "source": "nodedb",
            "event_count": events.len(),
            "events": events,
        })
        .to_string();

        let signature = if !self.config.webhook_hmac_secret.is_empty() {
            compute_hmac(&self.config.webhook_hmac_secret, &body)
        } else {
            String::new()
        };

        (body, signature)
    }

    /// Send buffered events to configured webhook (async).
    pub async fn flush_webhook(&self) {
        if self.config.webhook_url.is_empty() {
            return;
        }

        let audit_events = self.drain_audit();
        let auth_events = self.drain_auth();
        let all: Vec<AuditEntry> = audit_events.into_iter().chain(auth_events).collect();

        if all.is_empty() {
            return;
        }

        let (body, signature) = self.build_webhook_payload(&all);

        let mut req = self
            .client
            .post(&self.config.webhook_url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(
                self.config.webhook_timeout_secs,
            ))
            .body(body);

        if !signature.is_empty() {
            req = req.header("X-NodeDB-Signature", &signature);
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                info!(events = all.len(), "SIEM webhook delivered");
            }
            Ok(resp) => {
                warn!(status = %resp.status(), "SIEM webhook delivery failed");
            }
            Err(e) => {
                warn!(error = %e, "SIEM webhook request failed");
            }
        }
    }

    /// Whether any export destinations are configured.
    pub fn is_configured(&self) -> bool {
        !self.config.destinations.is_empty() || !self.config.webhook_url.is_empty()
    }

    /// Number of buffered events (audit + auth).
    pub fn buffered_count(&self) -> usize {
        let a = self
            .audit_buffer
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .len();
        let b = self
            .auth_buffer
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .len();
        a + b
    }
}

impl Default for SiemExporter {
    fn default() -> Self {
        Self::new(SiemConfig::default())
    }
}

/// Compute HMAC-SHA256 signature for webhook payload.
fn compute_hmac(secret: &str, message: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(message.as_bytes());
    let result = mac.finalize();
    result
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry() -> AuditEntry {
        AuditEntry {
            seq: 1,
            timestamp_us: 0,
            event: super::super::audit::AuditEvent::AuthSuccess,
            tenant_id: None,
            database_id: None,
            auth_user_id: "u1".into(),
            auth_user_name: "alice".into(),
            session_id: "s1".into(),
            source: "10.0.0.1".into(),
            detail: "test".into(),
            prev_hash: String::new(),
        }
    }

    #[test]
    fn buffer_and_drain() {
        let exporter = SiemExporter::default();
        exporter.push_audit(test_entry());
        exporter.push_audit(test_entry());
        exporter.push_auth(test_entry());

        assert_eq!(exporter.buffered_count(), 3);

        let audit = exporter.drain_audit();
        assert_eq!(audit.len(), 2);
        assert_eq!(exporter.buffered_count(), 1); // auth still buffered.
    }

    #[test]
    fn webhook_payload_with_hmac() {
        let config = SiemConfig {
            webhook_hmac_secret: "test_secret".into(),
            ..Default::default()
        };
        let exporter = SiemExporter::new(config);

        let (body, signature) = exporter.build_webhook_payload(&[test_entry()]);
        assert!(body.contains("nodedb"));
        assert!(!signature.is_empty());
        assert_eq!(signature.len(), 64); // SHA-256 hex = 64 chars.
    }

    #[test]
    fn hmac_consistency() {
        let sig1 = compute_hmac("secret", "hello");
        let sig2 = compute_hmac("secret", "hello");
        assert_eq!(sig1, sig2);

        let sig3 = compute_hmac("secret", "world");
        assert_ne!(sig1, sig3);
    }
}
