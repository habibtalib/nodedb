// SPDX-License-Identifier: BUSL-1.1

//! `DROP DATABASE source FORCE` orphan-protection test.
//!
//! With dependent clones present, FORCE must materialize them before
//! dropping the source. Until per-engine row copy lands, materialization is
//! gated, so FORCE itself is gated and surfaces SQLSTATE `0A000`
//! (`feature_not_supported`). The source database must remain intact when
//! the drop is rejected — half-dropping a source whose dependents cannot be
//! materialized would orphan them.
//!
//! When per-engine row copy is implemented, this test should be updated to
//! assert that FORCE succeeds, the source is gone, and every source row is
//! readable from the clone.

mod common;

use common::pgwire_harness::TestServer;

#[tokio::test]
async fn drop_source_force_is_gated_when_clones_need_materialization() {
    let server = TestServer::start().await;
    let client = &*server.client;

    client
        .simple_query("CREATE DATABASE force_src")
        .await
        .expect("CREATE DATABASE force_src");
    client
        .simple_query("USE DATABASE force_src")
        .await
        .expect("USE force_src");
    client
        .simple_query(
            "CREATE COLLECTION records \
             (key STRING PRIMARY KEY, data STRING) \
             WITH (engine='kv')",
        )
        .await
        .expect("CREATE COLLECTION records");
    client
        .simple_query("INSERT INTO records (key, data) VALUES ('r1', 'hello')")
        .await
        .expect("INSERT r1");
    client
        .simple_query("INSERT INTO records (key, data) VALUES ('r2', 'world')")
        .await
        .expect("INSERT r2");

    client
        .simple_query("USE DATABASE default")
        .await
        .expect("USE default");
    client
        .simple_query("CLONE DATABASE force_clone FROM force_src")
        .await
        .expect("CLONE DATABASE");

    // FORCE must reject because materialization can't yet run.
    let err = client
        .simple_query("DROP DATABASE force_src FORCE")
        .await
        .expect_err("DROP FORCE must error until materialization is implemented");
    let msg = err.to_string();
    assert!(
        msg.contains("0A000") || msg.contains("not yet implemented"),
        "expected 0A000 / not-yet-implemented error, got: {msg}"
    );

    // Source must still be intact — no half-drop on materialization failure.
    client
        .simple_query("USE DATABASE force_src")
        .await
        .expect("force_src must still exist after gated FORCE drop");
    client
        .simple_query("SELECT key FROM records")
        .await
        .expect("force_src.records must still be queryable");
}
