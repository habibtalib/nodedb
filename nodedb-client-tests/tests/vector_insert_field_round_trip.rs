// SPDX-License-Identifier: BUSL-1.1

//! End-to-end test that `NodeDb::vector_insert_field` honors the
//! `field_name` argument across the full pgwire round-trip.
//!
//! A trait default that delegates to `vector_insert` and discards
//! `field_name` lands the vector in the wrong index — the silent-
//! wrong pattern this test guards against.

use nodedb_client::{NodeDb, NodeDbRemote};
use nodedb_test_support::pgwire_harness::TestServer;

#[tokio::test]
async fn vector_insert_field_must_not_silently_delegate_to_unfielded() {
    let server = TestServer::start().await;
    let conn_str = format!(
        "host=127.0.0.1 port={} user=nodedb dbname=nodedb",
        server.pg_port
    );
    let remote = NodeDbRemote::connect(&conn_str)
        .await
        .expect("pgwire connect to harness must succeed");

    // Spec: `vector_insert_field("body_embedding", ...)` lands the
    // vector in the named HNSW index on the collection — not in the
    // default unfielded vector index.
    //
    // Stays RED until: (a) the harness provisions a collection with
    // multiple named vector fields, AND (b) the trait default or
    // NodeDbRemote override emits SQL/DSL that targets `field_name`.
    // An `Err("not implemented")` default is correct negative behavior
    // but not the spec — do not soften the assertion to accept `Err`;
    // that locks the broken default in as the contract.
    remote
        .vector_insert_field(
            "embeddings_multi",
            "body_embedding",
            "v1",
            &[0.1, 0.2, 0.3],
            None,
        )
        .await
        .expect("vector_insert_field must land the vector in the named field");

    server.graceful_shutdown().await;
}
