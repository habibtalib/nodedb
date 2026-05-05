//! Helper functions extracted from `start_raft` to keep that function
//! within the 500-line production-code limit.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use nodedb_cluster::calvin::SequencerStateMachine;
use nodedb_cluster::distributed_array::{ArrayLocalExecutor, handle_array_shard_rpc};
use nodedb_cluster::vshard_handler::{DispatchTarget, dispatch_by_type};
use nodedb_cluster::wire::VShardEnvelope;

use crate::control::cluster::calvin::scheduler::metrics::SchedulerMetrics;
use crate::control::cluster::calvin::scheduler::read_last_applied_epoch;
use crate::control::cluster::calvin::{ReadResultEvent, Scheduler, SchedulerConfig};
use crate::control::cluster::handle::ClusterHandle;
use crate::control::state::SharedState;

/// Build the `VShardEnvelopeHandler` closure used by `RaftLoop`.
///
/// The closure receives raw envelope bytes from the QUIC transport layer,
/// dispatches based on `msg_type`, and returns a serialized response.
pub(super) fn build_vshard_handler(
    array_executor: Arc<dyn ArrayLocalExecutor>,
) -> nodedb_cluster::VShardEnvelopeHandler {
    Arc::new(move |bytes: Vec<u8>| {
        let executor = array_executor.clone();
        let fut: Pin<
            Box<dyn std::future::Future<Output = nodedb_cluster::error::Result<Vec<u8>>> + Send>,
        > = Box::pin(async move {
            let envelope = VShardEnvelope::from_bytes(&bytes).ok_or_else(|| {
                nodedb_cluster::error::ClusterError::Codec {
                    detail: "vshard_handler: failed to deserialize VShardEnvelope".into(),
                }
            })?;

            let target = dispatch_by_type(&envelope);
            match target {
                DispatchTarget::ArrayShard => {
                    let opcode = envelope.msg_type as u32;
                    let resp_payload = handle_array_shard_rpc(
                        opcode,
                        envelope.vshard_id,
                        &envelope.payload,
                        &executor,
                    )
                    .await?;

                    // Response opcode = request opcode + 1 for all array shard RPCs.
                    // Resolve the msg_type variant via a minimal scratch envelope parse
                    // (avoids any unsafe transmute — the `from_bytes` mapping in wire.rs
                    // is the canonical source of truth for the opcode→variant table).
                    let resp_opcode = opcode + 1;
                    let resp_msg_type = resolve_vshard_msg_type(resp_opcode)?;
                    let resp_envelope = VShardEnvelope::new(
                        resp_msg_type,
                        envelope.target_node,
                        envelope.source_node,
                        envelope.vshard_id,
                        resp_payload,
                    );
                    Ok(resp_envelope.to_bytes())
                }

                other => Err(nodedb_cluster::error::ClusterError::Transport {
                    detail: format!(
                        "vshard_handler: no handler registered for dispatch target {other:?}"
                    ),
                }),
            }
        });
        fut
    })
}

/// Spawn a `Scheduler` task for each local vShard.
///
/// Reads the last applied epoch from the WAL for each vShard, wires the
/// sequenced-tx sender into the `SequencerStateMachine`, registers a
/// read-result sender, and tokio-spawns a `Scheduler::run` loop.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_vshard_schedulers(
    handle: &ClusterHandle,
    shared: &Arc<SharedState>,
    raft_loop_handle: Arc<Mutex<nodedb_cluster::multi_raft::MultiRaft>>,
    sequencer_state_machine: &Arc<Mutex<SequencerStateMachine>>,
    calvin_read_result_senders: &Arc<
        Mutex<std::collections::BTreeMap<u32, tokio::sync::mpsc::Sender<ReadResultEvent>>>,
    >,
    scheduler_config: &SchedulerConfig,
) -> crate::Result<()> {
    let mut local_vshards: Vec<u32> = {
        let routing = handle.routing.read().unwrap_or_else(|p| p.into_inner());
        let mut vshards = Vec::new();
        for (group_id, info) in routing.group_members() {
            if info.members.contains(&handle.node_id) {
                vshards.extend(routing.vshards_for_group(*group_id));
            }
        }
        vshards
    };
    local_vshards.sort_unstable();
    local_vshards.dedup();

    for vshard_id in local_vshards {
        let last_applied_epoch = read_last_applied_epoch(&shared.wal, vshard_id)?;
        let (sequenced_tx, sequenced_rx) =
            tokio::sync::mpsc::channel(scheduler_config.channel_capacity);
        sequencer_state_machine
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_vshard_sender(vshard_id, sequenced_tx);

        let (read_result_tx, read_result_rx) =
            tokio::sync::mpsc::channel(scheduler_config.channel_capacity);
        calvin_read_result_senders
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(vshard_id, read_result_tx);

        let scheduler = Scheduler::new(
            vshard_id,
            sequenced_rx,
            Arc::clone(shared),
            raft_loop_handle.clone(),
            last_applied_epoch,
            last_applied_epoch,
            scheduler_config.clone(),
            SchedulerMetrics::new(),
            read_result_rx,
        );
        let shutdown = shared.shutdown.subscribe();
        tokio::spawn(async move {
            scheduler.run(shutdown).await;
        });
    }

    Ok(())
}

/// Resolve a raw opcode `u32` to a `VShardMessageType` variant.
///
/// Uses `VShardEnvelope::from_bytes` as the canonical opcode→variant mapping
/// so this helper stays in sync with the wire format without duplicating the
/// match table.
pub(super) fn resolve_vshard_msg_type(
    opcode: u32,
) -> nodedb_cluster::error::Result<nodedb_cluster::wire::VShardMessageType> {
    let mut scratch = [0u8; 26];
    scratch[0..2].copy_from_slice(&1u16.to_le_bytes()); // version
    scratch[2..4].copy_from_slice(&(opcode as u16).to_le_bytes()); // msg_type

    VShardEnvelope::from_bytes(&scratch)
        .map(|e| e.msg_type)
        .ok_or_else(|| nodedb_cluster::error::ClusterError::Codec {
            detail: format!("resolve_vshard_msg_type: unknown opcode {opcode}"),
        })
}
