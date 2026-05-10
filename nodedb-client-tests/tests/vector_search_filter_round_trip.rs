// SPDX-License-Identifier: BUSL-1.1

//! End-to-end test that `NodeDbRemote::vector_search` honors a non-None
//! `MetadataFilter` argument across the full pgwire round-trip.
//!
//! Today's client rejects any non-None filter at the trait method
//! boundary (`build_vector_search_sql` returns Err). This test asserts
//! the spec — a filter must round-trip to the server, narrow results,
//! and come back as a typed `Vec<SearchResult>` — and therefore fails
//! until the fix replaces the client-side rejection with real predicate
//! rendering. The unit-level tests in
//! `nodedb-client/src/remote/sql.rs::tests` lock in the SQL-shape spec;
//! this test pins the wire round-trip.

use nodedb_client::{MetadataFilter, NodeDb, NodeDbRemote, Value};
use nodedb_test_support::pgwire_harness::TestServer;

#[tokio::test]
async fn vector_search_with_metadata_filter_round_trips_through_pgwire() {
    let server = TestServer::start().await;

    // Connect a NodeDbRemote to the harness's pgwire port. The harness
    // already provisions the `nodedb` superuser and the `default`
    // database so a Trust-mode connect succeeds without further setup.
    let conn_str = format!(
        "host=127.0.0.1 port={} user=nodedb dbname=nodedb",
        server.pg_port
    );
    let remote = NodeDbRemote::connect(&conn_str)
        .await
        .expect("pgwire connect to harness must succeed");

    // Spec: vector_search with a non-None filter renders into a
    // server-side predicate, the server applies it, and the call
    // returns matching results — Ok with real rows.
    //
    // Will stay RED until: (a) the harness provisions the `embeddings`
    // collection with vector + metadata columns AND (b) the server-side
    // planner accepts the WHERE-on-metadata predicate the client now
    // emits. Both are server / harness work outside this client fix.
    // The test stays here as the spec; do not soften the assertion to
    // make it green — that would re-create the silent-wrong pattern.
    let filter = MetadataFilter::eq("category", Value::String("ai".into()));
    let results = remote
        .vector_search("embeddings", &[0.1, 0.2, 0.3], 5, Some(&filter))
        .await
        .expect("vector_search with filter must return Ok end-to-end");
    let _ = results;

    server.graceful_shutdown().await;
}
