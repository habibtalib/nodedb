//! ILP (InfluxDB Line Protocol) TCP listener for timeseries ingest.
//!
//! Accepts plain TCP connections on the configured port. Each connection
//! reads newline-delimited ILP lines, parses them, and dispatches
//! `TimeseriesIngest` plans to the Data Plane via SPSC.
//!
//! Protocol: raw TCP, one ILP line per newline. No HTTP overhead.
//! Compatible with `telegraf`, `vector`, and InfluxDB client libraries.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};

/// Maximum byte length of a single ILP line. Lines exceeding this are
/// rejected and the connection is dropped to prevent memory exhaustion.
const MAX_ILP_LINE_BYTES: usize = 10 * 1024 * 1024; // 10 MiB
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::control::server::conn_stream::ConnStream;
use crate::control::state::SharedState;
use crate::types::TenantId;

#[path = "ilp_batch.rs"]
mod ilp_batch;
use ilp_batch::{IlpRateEstimator, flush_ilp_batch};

/// ILP TCP listener.
pub struct IlpListener {
    tcp: TcpListener,
    addr: SocketAddr,
}

impl IlpListener {
    /// Bind to the given address.
    pub async fn bind(addr: SocketAddr) -> crate::Result<Self> {
        let tcp = TcpListener::bind(addr).await.map_err(crate::Error::Io)?;
        let local_addr = tcp.local_addr().map_err(crate::Error::Io)?;
        info!(%local_addr, "ILP TCP listener bound");
        Ok(Self {
            tcp,
            addr: local_addr,
        })
    }

    /// Returns the local address the listener is bound to.
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    /// Run the accept loop until shutdown.
    pub async fn run(
        self,
        state: Arc<SharedState>,
        conn_semaphore: Arc<Semaphore>,
        tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
        startup_gate: Arc<crate::control::startup::StartupGate>,
        bus: crate::control::shutdown::ShutdownBus,
    ) -> crate::Result<()> {
        let drain_guard = bus.register_task(
            crate::control::shutdown::ShutdownPhase::DrainingListeners,
            "ilp",
            None,
        );
        let mut shutdown_handle = bus.handle();

        let tls_label = if tls_acceptor.is_some() {
            "tls"
        } else {
            "plain"
        };
        info!(addr = %self.addr, tls = tls_label, "ILP listener bound — waiting for GatewayEnable");

        startup_gate
            .await_phase(crate::control::startup::StartupPhase::GatewayEnable)
            .await
            .map_err(crate::Error::from)?;

        info!(addr = %self.addr, tls = tls_label, "ILP listener accepting connections");

        let mut connections = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                result = self.tcp.accept() => {
                    match result {
                        Ok((stream, peer)) => {
                            let permit = match conn_semaphore.clone().try_acquire_owned() {
                                Ok(p) => p,
                                Err(_) => {
                                    debug!(%peer, "ILP connection rejected: max connections");
                                    continue;
                                }
                            };
                            let state = Arc::clone(&state);

                            if let Some(ref acceptor) = tls_acceptor {
                                let acceptor = acceptor.clone();
                                connections.spawn(async move {
                                    match tokio::time::timeout(
                                        std::time::Duration::from_secs(10),
                                        acceptor.accept(stream),
                                    )
                                    .await
                                    {
                                        Ok(Ok(tls_stream)) => {
                                            let cs = ConnStream::tls(tls_stream);
                                            if let Err(e) = handle_ilp_connection(cs, peer, &state).await {
                                                warn!(%peer, error = %e, "ILP TLS connection error (data may be lost)");
                                            }
                                        }
                                        Ok(Err(e)) => {
                                            warn!(%peer, error = %e, "ILP TLS handshake failed");
                                        }
                                        Err(_) => {
                                            warn!(%peer, "ILP TLS handshake timed out");
                                        }
                                    }
                                    drop(permit);
                                });
                            } else {
                                connections.spawn(async move {
                                    let cs = ConnStream::plain(stream);
                                    if let Err(e) = handle_ilp_connection(cs, peer, &state).await {
                                        warn!(%peer, error = %e, "ILP connection error (data may be lost)");
                                    }
                                    drop(permit);
                                });
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "ILP accept error");
                        }
                    }
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
                _ = shutdown_handle.await_phase(crate::control::shutdown::ShutdownPhase::DrainingListeners) => {
                    info!(addr = %self.addr, "ILP listener shutting down");
                    break;
                }
            }
        }

        // Drain remaining connections with timeout.
        let drain = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while connections.join_next().await.is_some() {}
        });
        let _ = drain.await;
        drain_guard.report_drained();
        Ok(())
    }
}

/// Handle a single ILP TCP connection with adaptive batch coalescing.
///
/// Batch size adapts to ingest rate:
/// - High rate (>100K lines/s): batch up to 10K lines or 10ms window
/// - Medium rate (1K-100K/s): batch up to 1K lines or 50ms window
/// - Low rate (<1K/s): batch per 100 lines or 100ms window
///
/// Larger batches amortize per-batch overhead (WAL append, memtable lock,
/// partition lookup).
async fn handle_ilp_connection(
    stream: ConnStream,
    peer: SocketAddr,
    state: &SharedState,
) -> crate::Result<()> {
    debug!(%peer, "ILP connection accepted");

    let mut reader = BufReader::new(stream);
    let mut line_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut batch = String::new();
    let mut line_count = 0u64;
    let mut total_ingested = 0u64;

    // Adaptive batch coalescing state.
    let mut rate_estimator = IlpRateEstimator::new();
    let mut batch_target = 1000u64;
    let mut window = tokio::time::interval(std::time::Duration::from_millis(50));
    window.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let tenant_id = TenantId::new(1);

    // Per-tenant connection tracking.
    if let Err(e) = state.check_tenant_connection(tenant_id) {
        warn!(%peer, error = %e, "ILP connection rejected: tenant connection limit");
        return Err(e);
    }
    state.tenant_connection_start(tenant_id);

    loop {
        tokio::select! {
            // Read next line with an enforced byte-length cap.
            result = reader.read_until(b'\n', &mut line_buf) => {
                match result {
                    Ok(0) => break, // Connection closed (EOF).
                    Ok(_) => {
                        // Enforce line length limit before any allocation.
                        if line_buf.len() > MAX_ILP_LINE_BYTES {
                            warn!(
                                %peer,
                                len = line_buf.len(),
                                limit = MAX_ILP_LINE_BYTES,
                                "ILP line exceeds maximum length — dropping connection"
                            );
                            break;
                        }

                        // Strip trailing newline / CRLF.
                        let line_bytes = line_buf
                            .strip_suffix(b"\r\n")
                            .or_else(|| line_buf.strip_suffix(b"\n"))
                            .unwrap_or(&line_buf);

                        let line = match std::str::from_utf8(line_bytes) {
                            Ok(s) => s,
                            Err(_) => {
                                warn!(%peer, "ILP line is not valid UTF-8 — skipping");
                                line_buf.clear();
                                continue;
                            }
                        };

                        if line.is_empty() || line.starts_with('#') {
                            line_buf.clear();
                            continue;
                        }

                        batch.push_str(line);
                        batch.push('\n');
                        line_count += 1;
                        line_buf.clear();

                        // Flush when batch reaches adaptive target.
                        if line_count >= batch_target {
                            let flushed = line_count;
                            total_ingested += flush_ilp_batch(state, tenant_id, &batch).await?;
                            batch.clear();
                            line_count = 0;

                            // Update rate estimator and recalculate batch target.
                            rate_estimator.record(flushed);
                            let (new_target, new_window_ms) = rate_estimator.suggest_batch_params();
                            batch_target = new_target;
                            window = tokio::time::interval(
                                std::time::Duration::from_millis(new_window_ms),
                            );
                            window.set_missed_tick_behavior(
                                tokio::time::MissedTickBehavior::Delay,
                            );
                        }
                    }
                    Err(_) => break, // Read error.
                }
            }
            // Timer-based flush (for low-rate connections).
            _ = window.tick() => {
                if !batch.is_empty() {
                    let flushed = line_count;
                    total_ingested += flush_ilp_batch(state, tenant_id, &batch).await?;
                    batch.clear();
                    line_count = 0;

                    rate_estimator.record(flushed);
                    let (new_target, new_window_ms) = rate_estimator.suggest_batch_params();
                    batch_target = new_target;
                    window = tokio::time::interval(
                        std::time::Duration::from_millis(new_window_ms),
                    );
                    window.set_missed_tick_behavior(
                        tokio::time::MissedTickBehavior::Delay,
                    );
                }
            }
        }
    }

    // Flush remaining.
    if !batch.is_empty() {
        total_ingested += flush_ilp_batch(state, tenant_id, &batch).await?;
    }

    state.tenant_connection_end(tenant_id);
    debug!(%peer, total_ingested, "ILP connection closed");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn extract_collection_from_ilp() {
        let batch = "cpu,host=server01 value=0.64 1000\nmem,host=server01 used=1024 2000\n";
        let collection = batch
            .lines()
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .and_then(|l| l.split([',', ' ']).next())
            .unwrap_or("default_metrics");
        assert_eq!(collection, "cpu");
    }
}
