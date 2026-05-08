// SPDX-License-Identifier: BUSL-1.1

//! WAL replay for timeseries records.
//!
//! On startup, replays `TimeseriesBatch` records into the per-core
//! columnar memtable. Only replays records with LSN > `last_flushed_wal_lsn`
//! per partition (not max_ts — safe with out-of-order data).

use crate::bridge::envelope::{PhysicalPlan, Priority, Request};
use crate::bridge::physical_plan::{ColumnarInsertIntent, ColumnarOp, TimeseriesOp};
use crate::data::executor::core_loop::CoreLoop;
use crate::data::executor::task::{ExecutionTask, TaskState};
use crate::engine::timeseries::columnar_memtable::{
    ColumnarMemtable, ColumnarMemtableConfig, ColumnarSchema,
};
use crate::types::DatabaseId;
use crate::types::ReadConsistency;
use nodedb_types::timeseries::MetricSample;

/// Default timeseries memtable configuration for replay and auto-creation.
fn default_ts_config() -> ColumnarMemtableConfig {
    ColumnarMemtableConfig {
        max_memory_bytes: 64 * 1024 * 1024,
        hard_memory_limit: 80 * 1024 * 1024,
        max_tag_cardinality: 100_000,
    }
}

impl CoreLoop {
    fn replay_task(
        tenant_id: crate::types::TenantId,
        vshard_id: crate::types::VShardId,
        plan: PhysicalPlan,
    ) -> ExecutionTask {
        ExecutionTask {
            request: Request {
                request_id: crate::types::RequestId::new(0),
                tenant_id,
                database_id: DatabaseId::DEFAULT,
                vshard_id,
                plan,
                deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
                priority: Priority::Normal,
                trace_id: crate::types::TraceId::ZERO,
                consistency: ReadConsistency::Strong,
                idempotency_key: None,
                event_source: crate::event::EventSource::User,
                user_roles: Vec::new(),
            },
            state: TaskState::Running,
        }
    }

    /// Ensure a timeseries memtable exists for the given collection, creating if needed.
    fn ensure_columnar_memtable(
        &mut self,
        key: (crate::types::TenantId, String),
        schema: ColumnarSchema,
    ) {
        self.columnar_memtables
            .entry(key)
            .or_insert_with(|| ColumnarMemtable::new(schema, default_ts_config()));
    }

    fn replay_timeseries_payload(
        &mut self,
        tid: crate::types::TenantId,
        collection: &str,
        payload: &[u8],
        record_lsn: u64,
    ) -> usize {
        if let Ok(batch) =
            zerompk::from_msgpack::<nodedb_types::timeseries::TimeseriesWalBatch>(payload)
        {
            let key = (tid, collection.to_string());
            self.ensure_columnar_memtable(key.clone(), ColumnarSchema::metric_default());

            let Some(mt) = self.columnar_memtables.get_mut(&key) else {
                return 0;
            };
            for (series_id, timestamp_ms, value) in &batch.samples {
                mt.ingest_metric(
                    *series_id,
                    MetricSample {
                        timestamp_ms: *timestamp_ms,
                        value: *value,
                    },
                );
            }
            let sample_count = batch.samples.len();
            if sample_count > 0
                && let Some(ref gov) = self.governor
            {
                let _ = gov.try_reserve(nodedb_mem::EngineId::Timeseries, sample_count * 24);
            }
            return sample_count;
        }

        let format = if std::str::from_utf8(payload).is_ok() {
            "ilp"
        } else {
            "msgpack"
        };
        let task = Self::replay_task(
            tid,
            crate::types::VShardId::from_collection_in_database(DatabaseId::DEFAULT, collection),
            PhysicalPlan::Timeseries(TimeseriesOp::Ingest {
                collection: collection.to_string(),
                payload: payload.to_vec(),
                format: format.to_string(),
                wal_lsn: Some(record_lsn),
                surrogates: Vec::new(),
            }),
        );
        let response = self.execute_timeseries_ingest(
            &task,
            tid,
            collection,
            payload,
            format,
            Some(record_lsn),
        );
        if response.status != crate::bridge::envelope::Status::Ok {
            tracing::warn!(
                "timeseries WAL replay failed for collection={collection} lsn={record_lsn}: {:?}",
                response.error_code
            );
            return 0;
        }
        match nodedb_types::value_from_msgpack(payload) {
            Ok(nodedb_types::Value::Array(rows)) => rows.len(),
            Ok(nodedb_types::Value::Object(_)) => 1,
            _ => 0,
        }
    }

    fn replay_columnar_payload(
        &mut self,
        tid: crate::types::TenantId,
        collection: &str,
        payload: &[u8],
    ) -> usize {
        let task = Self::replay_task(
            tid,
            crate::types::VShardId::from_collection_in_database(DatabaseId::DEFAULT, collection),
            PhysicalPlan::Columnar(ColumnarOp::Insert {
                collection: collection.to_string(),
                payload: payload.to_vec(),
                format: "msgpack".into(),
                intent: ColumnarInsertIntent::Insert,
                on_conflict_updates: Vec::new(),
                surrogates: Vec::new(),
            }),
        );
        let response = self.execute_columnar_insert(
            &task,
            collection,
            payload,
            "msgpack",
            ColumnarInsertIntent::Insert,
            &[],
            &[],
        );
        if response.status != crate::bridge::envelope::Status::Ok {
            tracing::warn!(
                "columnar WAL replay failed for collection={collection}: {:?}",
                response.error_code
            );
            return 0;
        }
        match nodedb_types::value_from_msgpack(payload) {
            Ok(nodedb_types::Value::Array(rows)) => rows.len(),
            Ok(nodedb_types::Value::Object(_)) => 1,
            _ => 0,
        }
    }

    /// Replay WAL timeseries records to rebuild in-memory memtable state after crash.
    ///
    /// Called once during startup, after `open()` but before the event loop.
    /// Processes `TimeseriesBatch` records, ignoring records for other vShards.
    /// Uses LSN-based skip: only replays records with LSN > last flushed LSN.
    pub fn replay_timeseries_wal(
        &mut self,
        records: &[nodedb_wal::WalRecord],
        num_cores: usize,
        tombstones: &nodedb_wal::TombstoneSet,
    ) {
        use nodedb_wal::record::RecordType;

        let mut replayed = 0usize;
        let mut skipped = 0usize;

        for record in records {
            let logical_type = record.logical_record_type();
            let record_type = RecordType::from_raw(logical_type);

            let is_ts_batch = record_type == Some(RecordType::TimeseriesBatch);
            if !is_ts_batch {
                continue;
            }

            // Route by vShard to the correct core.
            let vshard_id = record.header.vshard_id as usize;
            let target_core = if num_cores > 0 {
                vshard_id % num_cores
            } else {
                0
            };
            if target_core != self.core_id {
                skipped += 1;
                continue;
            }

            let decoded = zerompk::from_msgpack::<(String, String, Vec<u8>)>(&record.payload)
                .map(|(kind, collection, payload)| (Some(kind), collection, payload))
                .or_else(|_| {
                    zerompk::from_msgpack::<(String, Vec<u8>)>(&record.payload)
                        .map(|(collection, payload)| (None, collection, payload))
                });
            let Ok((kind, raw_collection, payload)) = decoded else {
                tracing::warn!(
                    core = self.core_id,
                    lsn = record.header.lsn,
                    "skipping malformed TimeseriesBatch WAL record"
                );
                continue;
            };

            let tenant_id = record.header.tenant_id;
            let tid_id = crate::types::TenantId::new(tenant_id);
            let collection = raw_collection.as_str();
            let key = (tid_id, raw_collection.clone());

            let record_lsn = record.header.lsn;

            // Skip records for collections that were hard-deleted after
            // this write. Otherwise the purged memtable would resurrect.
            if tombstones.is_tombstoned(tenant_id, collection, record_lsn) {
                skipped += 1;
                continue;
            }

            // Check if this record was already flushed (LSN-based skip).
            if let Some(registry) = self.ts_registries.get(&key) {
                // Find the max flushed LSN across all partitions.
                let max_flushed_lsn = registry
                    .iter()
                    .map(|(_, e)| e.meta.last_flushed_wal_lsn)
                    .max()
                    .unwrap_or(0);
                if record_lsn <= max_flushed_lsn {
                    skipped += 1;
                    continue;
                }
            }

            // Track the max WAL LSN ingested per collection for flush metadata.
            if let Some(entry) = self.ts_max_ingested_lsn.get_mut(&key) {
                *entry = (*entry).max(record_lsn);
            } else {
                self.ts_max_ingested_lsn.insert(key.clone(), record_lsn);
            }

            let accepted = match kind.as_deref() {
                Some("columnar") => self.replay_columnar_payload(tid_id, collection, &payload),
                Some("timeseries") | None => {
                    self.replay_timeseries_payload(tid_id, collection, &payload, record_lsn)
                }
                Some(other) => {
                    tracing::warn!(
                        core = self.core_id,
                        lsn = record_lsn,
                        kind = other,
                        "skipping unknown TimeseriesBatch WAL kind"
                    );
                    0
                }
            };
            if accepted == 0 {
                continue;
            }
            replayed += accepted;
        }

        if replayed > 0 {
            tracing::info!(
                core = self.core_id,
                replayed,
                skipped,
                collections = self.columnar_memtables.len(),
                "WAL timeseries replay complete"
            );
        }
    }
}
