// SPDX-License-Identifier: BUSL-1.1

//! DDL replication apply-side for mirror databases.
//!
//! When a `CREATE COLLECTION` / `ALTER COLLECTION` / `DROP COLLECTION` Raft
//! entry is applied on the source and forwarded to the mirror's observer stream,
//! this module resolves the mapping from source collection names to local
//! collection names and updates `_system.mirror_collection_map` and
//! `_system.mirror_lag` in a single atomic redb write transaction.
//!
//! Atomicity guarantee: `_system.mirror_collection_map` and `_system.mirror_lag`
//! are always updated in the same redb write transaction. If the server crashes
//! mid-apply, the next restart will re-apply the same Raft entry and the
//! idempotency check in `apply_ddl_entry_atomic` (LSN guard) prevents
//! double-application.
//!
//! Idempotency rule: if `last_applied_lsn >= entry_lsn`, the apply is a no-op.
//! This is the correct behaviour after restart: re-applying entries already
//! committed is safe and produces no observable change.

use nodedb_types::{DatabaseId, Lsn};

use crate::control::security::catalog::SystemCatalog;

/// The kind of DDL operation being replicated from the source.
///
/// Exhaustive matches are required — no `_ =>` arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorDdlKind {
    /// A `CREATE COLLECTION` was applied on the source. The mirror allocates
    /// a local mapping for this collection name.
    CreateCollection,
    /// An `ALTER COLLECTION` (schema change) was applied on the source.
    /// The collection mapping is left intact; the schema is applied locally.
    AlterCollection,
    /// A `DROP COLLECTION` was applied on the source. The mirror removes
    /// the collection locally and deletes the mapping entry.
    DropCollection,
}

/// Apply one DDL Raft entry from the source to a mirror database.
///
/// This is the **single entry point** for the mirror's DDL apply path. It must
/// be called on every DDL entry received from the source observer stream,
/// including on replay after restart. The function is idempotent: if
/// `entry_lsn` has already been applied, this returns `Ok(false)` immediately.
///
/// # Arguments
///
/// - `catalog` — reference to the live system catalog.
/// - `mirror_db_id` — the local mirror database id.
/// - `entry_lsn` — WAL LSN of the Raft entry on the source.
/// - `entry_apply_ms` — wall-clock milliseconds when this entry was applied.
/// - `source_collection_name` — the collection name on the source cluster.
/// - `kind` — the DDL operation type.
///
/// # Returns
///
/// `Ok(true)` if the entry was applied, `Ok(false)` if idempotent skip.
pub fn apply_mirror_ddl_entry(
    catalog: &SystemCatalog,
    mirror_db_id: DatabaseId,
    entry_lsn: Lsn,
    entry_apply_ms: u64,
    source_collection_name: &str,
    kind: MirrorDdlKind,
) -> crate::Result<bool> {
    match kind {
        MirrorDdlKind::CreateCollection | MirrorDdlKind::AlterCollection => {
            // For CREATE and ALTER: upsert the collection map entry with the
            // same name on both sides (the mapping allows future RENAME
            // ON MIRROR operations to decouple names, but by default they match).
            //
            // If the source later renames the collection, a new DDL entry will
            // arrive with the new source name; the old mapping stays for
            // historical reads until the mirror is re-bootstrapped.
            let local_collection_name = source_collection_name;
            catalog.apply_ddl_entry_atomic(
                mirror_db_id,
                entry_lsn,
                entry_apply_ms,
                source_collection_name,
                local_collection_name,
            )
        }
        MirrorDdlKind::DropCollection => {
            // For DROP: use a sentinel value that marks the collection as dropped.
            // The read path must check for this sentinel and skip the collection.
            // We still advance the LSN watermark atomically.
            let drop_sentinel = "";
            catalog.apply_ddl_entry_atomic(
                mirror_db_id,
                entry_lsn,
                entry_apply_ms,
                source_collection_name,
                drop_sentinel,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use nodedb_types::{DatabaseId, Lsn};
    use tempfile::TempDir;

    use super::*;
    use crate::control::security::catalog::SystemCatalog;

    fn open_tmp_catalog(tmp: &TempDir) -> SystemCatalog {
        let path: PathBuf = tmp.path().join("system.redb");
        SystemCatalog::open(&path).expect("open catalog")
    }

    #[test]
    fn mirror_ddl_create_applies_and_updates_lag() {
        let tmp = TempDir::new().unwrap();
        let catalog = open_tmp_catalog(&tmp);
        let db_id = DatabaseId::new(1024);

        let applied = apply_mirror_ddl_entry(
            &catalog,
            db_id,
            Lsn::new(5),
            1000,
            "orders",
            MirrorDdlKind::CreateCollection,
        )
        .unwrap();
        assert!(applied, "first apply must return true");

        let lag = catalog.get_mirror_lag(db_id).unwrap().unwrap();
        assert_eq!(lag.last_applied_lsn, Lsn::new(5));
        assert_eq!(lag.last_apply_ms, 1000);

        let mapping = catalog
            .get_mirror_collection_mapping(db_id, "orders")
            .unwrap();
        assert_eq!(mapping, Some("orders".to_string()));
    }

    #[test]
    fn mirror_ddl_is_idempotent_across_simulated_restart() {
        let tmp = TempDir::new().unwrap();
        let catalog = open_tmp_catalog(&tmp);
        let db_id = DatabaseId::new(1025);
        let lsn = Lsn::new(10);

        // First apply.
        let first = apply_mirror_ddl_entry(
            &catalog,
            db_id,
            lsn,
            500,
            "events",
            MirrorDdlKind::CreateCollection,
        )
        .unwrap();
        assert!(first);

        // Simulate restart: re-apply the same entry.
        let second = apply_mirror_ddl_entry(
            &catalog,
            db_id,
            lsn,
            500,
            "events",
            MirrorDdlKind::CreateCollection,
        )
        .unwrap();
        assert!(!second, "idempotent replay must return false");

        // Lag record must be unchanged.
        let lag = catalog.get_mirror_lag(db_id).unwrap().unwrap();
        assert_eq!(lag.last_applied_lsn, lsn);
    }

    #[test]
    fn mirror_ddl_drop_advances_lsn() {
        let tmp = TempDir::new().unwrap();
        let catalog = open_tmp_catalog(&tmp);
        let db_id = DatabaseId::new(1026);

        apply_mirror_ddl_entry(
            &catalog,
            db_id,
            Lsn::new(3),
            300,
            "tmp_table",
            MirrorDdlKind::CreateCollection,
        )
        .unwrap();
        apply_mirror_ddl_entry(
            &catalog,
            db_id,
            Lsn::new(7),
            700,
            "tmp_table",
            MirrorDdlKind::DropCollection,
        )
        .unwrap();

        let lag = catalog.get_mirror_lag(db_id).unwrap().unwrap();
        assert_eq!(lag.last_applied_lsn, Lsn::new(7));
    }
}
