//! Shared pgwire end-to-end test harness.
//!
//! Spawns a full NodeDB server (Data Plane core + pgwire listener + response poller)
//! and provides a connected `tokio_postgres::Client` for SQL execution.

use std::sync::Arc;
use std::time::Duration;

use nodedb::bridge::dispatch::Dispatcher;
use nodedb::config::auth::AuthMode;
use nodedb::control::server::pgwire::listener::PgListener;
use nodedb::control::state::SharedState;
use nodedb::data::executor::core_loop::CoreLoop;
use nodedb::event::{EventPlane, create_event_bus};
use nodedb::types::TenantId;
use nodedb::wal::WalManager;

pub struct TestClient(Option<tokio_postgres::Client>);

impl TestClient {
    fn new(client: tokio_postgres::Client) -> Self {
        Self(Some(client))
    }

    fn take(&mut self) -> Option<tokio_postgres::Client> {
        self.0.take()
    }

    fn as_ref(&self) -> &tokio_postgres::Client {
        self.0.as_ref().expect("test client already closed")
    }
}

impl std::ops::Deref for TestClient {
    type Target = tokio_postgres::Client;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

/// A running test server with a connected pgwire client.
pub struct TestServer {
    pub client: TestClient,
    pub pg_port: u16,
    /// Underlying shared state — exposed so integration tests can drive
    /// store-level side effects (e.g. seeding a session handle with a
    /// specific `ClientFingerprint`) before hitting the wire.
    #[allow(dead_code)]
    pub shared: Arc<SharedState>,
    conn_handle: Option<tokio::task::JoinHandle<()>>,
    // Fields wrapped in Option so that `graceful_shutdown(self)` can `.take()`
    // them without moving out of a type that has a `Drop` impl (E0509).
    // `Drop` checks each one and is a no-op when already taken.
    shutdown_bus: Option<nodedb::control::shutdown::ShutdownBus>,
    poller_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
    core_stop_txs: Option<Vec<std::sync::mpsc::Sender<()>>>,
    pg_handle: Option<tokio::task::JoinHandle<()>>,
    poller_handle: Option<tokio::task::JoinHandle<()>>,
    core_handles: Option<Vec<tokio::task::JoinHandle<()>>>,
    event_plane: Option<EventPlane>,
    _dir: tempfile::TempDir,
}

/// A data directory whose lifetime is decoupled from a `TestServer` instance.
///
/// Obtaining this handle via `TestServer::take_dir()` lets a test shut down
/// one server, inspect or verify the on-disk state, and then call
/// `TestServer::open_on_path()` to reopen against the same files — verifying
/// WAL recovery and persistence across restarts.
pub struct TestDataDir(pub tempfile::TempDir);

impl TestDataDir {
    pub fn path(&self) -> &std::path::Path {
        self.0.path()
    }
}

#[allow(dead_code)]
impl TestServer {
    /// Spawn a single-core NodeDB server and connect via pgwire.
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let wal = Arc::new(WalManager::open_for_testing(&wal_path).unwrap());

        let (dispatcher, data_sides) = Dispatcher::new(1, 64);
        let (event_producers, event_consumers) = create_event_bus(1);

        // Use catalog-backed credential store (required for CREATE FUNCTION/TRIGGER/PROCEDURE).
        let catalog_path = dir.path().join("system.redb");
        let credentials = Arc::new(
            nodedb::control::security::credential::store::CredentialStore::open(&catalog_path)
                .unwrap(),
        );
        // Provision the harness superuser `nodedb` so Trust-mode strict
        // identity resolution accepts the default test connection. The
        // bootstrap exception in the handler only fires when the store
        // is empty, which would break as soon as any DDL creates a user.
        let _ = credentials.create_user(
            "nodedb",
            "nodedb",
            nodedb::types::TenantId::new(1),
            vec![nodedb::control::security::identity::Role::Superuser],
        );
        let mut shared =
            SharedState::new_with_credentials(dispatcher, Arc::clone(&wal), credentials);
        // Inject a fixed test KEK so backup tests produce encrypted envelopes.
        // Deterministic 32-byte key — same value every test run.
        if let Some(s) = Arc::get_mut(&mut shared) {
            s.backup_kek = Some(Arc::new([0x42u8; 32]));
        }
        let shared = shared;

        // Data Plane core. Share the SharedState's array_catalog so DDL
        // mutations made by the SQL converter are visible to the handler
        // (without this, CP and DP would each carry independent catalogs
        // and `OpenArray` post-DROP-and-recreate would see stale state).
        let mut core_stop_txs = Vec::new();
        let mut core_handles = Vec::new();
        for (idx, (data_side, event_producer)) in
            data_sides.into_iter().zip(event_producers).enumerate()
        {
            let core_dir = dir.path().to_path_buf();
            let core_array_catalog = shared.array_catalog.clone();
            let (core_stop_tx, core_stop_rx) = std::sync::mpsc::channel::<()>();
            let core_handle = tokio::task::spawn_blocking(move || {
                let mut core = CoreLoop::open_with_array_catalog(
                    idx,
                    data_side.request_rx,
                    data_side.response_tx,
                    &core_dir,
                    std::sync::Arc::new(nodedb_types::OrdinalClock::new()),
                    core_array_catalog,
                )
                .unwrap();
                core.set_event_producer(event_producer);
                while matches!(
                    core_stop_rx.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ) {
                    core.tick();
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            core_stop_txs.push(core_stop_tx);
            core_handles.push(core_handle);
        }

        // Response poller.
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

        let watermark_store =
            Arc::new(nodedb::event::watermark::WatermarkStore::open(dir.path()).unwrap());
        let trigger_dlq = Arc::new(std::sync::Mutex::new(
            nodedb::event::trigger::TriggerDlq::open(dir.path()).unwrap(),
        ));
        let event_plane = EventPlane::spawn(
            event_consumers,
            Arc::clone(&wal),
            watermark_store,
            Arc::clone(&shared),
            trigger_dlq,
            Arc::clone(&shared.cdc_router),
            Arc::clone(&shared.shutdown),
        );

        // PgWire listener.
        let pg_listener = PgListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let pg_addr = pg_listener.local_addr();

        // Create a shutdown bus wrapping the shared.shutdown watch so that
        // bus.initiate() also signals the flat ShutdownWatch.
        let (shutdown_bus, _) =
            nodedb::control::shutdown::ShutdownBus::new(Arc::clone(&shared.shutdown));
        let shared_pg = Arc::clone(&shared);
        // Use the startup gate already on SharedState (a pre-fired placeholder
        // from `new_inner`). The listener starts accepting immediately.
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

        // Connect client.
        let conn_str = format!(
            "host=127.0.0.1 port={} user=nodedb dbname=nodedb",
            pg_addr.port()
        );
        let (client, connection) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls)
            .await
            .expect("pgwire connect failed");

        let conn_handle = tokio::spawn(async move {
            let _ = connection.await;
        });

        Self {
            client: TestClient::new(client),
            pg_port: pg_addr.port(),
            shared,
            conn_handle: Some(conn_handle),
            shutdown_bus: Some(shutdown_bus),
            poller_shutdown_tx: Some(poller_shutdown_tx),
            core_stop_txs: Some(core_stop_txs),
            pg_handle: Some(pg_handle),
            poller_handle: Some(poller_handle),
            core_handles: Some(core_handles),
            event_plane: Some(event_plane),
            _dir: dir,
        }
    }

    /// Consume the data directory from a live server so it outlives the
    /// server's lifetime. The server continues to run until dropped, but
    /// ownership of the temp dir moves to the caller so the files survive
    /// the `Drop` of `TestServer`.
    ///
    /// The returned `TestDataDir` must be kept alive until the caller is
    /// done with the on-disk state (i.e., after `open_on_path` returns).
    pub fn take_dir(mut self) -> (Self, TestDataDir) {
        // Replace the TempDir inside self with a new one (data plane has
        // already loaded everything, so the new "empty" dir is unused).
        // We do this by reconstructing with a sentinel. The original dir
        // is returned to the caller via TestDataDir.
        let original_dir = {
            // SAFETY: we swap the dir out before drop so neither the old
            // nor the new TempDir is double-freed.
            let placeholder = tempfile::tempdir().unwrap();
            std::mem::replace(&mut self._dir, placeholder)
        };
        (self, TestDataDir(original_dir))
    }

    /// Consume the server, send shutdown signals, and await all core threads.
    ///
    /// Use this before `TestServer::open_on_path` to guarantee that the
    /// redb environment and WAL file handles are fully released before a
    /// second server opens the same directory.
    pub async fn graceful_shutdown(mut self) {
        // Drop the client first so the underlying socket closes and the
        // server-side pgwire session can drain before we try to reopen redb.
        let _ = self.client.take();
        let _ = self.shared.wal.sync();
        if let Some(bus) = self.shutdown_bus.take() {
            bus.initiate();
        }
        // Stop driving the client connection first so the server-side pgwire
        // session drops its Arc<SharedState> before we try to reopen catalog redb.
        if let Some(h) = self.conn_handle.take() {
            h.abort();
            let _ = h.await;
        }
        if let Some(tx) = self.poller_shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(txs) = self.core_stop_txs.take() {
            for tx in &txs {
                let _ = tx.send(());
            }
        }
        // Wait for Data Plane core threads — they own all engine redb handles.
        if let Some(handles) = self.core_handles.take() {
            for h in handles {
                h.abort();
                let _ = h.await;
            }
        }
        // Abort and join the poller so it drops its Arc<SharedState> clone.
        if let Some(h) = self.poller_handle.take() {
            h.abort();
            let _ = h.await;
        }
        // Let the listener complete its shutdown-bus drain. If it stalls,
        // fall back to abort so the test cannot hang indefinitely.
        if let Some(h) = self.pg_handle.take() {
            let mut h = h;
            match tokio::time::timeout(Duration::from_secs(2), &mut h).await {
                Ok(_) => {}
                Err(_) => {
                    h.abort();
                    let _ = h.await;
                }
            }
        }
        // Wait for event consumers so they drop their Arc<SharedState> and
        // WatermarkStore (redb) clones before we return.
        if let Some(ep) = self.event_plane.take() {
            ep.shutdown_and_join().await;
        }
        // Await every loop registered with LoopRegistry (scheduler, retention,
        // alert eval, etc.) so their Arc<SharedState> clones are released before
        // we return. The shutdown signal was already sent above via bus.initiate().
        self.shared
            .loop_registry
            .shutdown_all(std::time::Duration::from_secs(5))
            .await;
        // conn_handle, shared, _dir all drop here, releasing the remaining
        // Arc<SharedState> and CredentialStore redb handle.
    }

    /// Open a server backed by an existing data directory.
    ///
    /// The WAL, redb stores, and array catalog inside `dir` are reopened
    /// in-place, so any data written by a previous server is immediately
    /// visible. This is the correct way to test WAL-based durability: write
    /// data, call `take_dir()`, drop the first server, then call
    /// `open_on_path()` on the saved dir.
    pub async fn open_on_path(dir: TestDataDir) -> (Self, TestDataDir) {
        let data_dir = TestDataDir(dir.0);
        let server = Self::start_on_dir_ref(data_dir.path()).await;
        (server, data_dir)
    }

    /// Internal: start a server rooted at an existing path without taking
    /// ownership of the directory wrapper.
    async fn start_on_dir_ref(dir_path: &std::path::Path) -> Self {
        let wal_path = dir_path.join("test.wal");
        let wal = Arc::new(WalManager::open_for_testing(&wal_path).unwrap());
        let wal_records: Arc<[nodedb_wal::WalRecord]> =
            Arc::from(wal.replay().unwrap().into_boxed_slice());
        let replay_tombstones = nodedb_wal::extract_tombstones(&wal_records);

        let (dispatcher, data_sides) = Dispatcher::new(1, 64);
        let (event_producers, event_consumers) = create_event_bus(1);

        let catalog_path = dir_path.join("system.redb");
        let credentials = Arc::new(
            nodedb::control::security::credential::store::CredentialStore::open(&catalog_path)
                .unwrap(),
        );
        let _ = credentials.create_user(
            "nodedb",
            "nodedb",
            TenantId::new(1),
            vec![nodedb::control::security::identity::Role::Superuser],
        );
        let mut shared =
            SharedState::new_with_credentials(dispatcher, Arc::clone(&wal), credentials);
        if let Some(s) = Arc::get_mut(&mut shared) {
            s.backup_kek = Some(Arc::new([0x42u8; 32]));
        }
        let shared = shared;
        nodedb::bootstrap::credentials::replay_surrogate_wal(&shared, &wal_records);
        // Restore in-memory synonym registry from the persisted catalog.
        if let Some(catalog) = shared.credentials.catalog()
            && let Err(e) = shared.synonym_registry.reload_from_catalog(catalog)
        {
            eprintln!("pgwire_harness: failed to reload synonym groups: {e}");
        }
        let persisted_collections = shared
            .credentials
            .catalog()
            .as_ref()
            .and_then(|catalog| {
                if let Ok(entries) = catalog.load_all_arrays()
                    && let Ok(mut guard) = shared.array_catalog.write()
                {
                    for entry in entries {
                        let _ = guard.register(entry);
                    }
                }
                catalog
                    .load_collections_for_tenant(TenantId::new(1).as_u64())
                    .ok()
            })
            .unwrap_or_default();

        let mut core_stop_txs = Vec::new();
        let mut core_handles = Vec::new();
        for (idx, (data_side, event_producer)) in
            data_sides.into_iter().zip(event_producers).enumerate()
        {
            let core_dir = dir_path.to_path_buf();
            let core_array_catalog = shared.array_catalog.clone();
            let core_wal_records = Arc::clone(&wal_records);
            let core_tombstones = replay_tombstones.clone();
            let (core_stop_tx, core_stop_rx) = std::sync::mpsc::channel::<()>();
            let core_handle = tokio::task::spawn_blocking(move || {
                let mut core = CoreLoop::open_with_array_catalog(
                    idx,
                    data_side.request_rx,
                    data_side.response_tx,
                    &core_dir,
                    std::sync::Arc::new(nodedb_types::OrdinalClock::new()),
                    core_array_catalog,
                )
                .unwrap();
                core.set_event_producer(event_producer);
                if !core_wal_records.is_empty() {
                    core.replay_vector_wal(&core_wal_records, 1, &core_tombstones);
                    core.replay_kv_wal(&core_wal_records, 1, &core_tombstones);
                    core.replay_timeseries_wal(&core_wal_records, 1, &core_tombstones);
                    core.replay_array_wal(&core_wal_records, 1, &core_tombstones);
                }
                while matches!(
                    core_stop_rx.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ) {
                    core.tick();
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            core_stop_txs.push(core_stop_tx);
            core_handles.push(core_handle);
        }

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

        for coll in persisted_collections.into_iter().filter(|c| c.is_active) {
            nodedb::control::server::pgwire::ddl::collection::create::register::dispatch_register_from_stored(
                &shared,
                &coll,
            )
            .await
            .unwrap();
        }

        let watermark_store =
            Arc::new(nodedb::event::watermark::WatermarkStore::open(dir_path).unwrap());
        let trigger_dlq = Arc::new(std::sync::Mutex::new(
            nodedb::event::trigger::TriggerDlq::open(dir_path).unwrap(),
        ));
        let event_plane = EventPlane::spawn(
            event_consumers,
            Arc::clone(&wal),
            watermark_store,
            Arc::clone(&shared),
            trigger_dlq,
            Arc::clone(&shared.cdc_router),
            Arc::clone(&shared.shutdown),
        );

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
            let _ = connection.await;
        });

        // Use a new temp dir as the placeholder _dir (data is already open from dir_path).
        let placeholder_dir = tempfile::tempdir().unwrap();

        Self {
            client: TestClient::new(client),
            pg_port: pg_addr.port(),
            shared,
            conn_handle: Some(conn_handle),
            shutdown_bus: Some(shutdown_bus),
            poller_shutdown_tx: Some(poller_shutdown_tx),
            core_stop_txs: Some(core_stop_txs),
            pg_handle: Some(pg_handle),
            poller_handle: Some(poller_handle),
            core_handles: Some(core_handles),
            event_plane: Some(event_plane),
            _dir: placeholder_dir,
        }
    }

    /// Spawn an N-core NodeDB server and connect via pgwire.
    ///
    /// All N cores receive Register dispatches and are covered by the
    /// cross-core schema visibility barrier.  Use this variant in tests
    /// that verify schema changes are visible on every core before DDL
    /// returns success.
    pub async fn start_multicores(num_cores: usize) -> Self {
        assert!(num_cores >= 1, "num_cores must be at least 1");
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let wal = Arc::new(WalManager::open_for_testing(&wal_path).unwrap());

        let (dispatcher, data_sides) = Dispatcher::new(num_cores, 64);
        let (event_producers, event_consumers) = create_event_bus(num_cores);

        let catalog_path = dir.path().join("system.redb");
        let credentials = Arc::new(
            nodedb::control::security::credential::store::CredentialStore::open(&catalog_path)
                .unwrap(),
        );
        let _ = credentials.create_user(
            "nodedb",
            "nodedb",
            TenantId::new(1),
            vec![nodedb::control::security::identity::Role::Superuser],
        );
        let mut shared =
            SharedState::new_with_credentials(dispatcher, Arc::clone(&wal), credentials);
        if let Some(s) = Arc::get_mut(&mut shared) {
            s.backup_kek = Some(Arc::new([0x42u8; 32]));
        }
        let shared = shared;

        let mut core_stop_txs = Vec::new();
        let mut core_handles = Vec::new();
        for (idx, (data_side, event_producer)) in
            data_sides.into_iter().zip(event_producers).enumerate()
        {
            let core_dir = dir.path().to_path_buf();
            let core_array_catalog = shared.array_catalog.clone();
            let (core_stop_tx, core_stop_rx) = std::sync::mpsc::channel::<()>();
            let core_handle = tokio::task::spawn_blocking(move || {
                let mut core = CoreLoop::open_with_array_catalog(
                    idx,
                    data_side.request_rx,
                    data_side.response_tx,
                    &core_dir,
                    std::sync::Arc::new(nodedb_types::OrdinalClock::new()),
                    core_array_catalog,
                )
                .unwrap();
                core.set_event_producer(event_producer);
                while matches!(
                    core_stop_rx.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ) {
                    core.tick();
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            core_stop_txs.push(core_stop_tx);
            core_handles.push(core_handle);
        }

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

        let watermark_store =
            Arc::new(nodedb::event::watermark::WatermarkStore::open(dir.path()).unwrap());
        let trigger_dlq = Arc::new(std::sync::Mutex::new(
            nodedb::event::trigger::TriggerDlq::open(dir.path()).unwrap(),
        ));
        let event_plane = EventPlane::spawn(
            event_consumers,
            Arc::clone(&wal),
            watermark_store,
            Arc::clone(&shared),
            trigger_dlq,
            Arc::clone(&shared.cdc_router),
            Arc::clone(&shared.shutdown),
        );

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
            let _ = connection.await;
        });

        Self {
            client: TestClient::new(client),
            pg_port: pg_addr.port(),
            shared,
            conn_handle: Some(conn_handle),
            shutdown_bus: Some(shutdown_bus),
            poller_shutdown_tx: Some(poller_shutdown_tx),
            core_stop_txs: Some(core_stop_txs),
            pg_handle: Some(pg_handle),
            poller_handle: Some(poller_handle),
            core_handles: Some(core_handles),
            event_plane: Some(event_plane),
            _dir: dir,
        }
    }

    /// Execute a SQL statement, returning the text of each row's first column.
    pub async fn query_text(&self, sql: &str) -> Result<Vec<String>, String> {
        let client = self.client.as_ref();
        match client.simple_query(sql).await {
            Ok(msgs) => {
                let mut rows = Vec::new();
                for msg in msgs {
                    if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                        rows.push(row.get(0).unwrap_or("").to_string());
                    }
                }
                Ok(rows)
            }
            Err(e) => Err(pg_error_detail(&e)),
        }
    }

    /// Execute a SQL statement, returning every row as a Vec of its column
    /// values (in projection order). Column count is taken from the first
    /// row received.
    pub async fn query_rows(&self, sql: &str) -> Result<Vec<Vec<String>>, String> {
        let client = self.client.as_ref();
        match client.simple_query(sql).await {
            Ok(msgs) => {
                let mut rows: Vec<Vec<String>> = Vec::new();
                for msg in msgs {
                    if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                        let n = row.len();
                        let mut cols: Vec<String> = Vec::with_capacity(n);
                        for i in 0..n {
                            cols.push(row.get(i).unwrap_or("").to_string());
                        }
                        rows.push(cols);
                    }
                }
                Ok(rows)
            }
            Err(e) => Err(pg_error_detail(&e)),
        }
    }

    /// Execute a SQL statement expecting success (no result needed).
    pub async fn exec(&self, sql: &str) -> Result<(), String> {
        let client = self.client.as_ref();
        match client.simple_query(sql).await {
            Ok(_) => Ok(()),
            Err(e) => Err(pg_error_detail(&e)),
        }
    }

    /// Open a second pgwire connection on the same listener under a different
    /// username. Returns a client and its background connection task handle.
    pub async fn connect_as(
        &self,
        user: &str,
        password: &str,
    ) -> Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>), String> {
        let conn_str = format!(
            "host=127.0.0.1 port={} user={} password={} dbname=nodedb",
            self.pg_port, user, password
        );
        let (client, connection) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| pg_error_detail(&e))?;
        let handle = tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok((client, handle))
    }

    /// Execute a SQL statement expecting an error containing the given substring.
    pub async fn expect_error(&self, sql: &str, expected_substring: &str) {
        let client = self.client.as_ref();
        match client.simple_query(sql).await {
            Ok(_) => panic!("expected error containing '{expected_substring}', got success"),
            Err(e) => {
                let msg = pg_error_detail(&e);
                assert!(
                    msg.to_lowercase()
                        .contains(&expected_substring.to_lowercase()),
                    "expected error containing '{expected_substring}', got: {msg}"
                );
            }
        }
    }
}

/// Extract detailed error message from a tokio-postgres error.
///
/// tokio-postgres `Error::to_string()` just returns "db error" — useless for debugging.
/// This function extracts the actual server message from the `DbError` if available.
fn pg_error_detail(e: &tokio_postgres::Error) -> String {
    if let Some(db_err) = e.as_db_error() {
        format!(
            "{}: {} (SQLSTATE {})",
            db_err.severity(),
            db_err.message(),
            db_err.code().code()
        )
    } else {
        format!("{e:?}")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Each field is `None` when `graceful_shutdown` already ran; skip in that case.
        if let Some(bus) = self.shutdown_bus.take() {
            bus.initiate();
        }
        if let Some(tx) = self.poller_shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(txs) = self.core_stop_txs.take() {
            for tx in &txs {
                let _ = tx.send(());
            }
        }
        let _ = self.client.take();
        if let Some(h) = self.conn_handle.take() {
            h.abort();
        }
        if let Some(h) = self.pg_handle.take() {
            h.abort();
        }
        if let Some(h) = self.poller_handle.take() {
            h.abort();
        }
        if let Some(handles) = self.core_handles.take() {
            for h in handles {
                h.abort();
            }
        }
    }
}
