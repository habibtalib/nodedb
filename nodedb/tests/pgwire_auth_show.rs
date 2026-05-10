// SPDX-License-Identifier: BUSL-1.1

//! SHOW commands (SHOW SESSION, SHOW GRANTS) over the DDL path.

mod common;

use common::pgwire_auth_helpers::{ddl_ok, make_state, superuser};
use nodedb::control::server::pgwire::ddl;

#[tokio::test]
async fn show_session() {
    let state = make_state();
    let su = superuser();
    let result = ddl::dispatch(
        &state,
        &su,
        "SHOW SESSION",
        nodedb_types::id::DatabaseId::DEFAULT,
    )
    .await
    .unwrap()
    .unwrap();

    match &result[0] {
        pgwire::api::results::Response::Query(_) => {}
        other => panic!("expected Query response, got: {other:?}"),
    }
}

#[tokio::test]
async fn show_grants() {
    let state = make_state();
    let su = superuser();
    ddl_ok(
        &state,
        &su,
        "CREATE USER judy WITH PASSWORD 'pass' ROLE readwrite",
    )
    .await;
    ddl_ok(&state, &su, "GRANT ROLE monitor TO judy").await;

    let result = ddl::dispatch(
        &state,
        &su,
        "SHOW GRANTS FOR judy",
        nodedb_types::id::DatabaseId::DEFAULT,
    )
    .await
    .unwrap()
    .unwrap();
    match &result[0] {
        pgwire::api::results::Response::Query(_) => {}
        other => panic!("expected Query response, got: {other:?}"),
    }
}
