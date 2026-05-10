// SPDX-License-Identifier: BUSL-1.1

//! End-to-end test that `NodeDb::text_search` returns real BM25-ranked
//! matches against indexed text content.
//!
//! A trait default that short-circuits to `Ok(Vec::new())` without
//! ever reaching the wire is the silent-wrong pattern this test guards
//! against — a fake "no matches" answer is indistinguishable from a
//! real one and lets callers proceed as if FTS were working.

use nodedb_client::{NodeDb, NodeDbRemote};
use nodedb_test_support::pgwire_harness::TestServer;
use nodedb_types::text_search::TextSearchParams;

#[tokio::test]
async fn text_search_returns_real_matches() {
    let server = TestServer::start().await;
    let conn_str = format!(
        "host=127.0.0.1 port={} user=nodedb dbname=nodedb",
        server.pg_port
    );
    let remote = NodeDbRemote::connect(&conn_str)
        .await
        .expect("pgwire connect to harness must succeed");

    // Spec: with indexed content matching the query, `text_search`
    // returns Ok with at least one BM25-ranked hit.
    //
    // Stays RED until the trait API resolves the field-naming gap (a
    // `field` parameter on `text_search`, or a `text_search_field`
    // parallel method, or a server-side `SEARCH ... USING TEXT(...)`
    // DSL that auto-picks the FTS-indexed field). An
    // `Err("not implemented")` default is correct negative behavior
    // but not the spec — do not soften the assertion to accept `Err`;
    // that locks the gap in as the contract.
    let mut doc = nodedb_client::Document::new("d1");
    doc.set(
        "body",
        nodedb_client::Value::String("machine learning is everywhere".into()),
    );
    let _ = remote
        .execute_sql("CREATE COLLECTION docs TYPE document", &[])
        .await;
    let _ = remote.document_put("docs", doc).await;

    let matches = remote
        .text_search("docs", "machine learning", 10, TextSearchParams::default())
        .await
        .expect("text_search must return Ok with real matches against indexed content");
    assert!(
        !matches.is_empty(),
        "text_search must return real BM25-ranked matches; got empty"
    );

    server.graceful_shutdown().await;
}
