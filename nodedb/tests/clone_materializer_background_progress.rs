// SPDX-License-Identifier: BUSL-1.1

//! Background materializer sweep integration test.
//!
//! Until per-engine row copy lands, the background sweep is a no-op for
//! clones that need materialization: it must NOT advance status (which would
//! lose data) and it must NOT return an error (the periodic timer would spam
//! every tick). It must log at `info` level and leave the clone in
//! `Shadowed | Materializing` — fully usable through the CoW read path.
//!
//! Once real row copy is implemented, this test should be updated to assert
//! that the sweep eventually drives every clone to `Materialized` and that
//! all source rows are readable.

mod common;

use std::sync::atomic::AtomicBool;

use common::pgwire_harness::TestServer;
use nodedb::control::maintenance::clone_materializer::run_scheduled_sweep;
use nodedb_types::CloneStatus;

#[tokio::test]
async fn background_sweep_is_safe_no_op_when_row_copy_unimplemented() {
    let server = TestServer::start().await;
    let client = &*server.client;

    client
        .simple_query("CREATE DATABASE bg_sweep_src")
        .await
        .expect("CREATE DATABASE bg_sweep_src");
    client
        .simple_query("USE DATABASE bg_sweep_src")
        .await
        .expect("USE bg_sweep_src");
    client
        .simple_query(
            "CREATE COLLECTION records \
             (key STRING PRIMARY KEY, val STRING) \
             WITH (engine='kv')",
        )
        .await
        .expect("CREATE COLLECTION records");
    for i in 0..50u32 {
        client
            .simple_query(&format!(
                "INSERT INTO records (key, val) VALUES ('k{i}', 'v{i}')"
            ))
            .await
            .unwrap_or_else(|e| panic!("INSERT k{i}: {e}"));
    }

    client
        .simple_query("USE DATABASE default")
        .await
        .expect("USE default");
    client
        .simple_query("CLONE DATABASE bg_sweep_clone FROM bg_sweep_src")
        .await
        .expect("CLONE DATABASE");

    // Sweep must not error.
    let cancel = AtomicBool::new(false);
    let catalog = server
        .shared
        .credentials
        .catalog()
        .as_ref()
        .expect("system catalog");
    run_scheduled_sweep(&server.shared, catalog, &cancel)
        .expect("run_scheduled_sweep must not error in gated mode");

    // Clone collections must remain `Shadowed` (the sweep refused to advance
    // status because real row copy is not implemented). This is the data-loss
    // protection: a half-implemented sweep would have flipped them to
    // `Materialized`, terminating source delegation and losing 50 rows.
    let db_id = catalog
        .get_database_id_by_name("bg_sweep_clone")
        .expect("catalog lookup")
        .expect("bg_sweep_clone must exist");
    let colls = catalog
        .load_all_collections(db_id)
        .expect("load collections");
    assert!(
        !colls.is_empty(),
        "bg_sweep_clone must have collections after sweep"
    );
    for coll in &colls {
        assert!(
            matches!(
                coll.clone_status,
                CloneStatus::Shadowed | CloneStatus::Materializing { .. }
            ),
            "collection '{}' must remain Shadowed/Materializing while row copy is gated, got {:?}",
            coll.name,
            coll.clone_status
        );
    }

    // The CoW read path must keep working — all 50 source rows visible.
    client
        .simple_query("USE DATABASE bg_sweep_clone")
        .await
        .expect("USE bg_sweep_clone");

    let rows = client
        .simple_query("SELECT key FROM records")
        .await
        .expect("SELECT from bg_sweep_clone must not error");

    let data_count = rows
        .iter()
        .filter(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)))
        .count();
    assert_eq!(
        data_count, 50,
        "shadowed clone must serve all 50 source rows via CoW delegation, got {data_count}"
    );
}
