// SPDX-License-Identifier: BUSL-1.1

//! Surrogate ↔ PK catalog ops for the `_system.surrogate_pk{,_rev}_v2` tables.
//!
//! Forward + reverse mapping between user-visible primary keys and the
//! global `Surrogate` allocator. Every method writes both tables atomically
//! in a single redb write transaction so the two directions can never drift.
//!
//! The compound key is `(database_id, collection, pk_bytes)` (forward) and
//! `(database_id, collection, surrogate)` (reverse), scoping the PK map
//! to its database boundary.
//!
//! ## Migration
//!
//! `migrate_surrogate_pk()` reads all rows from the legacy bare
//! `_system.surrogate_pk` / `_system.surrogate_pk_rev` tables and rewrites
//! them under the v2 tables with `DatabaseId::DEFAULT` prepended.
//! Idempotent: skips if v2 is already non-empty.

use nodedb_types::{DatabaseId, Surrogate};
use redb::{ReadableTable, ReadableTableMetadata};

#[allow(unused_imports)] // SURROGATE_PK_REV_LEGACY is used only in #[cfg(test)] helpers
use super::types::{
    SURROGATE_PK, SURROGATE_PK_LEGACY, SURROGATE_PK_REV, SURROGATE_PK_REV_LEGACY, SystemCatalog,
    catalog_err,
};

impl SystemCatalog {
    /// Insert or overwrite a surrogate ↔ PK binding. Writes both the
    /// forward and reverse rows in one txn.
    ///
    /// Idempotent: re-binding the same `(database_id, collection, pk_bytes)` to
    /// the same surrogate is a no-op-on-disk overwrite.
    pub fn put_surrogate(
        &self,
        database_id: DatabaseId,
        collection: &str,
        pk_bytes: &[u8],
        surrogate: Surrogate,
    ) -> crate::Result<()> {
        let db_id = database_id.as_u64();
        let txn = self
            .db
            .begin_write()
            .map_err(|e| catalog_err("surrogate_pk write txn", e))?;
        {
            let mut fwd = txn
                .open_table(SURROGATE_PK)
                .map_err(|e| catalog_err("open surrogate_pk", e))?;
            fwd.insert((db_id, collection, pk_bytes), surrogate.as_u32())
                .map_err(|e| catalog_err("insert surrogate_pk", e))?;
            let mut rev = txn
                .open_table(SURROGATE_PK_REV)
                .map_err(|e| catalog_err("open surrogate_pk_rev", e))?;
            rev.insert((db_id, collection, surrogate.as_u32()), pk_bytes)
                .map_err(|e| catalog_err("insert surrogate_pk_rev", e))?;
        }
        txn.commit()
            .map_err(|e| catalog_err("surrogate_pk commit", e))
    }

    /// Look up the surrogate previously bound to `(database_id, collection, pk_bytes)`.
    /// Returns `None` if no binding exists.
    pub fn get_surrogate_for_pk(
        &self,
        database_id: DatabaseId,
        collection: &str,
        pk_bytes: &[u8],
    ) -> crate::Result<Option<Surrogate>> {
        let db_id = database_id.as_u64();
        let txn = self
            .db
            .begin_read()
            .map_err(|e| catalog_err("surrogate_pk read txn", e))?;
        let table = txn
            .open_table(SURROGATE_PK)
            .map_err(|e| catalog_err("open surrogate_pk", e))?;
        match table
            .get((db_id, collection, pk_bytes))
            .map_err(|e| catalog_err("get surrogate_pk", e))?
        {
            Some(v) => Ok(Some(Surrogate::new(v.value()))),
            None => Ok(None),
        }
    }

    /// Look up the PK previously bound to `(database_id, collection, surrogate)`.
    /// Returns `None` if no binding exists.
    pub fn get_pk_for_surrogate(
        &self,
        database_id: DatabaseId,
        collection: &str,
        surrogate: Surrogate,
    ) -> crate::Result<Option<Vec<u8>>> {
        let db_id = database_id.as_u64();
        let txn = self
            .db
            .begin_read()
            .map_err(|e| catalog_err("surrogate_pk_rev read txn", e))?;
        let table = txn
            .open_table(SURROGATE_PK_REV)
            .map_err(|e| catalog_err("open surrogate_pk_rev", e))?;
        match table
            .get((db_id, collection, surrogate.as_u32()))
            .map_err(|e| catalog_err("get surrogate_pk_rev", e))?
        {
            Some(v) => Ok(Some(v.value().to_vec())),
            None => Ok(None),
        }
    }

    /// Remove a surrogate ↔ PK binding atomically. Idempotent.
    pub fn delete_surrogate(
        &self,
        database_id: DatabaseId,
        collection: &str,
        pk_bytes: &[u8],
    ) -> crate::Result<()> {
        let db_id = database_id.as_u64();
        let txn = self
            .db
            .begin_write()
            .map_err(|e| catalog_err("surrogate_pk delete txn", e))?;
        {
            let mut fwd = txn
                .open_table(SURROGATE_PK)
                .map_err(|e| catalog_err("open surrogate_pk", e))?;
            let removed = fwd
                .remove((db_id, collection, pk_bytes))
                .map_err(|e| catalog_err("remove surrogate_pk", e))?;
            if let Some(v) = removed {
                let surrogate = v.value();
                let mut rev = txn
                    .open_table(SURROGATE_PK_REV)
                    .map_err(|e| catalog_err("open surrogate_pk_rev", e))?;
                rev.remove((db_id, collection, surrogate))
                    .map_err(|e| catalog_err("remove surrogate_pk_rev", e))?;
            }
        }
        txn.commit()
            .map_err(|e| catalog_err("surrogate_pk delete commit", e))
    }

    /// Scan every binding for a `(database_id, collection)` pair.
    /// Returns `Vec<(pk_bytes, surrogate)>` in redb's natural key order.
    pub fn scan_surrogates_for_collection(
        &self,
        database_id: DatabaseId,
        collection: &str,
    ) -> crate::Result<Vec<(Vec<u8>, Surrogate)>> {
        let db_id = database_id.as_u64();
        let txn = self
            .db
            .begin_read()
            .map_err(|e| catalog_err("surrogate_pk scan txn", e))?;
        let table = txn
            .open_table(SURROGATE_PK)
            .map_err(|e| catalog_err("open surrogate_pk", e))?;
        let mut out = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| catalog_err("iter surrogate_pk", e))?;
        for row in iter {
            let (k, v) = row.map_err(|e| catalog_err("iter surrogate_pk row", e))?;
            let (row_db_id, coll, pk) = k.value();
            if row_db_id == db_id && coll == collection {
                out.push((pk.to_vec(), Surrogate::new(v.value())));
            }
        }
        Ok(out)
    }

    /// Bulk-delete every surrogate binding for a `(database_id, collection)` pair.
    /// Drains both forward and reverse tables. Idempotent.
    pub fn delete_all_surrogates_for_collection(
        &self,
        database_id: DatabaseId,
        collection: &str,
    ) -> crate::Result<()> {
        let to_remove = self.scan_surrogates_for_collection(database_id, collection)?;
        if to_remove.is_empty() {
            return Ok(());
        }
        let db_id = database_id.as_u64();
        let txn = self
            .db
            .begin_write()
            .map_err(|e| catalog_err("surrogate_pk bulk-delete txn", e))?;
        {
            let mut fwd = txn
                .open_table(SURROGATE_PK)
                .map_err(|e| catalog_err("open surrogate_pk", e))?;
            let mut rev = txn
                .open_table(SURROGATE_PK_REV)
                .map_err(|e| catalog_err("open surrogate_pk_rev", e))?;
            for (pk, surrogate) in &to_remove {
                fwd.remove((db_id, collection, pk.as_slice()))
                    .map_err(|e| catalog_err("bulk remove surrogate_pk", e))?;
                rev.remove((db_id, collection, surrogate.as_u32()))
                    .map_err(|e| catalog_err("bulk remove surrogate_pk_rev", e))?;
            }
        }
        txn.commit()
            .map_err(|e| catalog_err("surrogate_pk bulk-delete commit", e))
    }

    /// Idempotent migration: reads all rows from the legacy
    /// `_system.surrogate_pk` / `_system.surrogate_pk_rev` tables (bare
    /// `(collection, pk_bytes)` / `(collection, surrogate)` keys) and
    /// rewrites them under the v2 tables with `DatabaseId::DEFAULT` prepended.
    ///
    /// Skips if the v2 forward table is already non-empty (already-migrated
    /// boot). Safe to call on fresh boot (legacy table absent → no-op).
    pub fn migrate_surrogate_pk(&self) -> crate::Result<()> {
        // Gather legacy rows.
        let legacy_fwd: Vec<(String, Vec<u8>, u32)> = {
            let txn = self
                .db
                .begin_read()
                .map_err(|e| catalog_err("migrate_surrogate_pk read txn", e))?;
            match txn.open_table(SURROGATE_PK_LEGACY) {
                Ok(table) => {
                    let iter = table
                        .iter()
                        .map_err(|e| catalog_err("migrate_surrogate_pk iter", e))?;
                    let mut rows = Vec::new();
                    for row in iter {
                        let (k, v) = row.map_err(|e| catalog_err("migrate_surrogate_pk row", e))?;
                        let (coll, pk) = k.value();
                        rows.push((coll.to_string(), pk.to_vec(), v.value()));
                    }
                    rows
                }
                Err(_) => Vec::new(),
            }
        };

        if legacy_fwd.is_empty() {
            return Ok(());
        }

        // Skip if v2 already populated.
        let v2_empty = {
            let txn = self
                .db
                .begin_read()
                .map_err(|e| catalog_err("migrate_surrogate_pk v2 check txn", e))?;
            match txn.open_table(SURROGATE_PK) {
                Ok(table) => table
                    .is_empty()
                    .map_err(|e| catalog_err("migrate_surrogate_pk v2 is_empty", e))?,
                Err(_) => true,
            }
        };
        if !v2_empty {
            return Ok(());
        }

        let db_id = DatabaseId::DEFAULT.as_u64();
        let txn = self
            .db
            .begin_write()
            .map_err(|e| catalog_err("migrate_surrogate_pk write txn", e))?;
        {
            let mut fwd = txn
                .open_table(SURROGATE_PK)
                .map_err(|e| catalog_err("migrate_surrogate_pk open fwd v2", e))?;
            let mut rev = txn
                .open_table(SURROGATE_PK_REV)
                .map_err(|e| catalog_err("migrate_surrogate_pk open rev v2", e))?;
            for (coll, pk, surrogate_u32) in &legacy_fwd {
                fwd.insert((db_id, coll.as_str(), pk.as_slice()), *surrogate_u32)
                    .map_err(|e| catalog_err("migrate_surrogate_pk insert fwd", e))?;
                rev.insert((db_id, coll.as_str(), *surrogate_u32), pk.as_slice())
                    .map_err(|e| catalog_err("migrate_surrogate_pk insert rev", e))?;
            }
        }
        txn.commit()
            .map_err(|e| catalog_err("migrate_surrogate_pk commit", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::security::catalog::types::{SURROGATE_PK_LEGACY, SURROGATE_PK_REV_LEGACY};

    fn open_catalog() -> (tempfile::TempDir, SystemCatalog) {
        let dir = tempfile::tempdir().unwrap();
        let cat = SystemCatalog::open(&dir.path().join("system.redb")).unwrap();
        (dir, cat)
    }

    #[test]
    fn put_then_get_roundtrip() {
        let (_dir, cat) = open_catalog();
        cat.put_surrogate(DatabaseId::DEFAULT, "users", b"alice", Surrogate::new(7))
            .unwrap();
        assert_eq!(
            cat.get_surrogate_for_pk(DatabaseId::DEFAULT, "users", b"alice")
                .unwrap(),
            Some(Surrogate::new(7))
        );
        assert_eq!(
            cat.get_pk_for_surrogate(DatabaseId::DEFAULT, "users", Surrogate::new(7))
                .unwrap(),
            Some(b"alice".to_vec())
        );
    }

    #[test]
    fn missing_returns_none() {
        let (_dir, cat) = open_catalog();
        assert_eq!(
            cat.get_surrogate_for_pk(DatabaseId::DEFAULT, "users", b"nobody")
                .unwrap(),
            None
        );
    }

    #[test]
    fn delete_is_idempotent_and_removes_both_directions() {
        let (_dir, cat) = open_catalog();
        cat.put_surrogate(DatabaseId::DEFAULT, "users", b"alice", Surrogate::new(7))
            .unwrap();
        cat.delete_surrogate(DatabaseId::DEFAULT, "users", b"alice")
            .unwrap();
        assert_eq!(
            cat.get_surrogate_for_pk(DatabaseId::DEFAULT, "users", b"alice")
                .unwrap(),
            None
        );
        cat.delete_surrogate(DatabaseId::DEFAULT, "users", b"alice")
            .unwrap();
    }

    #[test]
    fn scan_returns_only_named_collection() {
        let (_dir, cat) = open_catalog();
        cat.put_surrogate(DatabaseId::DEFAULT, "users", b"alice", Surrogate::new(1))
            .unwrap();
        cat.put_surrogate(DatabaseId::DEFAULT, "users", b"bob", Surrogate::new(2))
            .unwrap();
        cat.put_surrogate(DatabaseId::DEFAULT, "orders", b"alice", Surrogate::new(3))
            .unwrap();
        let mut got = cat
            .scan_surrogates_for_collection(DatabaseId::DEFAULT, "users")
            .unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![
                (b"alice".to_vec(), Surrogate::new(1)),
                (b"bob".to_vec(), Surrogate::new(2)),
            ]
        );
    }

    #[test]
    fn delete_all_wipes_collection_and_leaves_others_intact() {
        let (_dir, cat) = open_catalog();
        cat.put_surrogate(DatabaseId::DEFAULT, "users", b"alice", Surrogate::new(1))
            .unwrap();
        cat.put_surrogate(DatabaseId::DEFAULT, "orders", b"o1", Surrogate::new(2))
            .unwrap();
        cat.delete_all_surrogates_for_collection(DatabaseId::DEFAULT, "users")
            .unwrap();
        assert!(
            cat.scan_surrogates_for_collection(DatabaseId::DEFAULT, "users")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            cat.get_surrogate_for_pk(DatabaseId::DEFAULT, "orders", b"o1")
                .unwrap(),
            Some(Surrogate::new(2))
        );
        // double-delete is a no-op
        cat.delete_all_surrogates_for_collection(DatabaseId::DEFAULT, "users")
            .unwrap();
    }

    // ── Migration tests ───────────────────────────────────────────────────

    fn insert_legacy_fwd(cat: &SystemCatalog, coll: &str, pk: &[u8], surrogate: u32) {
        let txn = cat.db.begin_write().unwrap();
        {
            let mut t = txn.open_table(SURROGATE_PK_LEGACY).unwrap();
            t.insert((coll, pk), surrogate).unwrap();
            let mut r = txn.open_table(SURROGATE_PK_REV_LEGACY).unwrap();
            r.insert((coll, surrogate), pk).unwrap();
        }
        txn.commit().unwrap();
    }

    #[test]
    fn fresh_boot_migration_is_noop() {
        let (_dir, cat) = open_catalog();
        cat.migrate_surrogate_pk().unwrap();
        assert!(
            cat.scan_surrogates_for_collection(DatabaseId::DEFAULT, "users")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn pre_migration_boot_migrates_rows() {
        let (_dir, cat) = open_catalog();
        insert_legacy_fwd(&cat, "users", b"alice", 7);
        cat.migrate_surrogate_pk().unwrap();
        assert_eq!(
            cat.get_surrogate_for_pk(DatabaseId::DEFAULT, "users", b"alice")
                .unwrap(),
            Some(Surrogate::new(7))
        );
        assert_eq!(
            cat.get_pk_for_surrogate(DatabaseId::DEFAULT, "users", Surrogate::new(7))
                .unwrap(),
            Some(b"alice".to_vec())
        );
    }

    #[test]
    fn already_migrated_boot_is_idempotent() {
        let (_dir, cat) = open_catalog();
        // v2 row already exists
        cat.put_surrogate(DatabaseId::DEFAULT, "users", b"alice", Surrogate::new(7))
            .unwrap();
        // also insert a legacy row
        insert_legacy_fwd(&cat, "users", b"alice", 7);
        // migration should be a no-op
        cat.migrate_surrogate_pk().unwrap();
        // still only one row
        let rows = cat
            .scan_surrogates_for_collection(DatabaseId::DEFAULT, "users")
            .unwrap();
        assert_eq!(rows.len(), 1);
    }
}
