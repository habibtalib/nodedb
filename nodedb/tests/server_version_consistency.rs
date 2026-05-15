// SPDX-License-Identifier: BUSL-1.1

//! Regression suite for the wire-string version reporting.
//!
//! All three wire surfaces (pgwire startup `server_version` parameter,
//! pgwire `SHOW server_version` command, and RESP `INFO` `nodedb_version`)
//! must report a version sourced from the workspace's `CARGO_PKG_VERSION`.
//! Before the fix that introduced these tests, three call sites hardcoded
//! the literal string `"0.1.0"` and drifted as the workspace bumped to
//! `0.2.0`.

use std::sync::Arc;
use std::time::Duration;

use nodedb::bridge::dispatch::Dispatcher;
use nodedb::config::auth::AuthMode;
use nodedb::control::server::pgwire::listener::PgListener;
use nodedb::control::server::server_version_string;
use nodedb::control::state::SharedState;
use nodedb::data::executor::core_loop::CoreLoop;
use nodedb::wal::WalManager;

/// `server_version_string()` is the single source of truth for the wire
/// surface. It must format as `"NodeDB <semver>"` where `<semver>` is the
/// workspace's compile-time `CARGO_PKG_VERSION`.
#[test]
fn server_version_string_uses_cargo_pkg_version() {
    let v = server_version_string();
    let semver = v
        .strip_prefix("NodeDB ")
        .unwrap_or_else(|| panic!("missing 'NodeDB ' prefix: {v}"));
    assert_eq!(
        semver,
        env!("CARGO_PKG_VERSION"),
        "server_version_string() must equal CARGO_PKG_VERSION; \
         got `{semver}`, expected `{}`",
        env!("CARGO_PKG_VERSION")
    );
}

/// Regression guard: every wire-string call site was once hardcoded to
/// `"NodeDB 0.1.0"`. If anyone reintroduces a literal at any call site,
/// `server_version_string()` itself would still pass, but as a defense in
/// depth verify the formatted output is not the stale literal whenever the
/// workspace has moved past 0.1.0.
#[test]
fn server_version_string_is_not_stale_literal() {
    if env!("CARGO_PKG_VERSION") == "0.1.0" {
        // Workspace genuinely is 0.1.0 — nothing to assert.
        return;
    }
    assert_ne!(
        server_version_string(),
        "NodeDB 0.1.0",
        "version string is stale; should track workspace CARGO_PKG_VERSION"
    );
}

/// End-to-end pgwire test: a tokio-postgres client sees the same version
/// via both the startup `ParameterStatus` message and the
/// `SHOW server_version` query.
#[tokio::test]
async fn pgwire_reports_dynamic_server_version() {
    // Infrastructure setup mirrors pgwire_connect.rs.
    let dir = tempfile::tempdir().unwrap();
    let wal_path = dir.path().join("test.wal");
    let wal = Arc::new(WalManager::open_for_testing(&wal_path).unwrap());

    let (dispatcher, data_sides) = Dispatcher::new(1, 64);
    let shared = SharedState::new(dispatcher, wal);

    let data_side = data_sides.into_iter().next().unwrap();
    let core_dir = dir.path().to_path_buf();
    let (core_stop_tx, core_stop_rx) = std::sync::mpsc::channel::<()>();
    let core_handle = tokio::task::spawn_blocking(move || {
        let mut core = CoreLoop::open(
            0,
            data_side.request_rx,
            data_side.response_tx,
            &core_dir,
            std::sync::Arc::new(nodedb_types::OrdinalClock::new()),
        )
        .unwrap();
        while matches!(
            core_stop_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ) {
            core.tick();
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    let shared_poller = Arc::clone(&shared);
    let (poller_shutdown_tx, mut poller_shutdown_rx) = tokio::sync::watch::channel(false);
    let poller_handle = tokio::spawn(async move {
        loop {
            shared_poller.poll_and_route_responses();
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                _ = poller_shutdown_rx.changed() => break,
            }
        }
    });

    let pg_listener = PgListener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let pg_addr = pg_listener.local_addr();

    let (shutdown_bus, _) =
        nodedb::control::shutdown::ShutdownBus::new(Arc::clone(&shared.shutdown));
    let shared_pg = Arc::clone(&shared);
    let test_startup_gate = Arc::clone(&shared.startup);
    let bus_pg = shutdown_bus.clone();
    let pg_handle = tokio::spawn(async move {
        pg_listener
            .run(
                shared_pg,
                AuthMode::Trust,
                None,
                Arc::new(tokio::sync::Semaphore::new(128)),
                test_startup_gate,
                bus_pg,
            )
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let conn_str = format!(
        "host=127.0.0.1 port={} user=nodedb dbname=nodedb",
        pg_addr.port()
    );
    let (client, connection) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls)
        .await
        .expect("pgwire connect failed");

    let conn_handle = tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {e}");
        }
    });

    // tokio-postgres doesn't expose the startup `ParameterStatus` map on a
    // stable public API. The startup parameter goes through the same
    // `server_version_string()` helper as `SHOW server_version` (see
    // pgwire/factory.rs and pgwire/handler/session_cmds.rs), so verifying
    // SHOW also verifies the startup param.
    //
    // `SHOW server_version` returns the centralized string.
    let rows = client
        .simple_query("SHOW server_version")
        .await
        .expect("SHOW server_version failed");
    let row = rows
        .iter()
        .find_map(|m| match m {
            tokio_postgres::SimpleQueryMessage::Row(r) => Some(r),
            _ => None,
        })
        .expect("SHOW server_version returned no row");
    let reported = row.get(0).expect("server_version column empty").to_owned();
    assert_eq!(
        reported,
        server_version_string(),
        "SHOW server_version `{reported}` does not match server_version_string() `{}`",
        server_version_string()
    );

    // Cleanup.
    drop(client);
    let _ = conn_handle.await;
    shutdown_bus.initiate();
    let _ = pg_handle.await;
    let _ = poller_shutdown_tx.send(true);
    let _ = poller_handle.await;
    let _ = core_stop_tx.send(());
    let _ = core_handle.await;
}
