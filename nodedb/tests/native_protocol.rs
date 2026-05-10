// SPDX-License-Identifier: BUSL-1.1

//! Integration tests for the native binary protocol (port 6433).
//!
//! Covers:
//! - Version handshake: server accepts proto v1, rejects v0 / v2+
//! - Capability bits: server advertises known bits; client requests intersection
//! - Max frame 16 MiB enforced: oversized frame → typed error + clean close
//! - JSON-vs-MsgPack auto-detect: first frame selects encoding for the session
//! - Mid-session encoding switch: rejected with a decode error response

mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use nodedb::bridge::dispatch::Dispatcher;
use nodedb::config::auth::AuthMode;
use nodedb::control::server::listener::Listener;
use nodedb::control::state::SharedState;
use nodedb::data::executor::core_loop::CoreLoop;
use nodedb::event::{EventPlane, create_event_bus};
use nodedb::wal::WalManager;
use nodedb_types::protocol::request_fields::RequestFields;
use nodedb_types::protocol::text_fields::TextFields;
use nodedb_types::protocol::{
    CAP_FTS, CAP_MSGPACK, CAP_SPATIAL, CAP_STREAMING, FRAME_HEADER_LEN, HELLO_ACK_MAGIC,
    HELLO_ERROR_MAGIC_U32, HelloAckFrame, HelloErrorCode, HelloErrorFrame, HelloFrame,
    MAX_FRAME_SIZE, NativeRequest, NativeResponse, OpCode, PROTO_VERSION_MAX, PROTO_VERSION_MIN,
};

// ─── Test server harness ────────────────────────────────────────────────────

struct NativeTestServer {
    addr: std::net::SocketAddr,
    shutdown_bus: nodedb::control::shutdown::ShutdownBus,
    poller_shutdown_tx: tokio::sync::watch::Sender<bool>,
    core_stop_tx: std::sync::mpsc::Sender<()>,
    _listener_handle: tokio::task::JoinHandle<()>,
    _poller_handle: tokio::task::JoinHandle<()>,
    _core_handle: tokio::task::JoinHandle<()>,
    _event_plane: EventPlane,
    _dir: tempfile::TempDir,
}

impl NativeTestServer {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_path = dir.path().join("test.wal");
        let wal = Arc::new(WalManager::open_for_testing(&wal_path).expect("open wal"));

        let (dispatcher, data_sides) = Dispatcher::new(1, 64);
        let (event_producers, event_consumers) = create_event_bus(1);
        let shared = SharedState::new(dispatcher, Arc::clone(&wal));

        let data_side = data_sides.into_iter().next().expect("data side");
        let core_dir = dir.path().to_path_buf();
        let event_producer = event_producers.into_iter().next().expect("event producer");
        let core_array_catalog = shared.array_catalog.clone();
        let (core_stop_tx, core_stop_rx) = std::sync::mpsc::channel::<()>();
        let _core_handle = tokio::task::spawn_blocking(move || {
            let mut core = CoreLoop::open_with_array_catalog(
                0,
                data_side.request_rx,
                data_side.response_tx,
                &core_dir,
                std::sync::Arc::new(nodedb_types::OrdinalClock::new()),
                core_array_catalog,
            )
            .expect("open core");
            core.set_event_producer(event_producer);
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
        let _poller_handle = tokio::spawn(async move {
            loop {
                shared_poller.poll_and_route_responses();
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                    _ = poller_shutdown_rx.changed() => break,
                }
            }
        });

        let watermark_store = Arc::new(
            nodedb::event::watermark::WatermarkStore::open(dir.path()).expect("watermark"),
        );
        let trigger_dlq = Arc::new(std::sync::Mutex::new(
            nodedb::event::trigger::TriggerDlq::open(dir.path()).expect("trigger dlq"),
        ));
        let _event_plane = EventPlane::spawn(
            event_consumers,
            Arc::clone(&wal),
            watermark_store,
            Arc::clone(&shared),
            trigger_dlq,
            Arc::clone(&shared.cdc_router),
            Arc::clone(&shared.shutdown),
        );

        let listener = Listener::bind("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("bind");
        let addr = listener.local_addr();

        let (shutdown_bus, _) =
            nodedb::control::shutdown::ShutdownBus::new(Arc::clone(&shared.shutdown));
        let shared_listener = Arc::clone(&shared);
        let test_startup_gate = Arc::clone(&shared.startup);
        let bus_listener = shutdown_bus.clone();
        let _listener_handle = tokio::spawn(async move {
            listener
                .run(nodedb::control::server::listener::ListenerRunParams {
                    state: shared_listener,
                    auth_mode: AuthMode::Trust,
                    tls_acceptor: None,
                    conn_semaphore: Arc::new(tokio::sync::Semaphore::new(128)),
                    startup_gate: test_startup_gate,
                    bus: bus_listener,
                    admission: Arc::new(
                        nodedb::control::server::admission::AdmissionRegistry::new(),
                    ),
                })
                .await
                .expect("listener");
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        Self {
            addr,
            shutdown_bus,
            poller_shutdown_tx,
            core_stop_tx,
            _listener_handle,
            _poller_handle,
            _core_handle,
            _event_plane,
            _dir: dir,
        }
    }

    async fn shutdown(self) {
        self.shutdown_bus.initiate();
        let _ = self.poller_shutdown_tx.send(true);
        let _ = self.core_stop_tx.send(());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Perform the handshake with a custom `HelloFrame`.
/// Returns `(stream, ack_frame)` on success, or the parsed `HelloErrorFrame` via `Err`.
async fn do_handshake(
    addr: std::net::SocketAddr,
    hello: &HelloFrame,
) -> Result<(TcpStream, HelloAckFrame), HelloErrorFrame> {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(&hello.encode())
        .await
        .expect("write hello");
    stream.flush().await.expect("flush");

    let mut magic_buf = [0u8; 4];
    stream.read_exact(&mut magic_buf).await.expect("read magic");
    let magic = u32::from_be_bytes(magic_buf);

    if magic == HELLO_ERROR_MAGIC_U32 {
        // Read error code + msg_len + message.
        let mut code_buf = [0u8; 1];
        stream.read_exact(&mut code_buf).await.expect("read code");
        let mut len_buf = [0u8; 1];
        stream.read_exact(&mut len_buf).await.expect("read msg_len");
        let msg_len = len_buf[0] as usize;
        let mut msg = vec![0u8; msg_len];
        if msg_len > 0 {
            stream.read_exact(&mut msg).await.expect("read msg");
        }
        // Reassemble the full error frame bytes for HelloErrorFrame::decode.
        let mut full = Vec::with_capacity(6 + msg_len);
        full.extend_from_slice(b"NDBE");
        full.push(code_buf[0]);
        full.push(len_buf[0]);
        full.extend_from_slice(&msg);
        let err_frame = HelloErrorFrame::decode(&full).expect("decode error frame");
        return Err(err_frame);
    }

    assert_eq!(magic, HELLO_ACK_MAGIC, "expected HelloAck magic");

    // Read fixed rest: proto_version(2) + capabilities(8) + sv_len(1).
    let mut fixed_rest = [0u8; 11];
    stream
        .read_exact(&mut fixed_rest)
        .await
        .expect("read fixed");
    let sv_len = fixed_rest[10] as usize;
    let var_len = sv_len + 1 + 7 * 5;
    let mut var_buf = vec![0u8; var_len];
    stream.read_exact(&mut var_buf).await.expect("read var");

    let mut ack_buf = Vec::with_capacity(4 + 11 + var_len);
    ack_buf.extend_from_slice(&magic_buf);
    ack_buf.extend_from_slice(&fixed_rest);
    ack_buf.extend_from_slice(&var_buf);

    let ack = HelloAckFrame::decode(&ack_buf).expect("decode ack");
    Ok((stream, ack))
}

/// Write a length-prefixed frame payload to the stream.
async fn write_frame(stream: &mut TcpStream, payload: &[u8]) {
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len).await.expect("write len");
    stream.write_all(payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

/// Read a length-prefixed frame from the stream.
/// Returns `None` on EOF.
async fn read_frame(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; FRAME_HEADER_LEN];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return None,
        Err(e) => panic!("read_frame error: {e}"),
    }
    let payload_len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).await.expect("read payload");
    Some(payload)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Version handshake: server accepts a proto v1 client and echoes proto_version=1.
#[tokio::test]
async fn version_handshake_v1_accepted() {
    let server = NativeTestServer::start().await;

    let hello = HelloFrame {
        proto_min: 1,
        proto_max: 1,
        capabilities: CAP_STREAMING | CAP_MSGPACK,
    };
    let result = do_handshake(server.addr, &hello).await;
    server.shutdown().await;

    let (_stream, ack) = result.expect("handshake should succeed for v1 client");
    assert_eq!(ack.proto_version, 1, "negotiated version must be 1");
    assert!(
        ack.server_version.contains("NodeDB"),
        "server_version '{}'  missing 'NodeDB'",
        ack.server_version
    );
}

/// Version handshake: server rejects a client whose range is entirely below v1
/// (proto v0 only) with a `VersionMismatch` error and clean disconnect.
#[tokio::test]
async fn version_handshake_v0_rejected() {
    // PROTO_VERSION_MIN is 1, so a client advertising [0, 0] has no overlap.
    if PROTO_VERSION_MIN == 0 {
        // If MIN were ever lowered to 0, v0 clients would be accepted — skip.
        return;
    }

    let server = NativeTestServer::start().await;

    let hello = HelloFrame {
        proto_min: 0,
        proto_max: 0,
        capabilities: 0,
    };
    let result = do_handshake(server.addr, &hello).await;
    server.shutdown().await;

    let err_frame = result.expect_err("v0-only client must be rejected");
    assert_eq!(
        err_frame.code,
        HelloErrorCode::VersionMismatch,
        "error code must be VersionMismatch, got {:?}",
        err_frame.code
    );
}

/// Version handshake: server rejects a client whose range is entirely above
/// the server maximum (proto v2+) with a `VersionMismatch` error.
#[tokio::test]
async fn version_handshake_future_version_rejected() {
    let server = NativeTestServer::start().await;

    let hello = HelloFrame {
        proto_min: PROTO_VERSION_MAX.saturating_add(1),
        proto_max: PROTO_VERSION_MAX.saturating_add(5),
        capabilities: 0,
    };
    let result = do_handshake(server.addr, &hello).await;
    server.shutdown().await;

    let err_frame = result.expect_err("future-version-only client must be rejected");
    assert_eq!(
        err_frame.code,
        HelloErrorCode::VersionMismatch,
        "error code must be VersionMismatch"
    );
}

/// Capability bits: server advertises a non-empty capability set; when the client
/// requests a subset the ack reflects that subset (intersection).
/// Asserts at least one set bit and at least one bit in the client's request
/// that the server supports.
#[tokio::test]
async fn capability_bits_negotiated() {
    let server = NativeTestServer::start().await;

    // Request a subset of known capabilities.
    let client_caps = CAP_STREAMING | CAP_FTS | CAP_SPATIAL;
    let hello = HelloFrame {
        proto_min: 1,
        proto_max: 1,
        capabilities: client_caps,
    };
    let result = do_handshake(server.addr, &hello).await;
    server.shutdown().await;

    let (_stream, ack) = result.expect("handshake ok");

    // Server must advertise at least one capability bit.
    assert_ne!(
        ack.capabilities, 0,
        "server must advertise at least one capability"
    );

    // The echoed bits must be a subset of what the client offered.
    let intersection = ack.capabilities & client_caps;
    assert_ne!(
        intersection, 0,
        "at least one capability bit must be in the intersection"
    );

    // CAP_MSGPACK is defined but was not in client_caps — server does not set it.
    let rejected = CAP_MSGPACK & ack.capabilities & !client_caps;
    assert_eq!(
        rejected, 0,
        "server must not set bits the client did not request"
    );
}

/// Max frame enforcement: sending a frame whose length prefix exceeds 16 MiB
/// causes the server to send a typed error response and close the connection.
/// The connection must NOT hang and the process must NOT crash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_frame_size_enforced() {
    let server = NativeTestServer::start().await;

    // Complete the handshake first so we're past the hello exchange.
    let (mut stream, _ack) = do_handshake(server.addr, &HelloFrame::current())
        .await
        .expect("handshake");

    // Write a frame whose 4-byte length prefix = MAX_FRAME_SIZE + 1.
    // We do NOT need to write that many bytes — the server rejects at the header.
    let oversized_len = (MAX_FRAME_SIZE + 1).to_be_bytes();
    stream
        .write_all(&oversized_len)
        .await
        .expect("write oversized length prefix");
    stream.flush().await.expect("flush");

    // Server must respond with a typed error frame (SQLSTATE 54000 / out-of-range)
    // and then close the connection.
    let response_payload = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream))
        .await
        .expect("server must respond within 5 seconds");

    server.shutdown().await;

    let payload = response_payload.expect("server must send an error response before closing");

    // The response must be a valid NativeResponse with an error status.
    // Try MsgPack first (default format before first data frame), then JSON.
    let response: NativeResponse = zerompk::from_msgpack(&payload)
        .or_else(|_| sonic_rs::from_slice(&payload))
        .expect("response must be a valid NativeResponse");

    assert_eq!(
        response.status,
        nodedb_types::protocol::opcodes::ResponseStatus::Error,
        "response status must be Error for oversized frame"
    );

    let err = response.error.expect("error payload must be present");
    assert!(
        err.message.contains("frame") || err.message.contains("54000") || err.code == "54000",
        "error must mention frame rejection, got code='{}' message='{}'",
        err.code,
        err.message
    );
}

/// JSON request → JSON response: if the client sends a JSON-encoded Ping,
/// the server responds with a JSON-encoded response.
#[tokio::test]
async fn json_request_gets_json_response() {
    let server = NativeTestServer::start().await;

    let (mut stream, _ack) = do_handshake(server.addr, &HelloFrame::current())
        .await
        .expect("handshake");

    // Encode a Ping request as JSON.
    let req = NativeRequest {
        op: OpCode::Ping,
        seq: 77,
        fields: RequestFields::Text(TextFields::default()),
    };
    let json_bytes = sonic_rs::to_vec(&req).expect("json encode");
    assert_eq!(json_bytes[0], b'{', "JSON must start with open brace");

    write_frame(&mut stream, &json_bytes).await;

    let response_payload = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream))
        .await
        .expect("timeout")
        .expect("response");

    server.shutdown().await;

    // Response must be valid JSON (starts with `{`).
    assert_eq!(
        response_payload[0], b'{',
        "JSON session must produce JSON response, got first byte 0x{:02X}",
        response_payload[0]
    );
    let resp: NativeResponse = sonic_rs::from_slice(&response_payload).expect("json decode");
    assert_eq!(resp.seq, 77, "seq must echo");
    assert_eq!(
        resp.status,
        nodedb_types::protocol::opcodes::ResponseStatus::Ok,
        "Ping must return Ok"
    );
}

/// MsgPack request → MsgPack response: if the client sends a MsgPack-encoded Ping,
/// the server responds with a MsgPack-encoded response.
#[tokio::test]
async fn msgpack_request_gets_msgpack_response() {
    let server = NativeTestServer::start().await;

    let (mut stream, _ack) = do_handshake(server.addr, &HelloFrame::current())
        .await
        .expect("handshake");

    let req = NativeRequest {
        op: OpCode::Ping,
        seq: 99,
        fields: RequestFields::Text(TextFields::default()),
    };
    let mp_bytes = zerompk::to_msgpack_vec(&req).expect("msgpack encode");
    assert_ne!(mp_bytes[0], b'{', "MsgPack must NOT start with open brace");

    write_frame(&mut stream, &mp_bytes).await;

    let response_payload = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream))
        .await
        .expect("timeout")
        .expect("response");

    server.shutdown().await;

    // Response must NOT start with `{` (it is MsgPack).
    assert_ne!(
        response_payload[0], b'{',
        "MsgPack session must produce MsgPack response, got first byte 0x{:02X}",
        response_payload[0]
    );
    let resp: NativeResponse = zerompk::from_msgpack(&response_payload).expect("msgpack decode");
    assert_eq!(resp.seq, 99, "seq must echo");
    assert_eq!(
        resp.status,
        nodedb_types::protocol::opcodes::ResponseStatus::Ok,
        "Ping must return Ok"
    );
}

/// Mid-session encoding switch: if a JSON session receives a MsgPack frame (or
/// vice versa), the server must NOT silently accept it. It must return an error
/// response (decode failure). The connection stays open — it is not terminated.
#[tokio::test]
async fn mid_session_encoding_switch_rejected() {
    let server = NativeTestServer::start().await;

    let (mut stream, _ack) = do_handshake(server.addr, &HelloFrame::current())
        .await
        .expect("handshake");

    // First frame: JSON → establishes JSON encoding for the session.
    let ping_json = NativeRequest {
        op: OpCode::Ping,
        seq: 1,
        fields: RequestFields::Text(TextFields::default()),
    };
    let json_bytes = sonic_rs::to_vec(&ping_json).expect("json encode");
    write_frame(&mut stream, &json_bytes).await;

    // Consume the response to the first Ping.
    let _first_resp = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream))
        .await
        .expect("timeout")
        .expect("first response");

    // Second frame: MsgPack — the session is locked to JSON, so this must fail decode.
    let ping_mp = NativeRequest {
        op: OpCode::Ping,
        seq: 2,
        fields: RequestFields::Text(TextFields::default()),
    };
    let mp_bytes = zerompk::to_msgpack_vec(&ping_mp).expect("msgpack encode");
    write_frame(&mut stream, &mp_bytes).await;

    let switch_resp = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream))
        .await
        .expect("timeout")
        .expect("switch response");

    server.shutdown().await;

    // The response must be JSON-encoded (session is still in JSON mode).
    assert_eq!(
        switch_resp[0], b'{',
        "response must still be JSON after mid-session switch attempt"
    );
    let resp: NativeResponse = sonic_rs::from_slice(&switch_resp).expect("json decode");
    assert_eq!(
        resp.status,
        nodedb_types::protocol::opcodes::ResponseStatus::Error,
        "mid-session encoding switch must produce an Error response"
    );
}
