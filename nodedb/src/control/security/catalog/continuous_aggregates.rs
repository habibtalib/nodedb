// SPDX-License-Identifier: BUSL-1.1

//! Continuous-aggregate metadata operations for the system catalog.

use super::types::{CONTINUOUS_AGGREGATES, StoredContinuousAggregate, SystemCatalog, catalog_err};

impl SystemCatalog {
    /// Store a continuous-aggregate record.
    pub fn put_continuous_aggregate(&self, cagg: &StoredContinuousAggregate) -> crate::Result<()> {
        let key = format!("{}:{}", cagg.tenant_id, cagg.name);
        let bytes = zerompk::to_msgpack_vec(cagg)
            .map_err(|e| catalog_err("serialize continuous aggregate", e))?;
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| catalog_err("write txn", e))?;
        {
            let mut table = write_txn
                .open_table(CONTINUOUS_AGGREGATES)
                .map_err(|e| catalog_err("open continuous_aggregates", e))?;
            table
                .insert(key.as_str(), bytes.as_slice())
                .map_err(|e| catalog_err("insert continuous_aggregate", e))?;
        }
        write_txn.commit().map_err(|e| catalog_err("commit", e))
    }

    /// Get a continuous aggregate by name.
    pub fn get_continuous_aggregate(
        &self,
        tenant_id: u64,
        name: &str,
    ) -> crate::Result<Option<StoredContinuousAggregate>> {
        let key = format!("{tenant_id}:{name}");
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| catalog_err("read txn", e))?;
        let table = read_txn
            .open_table(CONTINUOUS_AGGREGATES)
            .map_err(|e| catalog_err("open continuous_aggregates", e))?;
        match table.get(key.as_str()) {
            Ok(Some(guard)) => {
                let bytes = guard.value();
                let cagg: StoredContinuousAggregate = zerompk::from_msgpack(bytes)
                    .map_err(|e| catalog_err("deserialize continuous aggregate", e))?;
                Ok(Some(cagg))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(catalog_err("get continuous_aggregate", e)),
        }
    }

    /// Load every continuous aggregate across all tenants. Used by
    /// startup replay (re-register each definition on the local Data
    /// Plane) and by the integrity verifier.
    pub fn load_all_continuous_aggregates(&self) -> crate::Result<Vec<StoredContinuousAggregate>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| catalog_err("read txn", e))?;
        let table = read_txn
            .open_table(CONTINUOUS_AGGREGATES)
            .map_err(|e| catalog_err("open continuous_aggregates", e))?;
        let mut caggs = Vec::new();
        for entry in table
            .range::<&str>(..)
            .map_err(|e| catalog_err("range scan", e))?
        {
            let (_key, val) = entry.map_err(|e| catalog_err("read entry", e))?;
            let cagg: StoredContinuousAggregate = zerompk::from_msgpack(val.value())
                .map_err(|e| catalog_err("deser continuous_aggregate", e))?;
            caggs.push(cagg);
        }
        Ok(caggs)
    }

    /// List continuous aggregates for a single tenant.
    pub fn list_continuous_aggregates(
        &self,
        tenant_id: u64,
    ) -> crate::Result<Vec<StoredContinuousAggregate>> {
        let prefix = format!("{tenant_id}:");
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| catalog_err("read txn", e))?;
        let table = read_txn
            .open_table(CONTINUOUS_AGGREGATES)
            .map_err(|e| catalog_err("open continuous_aggregates", e))?;
        let mut caggs = Vec::new();
        for entry in table
            .range::<&str>(..)
            .map_err(|e| catalog_err("range scan", e))?
        {
            let (key, val) = entry.map_err(|e| catalog_err("read entry", e))?;
            if key.value().starts_with(&prefix)
                && let Ok(cagg) = zerompk::from_msgpack::<StoredContinuousAggregate>(val.value())
            {
                caggs.push(cagg);
            }
        }
        Ok(caggs)
    }

    /// Delete a continuous aggregate by name.
    pub fn delete_continuous_aggregate(&self, tenant_id: u64, name: &str) -> crate::Result<()> {
        let key = format!("{tenant_id}:{name}");
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| catalog_err("write txn", e))?;
        {
            let mut table = write_txn
                .open_table(CONTINUOUS_AGGREGATES)
                .map_err(|e| catalog_err("open continuous_aggregates", e))?;
            table
                .remove(key.as_str())
                .map_err(|e| catalog_err("delete continuous_aggregate", e))?;
        }
        write_txn.commit().map_err(|e| catalog_err("commit", e))
    }
}
