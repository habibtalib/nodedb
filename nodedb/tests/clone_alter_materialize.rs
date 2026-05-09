// SPDX-License-Identifier: BUSL-1.1

//! `ALTER DATABASE clone MATERIALIZE` test.
//!
//! Real source-to-target row copy is not yet implemented, so MATERIALIZE is
//! gated behind SQLSTATE `0A000` (`feature_not_supported`). When per-engine
//! bulk copy lands, this test should be updated to assert that:
//!   1. The command succeeds.
//!   2. Every source row is readable from the clone after the call returns.
//!   3. `clone_status == Materialized` and CoW auxiliary tables are reaped.

mod common;

use common::pgwire_harness::TestServer;

/// Until per-engine row copy is wired, `ALTER DATABASE … MATERIALIZE` must
/// fail with SQLSTATE `0A000` rather than silently flip status (which would
/// stop source delegation and lose every source row not yet copy-up'd).
#[tokio::test]
async fn alter_database_materialize_is_gated_until_real_impl() {
    let server = TestServer::start().await;
    let client = &*server.client;

    client
        .simple_query("CREATE DATABASE alter_src")
        .await
        .expect("CREATE DATABASE alter_src");
    client
        .simple_query("USE DATABASE alter_src")
        .await
        .expect("USE alter_src");
    client
        .simple_query(
            "CREATE COLLECTION events \
             (key STRING PRIMARY KEY, payload STRING) \
             WITH (engine='kv')",
        )
        .await
        .expect("CREATE COLLECTION events");
    for i in 0..20u32 {
        client
            .simple_query(&format!(
                "INSERT INTO events (key, payload) VALUES ('e{i}', 'data{i}')"
            ))
            .await
            .unwrap_or_else(|e| panic!("INSERT e{i}: {e}"));
    }

    client
        .simple_query("USE DATABASE default")
        .await
        .expect("USE default");
    client
        .simple_query("CLONE DATABASE alter_clone FROM alter_src")
        .await
        .expect("CLONE DATABASE");

    let err = client
        .simple_query("ALTER DATABASE alter_clone MATERIALIZE")
        .await
        .expect_err("MATERIALIZE must error until row copy is implemented");

    let msg = err.to_string();
    assert!(
        msg.contains("0A000") || msg.contains("not yet implemented"),
        "expected 0A000 / not-yet-implemented error, got: {msg}"
    );

    // The CoW shadow read path must still work — the clone is fully usable
    // for reads/writes; only the destructive MATERIALIZE flip is gated.
    client
        .simple_query("USE DATABASE alter_clone")
        .await
        .expect("USE alter_clone");
    client
        .simple_query("SELECT key FROM events")
        .await
        .expect("SELECT on shadowed clone must keep working");
}
