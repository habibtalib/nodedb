// SPDX-License-Identifier: BUSL-1.1

//! Restart-resume for mirror databases.
//!
//! On server start, each database with `mirror_origin.is_some()` and
//! `status != Promoted` must have its cross-cluster observer link
//! re-established from the persisted `last_applied` LSN. This avoids
//! fetching a fresh snapshot when the server simply rebooted — the source
//! will stream entries from `last_applied + 1` onward.
//!
//! A `Promoted` mirror is a normal writable database; it must NOT
//! attempt to reconnect.
//!
//! Recovery uses the `MirrorLagRecord` from `_system.mirror_lag` for the
//! precise LSN watermark. If no lag record exists the descriptor's
//! `last_applied` field is used. If that is also zero the mirror restarts
//! from bootstrap (LSN 0 triggers a fresh snapshot on the source side).

use nodedb_types::MirrorStatus;
use tracing::{info, warn};

use crate::control::security::catalog::SystemCatalog;

/// Summary of one database's restart decision.
#[derive(Debug)]
pub struct MirrorRestartDecision {
    pub database_name: String,
    pub resume_from_lsn: u64,
    pub needs_bootstrap: bool,
}

/// Enumerate databases in `catalog` that need their observer link restarted.
///
/// Returns a list of databases (by name + resume LSN) that are mirrors but
/// not yet promoted. Databases with `MirrorStatus::Promoted` are excluded —
/// they are normal writable databases and must not attempt to reconnect.
pub fn enumerate_resumable_mirrors(
    catalog: &SystemCatalog,
) -> crate::Result<Vec<MirrorRestartDecision>> {
    let databases = catalog.list_databases()?;
    let mut decisions = Vec::new();

    for db in databases {
        let origin = match db.mirror_origin {
            Some(ref o) => o,
            None => continue,
        };

        // Promoted mirrors are normal writable databases — skip.
        if matches!(origin.status, MirrorStatus::Promoted) {
            info!(
                database = %db.name,
                "mirror restart: database is Promoted — skipping observer reconnect"
            );
            continue;
        }

        // Determine the precise resume LSN: prefer the lag record (written
        // atomically with every DDL apply) over the descriptor's field
        // (updated only at bootstrap completion and PROMOTE).
        let resume_lsn = match catalog.get_mirror_lag(db.id) {
            Ok(Some(lag)) => lag.last_applied_lsn.as_u64(),
            Ok(None) => origin.last_applied.as_u64(),
            Err(e) => {
                warn!(
                    database = %db.name,
                    error = %e,
                    "mirror restart: failed to read mirror_lag; falling back to descriptor LSN"
                );
                origin.last_applied.as_u64()
            }
        };

        // LSN 0 means no entries have ever been applied — a fresh snapshot
        // is required from the source. The bootstrap receiver handles this
        // automatically: when the mirror connects with LSN 0 the source
        // streams a full snapshot.
        let needs_bootstrap = resume_lsn == 0;

        decisions.push(MirrorRestartDecision {
            database_name: db.name.clone(),
            resume_from_lsn: resume_lsn,
            needs_bootstrap,
        });

        info!(
            database = %db.name,
            resume_lsn,
            needs_bootstrap,
            status = ?origin.status,
            "mirror restart: scheduling observer resume"
        );
    }

    Ok(decisions)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use nodedb_types::{DatabaseId, Lsn, MirrorLagRecord, MirrorMode, MirrorOrigin, MirrorStatus};
    use tempfile::TempDir;

    use super::*;
    use crate::control::security::catalog::SystemCatalog;
    use crate::control::security::catalog::database_types::{DatabaseDescriptor, DatabaseStatus};

    fn open_tmp_catalog(tmp: &TempDir) -> SystemCatalog {
        let path: PathBuf = tmp.path().join("system.redb");
        SystemCatalog::open(&path).expect("open catalog")
    }

    fn make_mirror_db(
        id: u64,
        name: &str,
        status: MirrorStatus,
        last_applied: Lsn,
    ) -> DatabaseDescriptor {
        DatabaseDescriptor {
            id: DatabaseId::new(id),
            name: name.to_string(),
            status: DatabaseStatus::Mirroring,
            created_at_lsn: 0,
            quota_ref: 0,
            parent_clone: None,
            mirror_origin: Some(MirrorOrigin {
                source_cluster: "prod-us".to_string(),
                source_database: DatabaseId::new(0),
                mode: MirrorMode::Async,
                last_applied,
                status,
            }),
        }
    }

    #[test]
    fn promoted_mirror_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let catalog = open_tmp_catalog(&tmp);
        let db_id = DatabaseId::new(100);
        let db = make_mirror_db(
            db_id.as_u64(),
            "promoted_db",
            MirrorStatus::Promoted,
            Lsn::new(50),
        );
        catalog.put_database(&db).unwrap();

        let decisions = enumerate_resumable_mirrors(&catalog).unwrap();
        assert!(
            !decisions.iter().any(|d| d.database_name == "promoted_db"),
            "promoted mirror must be excluded from restart"
        );
    }

    #[test]
    fn following_mirror_included_with_lag_lsn() {
        let tmp = TempDir::new().unwrap();
        let catalog = open_tmp_catalog(&tmp);
        let db_id = DatabaseId::new(101);
        let db = make_mirror_db(
            db_id.as_u64(),
            "follower_db",
            MirrorStatus::Following,
            Lsn::new(10),
        );
        catalog.put_database(&db).unwrap();

        // Write a lag record with a higher LSN than the descriptor.
        catalog
            .put_mirror_lag(
                db_id,
                &MirrorLagRecord {
                    last_applied_lsn: Lsn::new(99),
                    last_apply_ms: 1_000,
                },
            )
            .unwrap();

        let decisions = enumerate_resumable_mirrors(&catalog).unwrap();
        let d = decisions
            .iter()
            .find(|d| d.database_name == "follower_db")
            .unwrap();
        assert_eq!(d.resume_from_lsn, 99, "should use lag record LSN");
        assert!(!d.needs_bootstrap);
    }

    #[test]
    fn mirror_with_no_lag_record_falls_back_to_descriptor_lsn() {
        let tmp = TempDir::new().unwrap();
        let catalog = open_tmp_catalog(&tmp);
        let db_id = DatabaseId::new(102);
        let db = make_mirror_db(
            db_id.as_u64(),
            "no_lag_db",
            MirrorStatus::Following,
            Lsn::new(42),
        );
        catalog.put_database(&db).unwrap();
        // No lag record written.

        let decisions = enumerate_resumable_mirrors(&catalog).unwrap();
        let d = decisions
            .iter()
            .find(|d| d.database_name == "no_lag_db")
            .unwrap();
        assert_eq!(d.resume_from_lsn, 42);
        assert!(!d.needs_bootstrap);
    }

    #[test]
    fn mirror_with_zero_lsn_needs_bootstrap() {
        let tmp = TempDir::new().unwrap();
        let catalog = open_tmp_catalog(&tmp);
        let db_id = DatabaseId::new(103);
        let db = make_mirror_db(
            db_id.as_u64(),
            "fresh_db",
            MirrorStatus::Bootstrapping {
                bytes_done: 0,
                bytes_total: 0,
            },
            Lsn::new(0),
        );
        catalog.put_database(&db).unwrap();

        let decisions = enumerate_resumable_mirrors(&catalog).unwrap();
        let d = decisions
            .iter()
            .find(|d| d.database_name == "fresh_db")
            .unwrap();
        assert_eq!(d.resume_from_lsn, 0);
        assert!(d.needs_bootstrap);
    }
}
