// SPDX-License-Identifier: BUSL-1.1

//! Background loop and subsystem spawning after SharedState is ready.

use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::ServerConfig;
use crate::control::state::SharedState;
use crate::event::bus::EventConsumerRx;
use crate::event::trigger::TriggerDlq;
use crate::event::watermark::WatermarkStore;
use crate::wal::WalManager;

/// Event Plane components passed to [`spawn_background_loops`].
pub struct EventPlaneComponents {
    pub wal: Arc<WalManager>,
    pub event_consumers: Vec<EventConsumerRx>,
    pub watermark_store: Arc<WatermarkStore>,
    pub trigger_dlq: Arc<std::sync::Mutex<TriggerDlq>>,
}

/// Spawn all persistent background subsystems.
///
/// Includes: Event Plane consumers, event trigger processor, webhook manager wiring,
/// collection GC, L2 cleanup, tenant rate/audit/memory timers, checkpoint manager,
/// usage metering flush, and cold tier task.
///
/// Returns the [`EventPlane`] handle. The caller MUST hold this for the
/// server's lifetime — dropping it aborts every consumer task and turns
/// the per-core event ring buffers into one-way drains, which silently
/// loses every WriteEvent emitted by the Data Plane until process exit.
#[must_use = "EventPlane must be held for the server's lifetime; dropping it stops all event consumers"]
pub fn spawn_background_loops(
    shared: &Arc<SharedState>,
    components: EventPlaneComponents,
    config: &ServerConfig,
    num_cores: usize,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> crate::event::EventPlane {
    let EventPlaneComponents {
        wal,
        event_consumers,
        watermark_store,
        trigger_dlq,
    } = components;
    // Event trigger processor.
    crate::control::event_trigger::spawn_event_trigger_processor(Arc::clone(shared));

    // Wire webhook manager.
    shared.webhook_manager.set_state(Arc::clone(shared));

    // Event Plane: one consumer Tokio task per Data Plane core.
    // Returned to the caller — must outlive the server, otherwise its
    // Drop impl aborts every consumer and the Data Plane producers
    // start dropping every WriteEvent they emit.
    let event_plane = crate::event::EventPlane::spawn(
        event_consumers,
        Arc::clone(&wal),
        watermark_store,
        Arc::clone(shared),
        trigger_dlq,
        Arc::clone(&shared.cdc_router),
        Arc::clone(&shared.shutdown),
    );
    info!(num_cores, "event plane running");

    // Collection hard-delete retention GC.
    if let Ok(mut w) = shared.retention_settings.write() {
        *w = config.retention.clone();
    }
    let _collection_gc = crate::event::collection_gc::spawn_collection_gc(Arc::clone(shared));
    info!(
        retention_days = config.retention.deactivated_collection_retention_days,
        sweep_interval_secs = config.retention.gc_sweep_interval_secs,
        "collection-gc sweeper running"
    );

    // L2 cleanup worker.
    let _l2_cleanup = crate::event::collection_gc::spawn_l2_cleanup(Arc::clone(shared));

    // Tenant rate counter reset (1-second timer).
    let shared_rate = Arc::clone(shared);
    crate::control::shutdown::spawn_loop(
        &shared.loop_registry,
        &shared.shutdown,
        "tenant_rate_reset",
        move |mut shutdown| async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = shutdown.wait_cancelled() => break,
                    _ = tick.tick() => shared_rate.reset_tenant_rate_counters(),
                }
            }
        },
    );

    // Audit log flush (10-second timer).
    let shared_audit = Arc::clone(shared);
    crate::control::shutdown::spawn_loop(
        &shared.loop_registry,
        &shared.shutdown,
        "audit_log_flush",
        move |mut shutdown| async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    _ = shutdown.wait_cancelled() => break,
                    _ = tick.tick() => shared_audit.flush_audit_log(),
                }
            }
        },
    );

    // Tenant memory estimation (30-second timer).
    let shared_mem = Arc::clone(shared);
    crate::control::shutdown::spawn_loop(
        &shared.loop_registry,
        &shared.shutdown,
        "tenant_memory_estimate",
        move |mut shutdown| async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = shutdown.wait_cancelled() => break,
                    _ = tick.tick() => shared_mem.update_tenant_memory_estimates(),
                }
            }
        },
    );

    // Checkpoint manager.
    let shared_ckpt = Arc::clone(shared);
    let shutdown_rx_ckpt = shutdown_rx.clone();
    crate::control::checkpoint_manager::spawn_checkpoint_task(
        shared_ckpt,
        num_cores,
        config.checkpoint.to_manager_config(),
        shutdown_rx_ckpt,
    );

    // Usage metering flush.
    let _metering_flush = crate::control::security::metering::counter::spawn_flush_task(
        Arc::clone(&shared.usage_counter),
        Arc::clone(&shared.usage_store),
        60,
    );

    // Cold tier task (if configured).
    if let Some(ref cold_settings) = config.cold_storage {
        let shared_cold = Arc::clone(shared);
        let cold_settings_clone = cold_settings.clone();
        let data_dir_clone = config.server.data_dir.clone();
        let shutdown_rx_cold = shutdown_rx.clone();
        crate::control::cold_tier::spawn_cold_tier_task(
            shared_cold,
            cold_settings_clone,
            data_dir_clone,
            shutdown_rx_cold,
        );
        info!("cold tier task spawned");
    }

    event_plane
}

/// Spawn the response poller loop (routes Data Plane responses to waiting sessions).
pub fn spawn_response_poller(shared: &Arc<SharedState>) {
    let shared_poller = Arc::clone(shared);
    crate::control::shutdown::spawn_loop(
        &shared.loop_registry,
        &shared.shutdown,
        "response_poller",
        move |shutdown| async move {
            let mut idle_iters: u32 = 0;
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                let routed = shared_poller.poll_and_route_responses();
                if routed > 0 {
                    idle_iters = 0;
                    tokio::task::yield_now().await;
                    continue;
                }
                idle_iters = idle_iters.saturating_add(1);
                if idle_iters <= 256 {
                    tokio::task::yield_now().await;
                } else if idle_iters <= 1024 {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        },
    );
}
