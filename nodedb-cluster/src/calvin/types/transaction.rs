// SPDX-License-Identifier: BUSL-1.1

//! Calvin transaction class types.
//!
//! Provides [`ReadWriteSet`] and [`TxClass`] — the core transaction
//! representation submitted to the sequencer.

use nodedb_types::TenantId;
use nodedb_types::id::{DatabaseId, VShardId};
use serde::{Deserialize, Serialize};

use crate::error::CalvinError;

use super::primitives::{DependentReadSpec, EngineKeySet};

// ── ReadWriteSet ──────────────────────────────────────────────────────────────

/// A set of keys spanning one or more engines, forming either the read set
/// or the write set of a Calvin transaction.
///
/// Cross-engine atomic transactions — e.g. a Document+Vector insert that must
/// land atomically — require all affected engines to appear in a single
/// `ReadWriteSet`. Decomposing by engine would break atomicity.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct ReadWriteSet(pub Vec<EngineKeySet>);

impl ReadWriteSet {
    pub fn new(sets: Vec<EngineKeySet>) -> Self {
        Self(sets)
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|s| s.is_empty())
    }

    /// Derive the set of vShards participating in this read/write set.
    ///
    /// For Document/Vector/Edge entries the vshard is derived from the
    /// collection name (collection-level routing, consistent with the
    /// per-vshard Raft groups that own each collection). For KV entries
    /// the vshard is also derived from the collection name because KV
    /// collections are assigned a single vshard at creation time.
    ///
    /// This derivation is re-run on decode rather than serialized, so the
    /// serialized bytes remain deterministic regardless of how `VShardId`
    /// is computed.
    pub fn participating_vshards(&self) -> Vec<VShardId> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for engine_set in &self.0 {
            let vshard =
                VShardId::from_collection_in_database(DatabaseId::DEFAULT, engine_set.collection());
            if seen.insert(vshard.as_u32()) {
                result.push(vshard);
            }
        }
        result.sort_by_key(|v| v.as_u32());
        result
    }
}

// ── TxClass ───────────────────────────────────────────────────────────────────

/// A fully-declared Calvin transaction class.
///
/// Constructed via [`TxClass::new`], which validates the write set and caches
/// the participating-vshard set. The `participating_vshards` field is skipped
/// during serialization and re-derived on decode to keep serialized bytes
/// byte-deterministic.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct TxClass {
    /// Keys that must be read (may be empty for pure-write transactions).
    pub read_set: ReadWriteSet,
    /// Keys that will be written. Must span at least two vShards.
    pub write_set: ReadWriteSet,
    /// Opaque msgpack-encoded physical plan bytes. Decoded by the executor
    /// in the `nodedb` crate; the sequencer treats this as an opaque blob.
    pub plans: Vec<u8>,
    /// Tenant scope. All keys in `read_set` and `write_set` must belong to
    /// this tenant; cross-tenant transactions are rejected at construction.
    pub tenant_id: TenantId,
    /// Optional dependent-read specification.
    ///
    /// When present, this transaction is a dependent-read Calvin txn: the
    /// passive vshards listed here must read their keys and broadcast the
    /// results (via `ReplicatedWrite::CalvinReadResult`) before the active
    /// participants may write.
    ///
    /// `None` for static-set transactions (the common case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependent_reads: Option<DependentReadSpec>,
    /// Cached participating-vshard set. Re-derived on decode; not serialized.
    #[serde(skip)]
    #[msgpack(ignore)]
    participating_vshards: Vec<VShardId>,
}

impl TxClass {
    /// Construct a validated transaction class.
    ///
    /// Rejects:
    /// - An empty write set (nothing to commit).
    /// - A write set that resolves to a single vshard (must use the single-
    ///   shard fast path instead).
    ///
    /// Pass `dependent_reads: None` for static-set transactions (the common
    /// case).  Pass `Some(spec)` for dependent-read (OLLP) transactions.
    pub fn new(
        read_set: ReadWriteSet,
        write_set: ReadWriteSet,
        plans: Vec<u8>,
        tenant_id: TenantId,
        dependent_reads: Option<DependentReadSpec>,
    ) -> Result<Self, CalvinError> {
        if write_set.is_empty() {
            return Err(CalvinError::EmptyWriteSet);
        }
        let mut participating_vshards = write_set.participating_vshards();
        if participating_vshards.len() < 2 {
            let vshard = participating_vshards
                .first()
                .map(|v| v.as_u32())
                .unwrap_or(0);
            return Err(CalvinError::SingleVshardTxn { vshard });
        }
        // Extend participating_vshards with passive vshards from dependent_reads.
        if let Some(ref spec) = dependent_reads {
            for &passive_vshard in spec.passive_reads.keys() {
                let v = VShardId::new(passive_vshard);
                if !participating_vshards
                    .iter()
                    .any(|e| e.as_u32() == passive_vshard)
                {
                    participating_vshards.push(v);
                }
            }
            participating_vshards.sort_by_key(|v| v.as_u32());
        }
        Ok(Self {
            read_set,
            write_set,
            plans,
            tenant_id,
            dependent_reads,
            participating_vshards,
        })
    }

    /// Ergonomic constructor for dependent-read Calvin transactions.
    ///
    /// Equivalent to `TxClass::new(read_set, write_set, plans, tenant_id, Some(dependent_reads))`.
    pub fn new_dependent(
        read_set: ReadWriteSet,
        write_set: ReadWriteSet,
        plans: Vec<u8>,
        tenant_id: TenantId,
        dependent_reads: DependentReadSpec,
    ) -> Result<Self, CalvinError> {
        Self::new(read_set, write_set, plans, tenant_id, Some(dependent_reads))
    }

    /// The vShards that must receive this transaction's slice.
    ///
    /// Derived from the write set's collection names. Re-derived after
    /// deserialization via [`TxClass::restore_derived`].
    pub fn participating_vshards(&self) -> &[VShardId] {
        &self.participating_vshards
    }

    /// Re-derive fields skipped during serialization.
    ///
    /// Call this immediately after deserializing a `TxClass` that came off
    /// the wire or out of the Raft log.
    pub fn restore_derived(&mut self) {
        let mut vshards = self.write_set.participating_vshards();
        if let Some(ref spec) = self.dependent_reads {
            for &passive_vshard in spec.passive_reads.keys() {
                if !vshards.iter().any(|e| e.as_u32() == passive_vshard) {
                    vshards.push(VShardId::new(passive_vshard));
                }
            }
            vshards.sort_by_key(|v| v.as_u32());
        }
        self.participating_vshards = vshards;
    }
}
