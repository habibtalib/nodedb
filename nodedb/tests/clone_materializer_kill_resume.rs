// SPDX-License-Identifier: BUSL-1.1

//! Materializer kill-and-resume test.
//!
//! Until per-engine row copy lands, MATERIALIZE returns SQLSTATE `0A000` and
//! the clone status remains `Shadowed`. The kill-and-resume property to
//! verify pre-impl is therefore: an aborted MATERIALIZE leaves the clone in
//! a usable shadowed state — no half-flip, no data loss. After per-engine
//! copy lands this test should be replaced with a real seed-clone-materialize-
//! verify-rows-after-restart scenario.

mod common;

use common::pgwire_harness::TestServer;

#[tokio::test]
async fn materialize_attempt_leaves_clone_in_shadowed_state() {
    let server = TestServer::start().await;
    let client = &*server.client;

    client
        .simple_query("CREATE DATABASE mat_src")
        .await
        .expect("CREATE DATABASE mat_src");
    client
        .simple_query("USE DATABASE mat_src")
        .await
        .expect("USE mat_src");
    client
        .simple_query(
            "CREATE COLLECTION items \
             (key STRING PRIMARY KEY, value STRING) \
             WITH (engine='kv')",
        )
        .await
        .expect("CREATE COLLECTION items");

    for i in 0..100u32 {
        client
            .simple_query(&format!(
                "INSERT INTO items (key, value) VALUES ('k{i}', 'v{i}')"
            ))
            .await
            .unwrap_or_else(|e| panic!("INSERT k{i}: {e}"));
    }

    client
        .simple_query("USE DATABASE default")
        .await
        .expect("USE default");
    client
        .simple_query("CLONE DATABASE mat_clone FROM mat_src")
        .await
        .expect("CLONE DATABASE");

    let err = client
        .simple_query("ALTER DATABASE mat_clone MATERIALIZE")
        .await
        .expect_err("MATERIALIZE must error until row copy is implemented");
    assert!(
        err.to_string().contains("0A000") || err.to_string().contains("not yet implemented"),
        "expected 0A000 / not-yet-implemented error, got: {err}"
    );

    // After the gated error, the clone must still be readable through the
    // CoW shadow read path — source delegation must NOT have been disabled.
    client
        .simple_query("USE DATABASE mat_clone")
        .await
        .expect("USE mat_clone");

    let rows = client
        .simple_query("SELECT key FROM items")
        .await
        .expect("SELECT from mat_clone");

    let data_rows: Vec<_> = rows
        .iter()
        .filter_map(|m| {
            if let tokio_postgres::SimpleQueryMessage::Row(r) = m {
                Some(r.get("key").unwrap_or("").to_string())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        data_rows.len(),
        100,
        "shadowed clone must still serve all 100 source rows after a gated \
         MATERIALIZE attempt; got {}",
        data_rows.len()
    );

    // A second MATERIALIZE attempt must produce the same gated error
    // (idempotent failure mode).
    client
        .simple_query("USE DATABASE default")
        .await
        .expect("USE default");
    let err2 = client
        .simple_query("ALTER DATABASE mat_clone MATERIALIZE")
        .await
        .expect_err("second MATERIALIZE must also be gated");
    assert!(
        err2.to_string().contains("0A000") || err2.to_string().contains("not yet implemented"),
        "expected 0A000 / not-yet-implemented error on retry, got: {err2}"
    );
}
