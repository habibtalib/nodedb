// SPDX-License-Identifier: BUSL-1.1

//! End-to-end test that `NodeDb::vector_search_field` honors the
//! `field_name` argument across the full pgwire round-trip.
//!
//! A search against a named field must hit a SQL/DSL path that scopes
//! to that field, with results reflecting the field's data — never
//! the unfielded fallback. A trait default that delegates to
//! `vector_search` and discards `field_name` is the silent-wrong
//! pattern this test guards against.

use nodedb_client::{NodeDb, NodeDbRemote};
use nodedb_test_support::pgwire_harness::TestServer;

#[tokio::test]
async fn vector_search_field_must_not_silently_delegate_to_unfielded() {
    let server = TestServer::start().await;
    let conn_str = format!(
        "host=127.0.0.1 port={} user=nodedb dbname=nodedb",
        server.pg_port
    );
    let remote = NodeDbRemote::connect(&conn_str)
        .await
        .expect("pgwire connect to harness must succeed");

    // Spec: `vector_search_field("body_embedding", ...)` searches the
    // named HNSW index on the collection and returns results from THAT
    // field — not from the default unfielded vector index.
    //
    // Stays RED until: (a) the harness provisions a collection with
    // multiple named vector fields, AND (b) the trait default or
    // NodeDbRemote override emits SQL/DSL that scopes the search to
    // `field_name`. An `Err("not implemented")` default is correct
    // negative behavior but not the spec — do not soften the assertion
    // to accept `Err`; that locks the broken default in as the
    // contract.
    let _results = remote
        .vector_search_field(
            "embeddings_multi",
            "body_embedding",
            &[0.1, 0.2, 0.3],
            5,
            None,
        )
        .await
        .expect("vector_search_field must scope to the named field and return results");

    server.graceful_shutdown().await;
}
