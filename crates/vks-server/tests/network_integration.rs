//! Network integration test suite with live TCP/UDP sockets on 127.0.0.1.
//!
//! Tests:
//! 1. `test_admin_rest_jwt_sqlite`: Room CRUD via Axum REST with SQLite persistence and JWT auth.
//! 2. `test_signaling_ws_handshake`: Full WebRTC signaling handshake (Hello -> Join -> SDP Offer/Answer -> ICE -> Leave) over live WebSocket.
//! 3. `test_sfu_udp_media_loopback`: Zero-copy RTP packet routing over live UDP socket.
//! 4. `test_e2e_two_clients_signaling_session`: Two clients join room, negotiate SDP, and verify state.
//! 5. `test_live_composition_root_serves_ui`: `build_app` отдаёт UI PNM (`/` → 307 `/pnm/`, `/pnm/` → index), v1 не сломан.
//! 6. `test_composition_api_v3_login_chain`: сквозная цепочка адаптера UI через `build_app`
//!    (exempt → 401 без токена → login → sessions с токеном → catch-all 501).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as AxumWsMsg, WebSocket, WebSocketUpgrade};
use axum::http::{header, Request, StatusCode};
use axum::middleware;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TungsteniteMsg;
use tower::ServiceExt;

use vks_admin::auth_middleware::jwt_auth_middleware;
use vks_admin::handlers::router as admin_router;
use vks_admin::state::AppState;
use vks_auth::jwt::{JwtConfig, JwtIssuer};
use vks_auth::local_db::LocalDb;
use vks_media_sfu::forwarder::RtpPacket;
use vks_media_sfu::udp_loop::UdpMediaLoop;
use vks_proto::signaling as sig;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::SignalingMessage;
use vks_server::build_app;
use vks_server::frontend::Frontend;
use vks_signaling::envelope::Envelope;
use vks_signaling::handler::{HandlerConfig, SignalingHandler};
use vks_storage::SqliteStorage;

fn client_hello(msg_id: u64, room_id: &str, client_id: &str) -> Envelope {
    let msg = sig::ClientHello {
        proto_version: 1,
        client_id: client_id.into(),
        sdk_version: "1.0".into(),
        user_agent: "test".into(),
        auth_token: "mock-token".into(),
        room_id: room_id.into(),
        capabilities: Vec::new(),
        resume_nonce: None,
    };
    Envelope::new(SignalingMessage { msg_id, reply_to: 0, payload: Some(Payload::ClientHello(msg)) })
}

fn join_room(msg_id: u64, room_id: &str, display_name: &str) -> Envelope {
    let msg = sig::JoinRoom { room_id: room_id.into(), display_name: display_name.into(), is_recorder: false };
    Envelope::new(SignalingMessage { msg_id, reply_to: 0, payload: Some(Payload::JoinRoom(msg)) })
}

fn sdp_offer(msg_id: u64, sdp: &str) -> Envelope {
    let msg = sig::SdpOffer { sdp: sdp.into() };
    Envelope::new(SignalingMessage { msg_id, reply_to: 0, payload: Some(Payload::SdpOffer(msg)) })
}

const SAMPLE_SDP: &str = "v=0\no=c1 1 1 0 IN IP4 0.0.0.0\ns=test\nm=audio 9 UDP/TLS/RTP/SAVPF 111\na=mid:0\na=rtpmap:111 opus/48000/2\nm=video 9 UDP/TLS/RTP/SAVPF 96\na=mid:1\na=rtpmap:96 VP8/90000";

/// Helper to build test server router with SQLite and JWT.
async fn build_test_app() -> (Router, Arc<AppState>, Arc<JwtIssuer>, String) {
    let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
    let mut state = AppState::with_storage(sqlite);

    let jwt_config = JwtConfig {
        secret: "integration_test_secret_key_98765".to_string(),
        ttl_secs: 600,
        issuer: "vks-integration".to_string(),
    };
    let jwt_issuer = Arc::new(JwtIssuer::new(jwt_config));
    let token = jwt_issuer.issue("admin_user", None).expect("issue token");

    state.jwt_issuer = Some(jwt_issuer.clone());
    let shared_state = Arc::new(state);

    let app = admin_router(shared_state.clone())
        .route("/ws", get(ws_handler))
        .layer(middleware::from_fn_with_state(shared_state.clone(), jwt_auth_middleware));

    (app, shared_state, jwt_issuer, token)
}

async fn ws_handler(ws: WebSocketUpgrade) -> axum::response::Response {
    ws.on_upgrade(handle_ws)
}

async fn handle_ws(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut handler = SignalingHandler::new(HandlerConfig::default_test());

    while let Some(msg_res) = receiver.next().await {
        if let Ok(AxumWsMsg::Binary(bytes)) = msg_res {
            if let Ok(env) = Envelope::decode(&bytes) {
                let replies = handler.handle(env);
                for reply in replies {
                    let _ = sender.send(AxumWsMsg::Binary(reply.encode().into())).await;
                }
            }
        }
    }
}

/// Helper to run live server on an ephemeral port (composition root: `build_app`).
async fn spawn_live_server() -> SocketAddr {
    let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
    let state = Arc::new(AppState::with_storage(sqlite));
    let frontend = Frontend::load(&frontend_fixture_dir());

    let app = build_app(state, &frontend);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    addr
}

/// Каталог-фикстура статики фронтенда для composition-теста (идемпотентно).
fn frontend_fixture_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vks-server-frontend-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create frontend fixture dir");
    std::fs::write(
        dir.join("index.html"),
        "<!DOCTYPE html><html><body><div id=\"root\">vks-frontend-fixture</div></body></html>",
    )
    .expect("write frontend fixture index.html");
    dir
}

#[tokio::test]
async fn test_admin_rest_jwt_sqlite() {
    let (app, _state, _issuer, token) = build_test_app().await;

    // 1. Unauthorized request should fail with 401
    let req_unauth = Request::builder().uri("/api/v1/rooms").body(axum::body::Body::empty()).unwrap();
    let resp_unauth = app.clone().oneshot(req_unauth).await.unwrap();
    assert_eq!(resp_unauth.status(), StatusCode::UNAUTHORIZED);

    // 2. Create room via POST /api/v1/rooms with valid JWT
    let req_create = Request::builder()
        .uri("/api/v1/rooms")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(r#"{"name":"conf-alpha"}"#))
        .unwrap();
    let resp_create = app.clone().oneshot(req_create).await.unwrap();
    assert_eq!(resp_create.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp_create.into_body(), 1024 * 64).await.unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let room_id = created["id"].as_str().expect("room_id");
    assert_eq!(created["name"], "conf-alpha");

    // 3. List rooms via GET /api/v1/rooms
    let req_list = Request::builder()
        .uri("/api/v1/rooms")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp_list = app.clone().oneshot(req_list).await.unwrap();
    assert_eq!(resp_list.status(), StatusCode::OK);

    let list_bytes = axum::body::to_bytes(resp_list.into_body(), 1024 * 64).await.unwrap();
    let rooms: Vec<serde_json::Value> = serde_json::from_slice(&list_bytes).unwrap();
    assert!(rooms.iter().any(|r| r["id"] == room_id));

    // 4. Delete room via DELETE /api/v1/rooms/{id}
    let req_del = Request::builder()
        .uri(format!("/api/v1/rooms/{room_id}"))
        .method("DELETE")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp_del = app.clone().oneshot(req_del).await.unwrap();
    assert_eq!(resp_del.status(), StatusCode::NO_CONTENT);

    // 5. Verify room is deleted
    let req_get = Request::builder()
        .uri(format!("/api/v1/rooms/{room_id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp_get = app.oneshot(req_get).await.unwrap();
    assert_eq!(resp_get.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_signaling_ws_handshake() {
    let addr = spawn_live_server().await;
    let url = format!("ws://{addr}/ws");

    let (ws_stream, _) = connect_async(&url).await.expect("connect to websocket");
    let (mut write, mut read) = ws_stream.split();

    // 1. ClientHello
    let hello = client_hello(1, "room-beta", "test-client");
    write.send(TungsteniteMsg::Binary(hello.encode().into())).await.expect("send hello");

    let reply = read.next().await.expect("server reply").expect("ws msg");
    let env_reply = Envelope::decode(&reply.into_data()).expect("decode server hello");
    assert!(matches!(env_reply.payload(), Some(Payload::ServerHello(_))));

    // 2. JoinRoom
    let join = join_room(2, "room-beta", "Alice");
    write.send(TungsteniteMsg::Binary(join.encode().into())).await.expect("send join");

    let reply = read.next().await.expect("server reply").expect("ws msg");
    let env_reply = Envelope::decode(&reply.into_data()).expect("decode join result");
    match env_reply.payload() {
        Some(Payload::JoinRoomResult(res)) => {
            assert!(res.participant_id.starts_with("part-room-beta-"));
            assert_ne!(res.join_nonce, 0);
        }
        other => panic!("expected JoinRoomResult, got {other:?}"),
    }

    // 3. SdpOffer -> SdpAnswer
    let sdp_off = sdp_offer(3, SAMPLE_SDP);
    write.send(TungsteniteMsg::Binary(sdp_off.encode().into())).await.expect("send sdp offer");

    let reply = read.next().await.expect("server reply").expect("ws msg");
    let env_reply = Envelope::decode(&reply.into_data()).expect("decode sdp answer");
    assert!(matches!(env_reply.payload(), Some(Payload::SdpAnswer(_))));

    // 4. IceComplete -> Ack
    let ice_complete = Envelope::new(SignalingMessage {
        msg_id: 4,
        reply_to: 0,
        payload: Some(Payload::IceComplete(sig::IceComplete { success: true, rtt_ms: 10 })),
    });
    write.send(TungsteniteMsg::Binary(ice_complete.encode().into())).await.expect("send ice complete");

    let reply = read.next().await.expect("server reply").expect("ws msg");
    let env_reply = Envelope::decode(&reply.into_data()).expect("decode ack");
    assert!(matches!(env_reply.payload(), Some(Payload::Ack(_))));

    // 5. LeaveRoom -> Ack
    let leave = Envelope::new(SignalingMessage {
        msg_id: 5,
        reply_to: 0,
        payload: Some(Payload::LeaveRoom(sig::LeaveRoom { room_id: "room-beta".into() })),
    });
    write.send(TungsteniteMsg::Binary(leave.encode().into())).await.expect("send leave");

    let reply = read.next().await.expect("server reply").expect("ws msg");
    let env_reply = Envelope::decode(&reply.into_data()).expect("decode ack");
    assert!(matches!(env_reply.payload(), Some(Payload::Ack(_))));
}

#[tokio::test]
async fn test_sfu_udp_media_loopback() {
    let server = UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind server");
    let client = UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind client");

    let server_addr = server.local_addr();
    let client_addr = client.local_addr();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(SocketAddr, RtpPacket)>(10);

    let _server_handle = server.start_receive_loop(move |src, packet| {
        let _ = tx.try_send((src, packet));
    });

    let dummy_rtp_header_and_payload =
        vec![0x80, 0x60, 0x00, 0x01, 0x00, 0x00, 0x03, 0xe8, 0x12, 0x34, 0x56, 0x78, 0xaa, 0xbb, 0xcc];
    let packet = RtpPacket::new(vks_core::TrackId::new(), dummy_rtp_header_and_payload.clone());

    client.send_packet(server_addr, &packet).await.expect("send udp");

    let (src, received) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("timed out waiting for udp packet")
        .expect("channel closed");

    assert_eq!(src, client_addr);
    assert_eq!(&received.payload[..], &dummy_rtp_header_and_payload[..]);

    server.shutdown();
}

#[tokio::test]
async fn test_e2e_two_clients_signaling_session() {
    let addr = spawn_live_server().await;
    let url = format!("ws://{addr}/ws");

    // Client 1 connects
    let (ws_1, _) = connect_async(&url).await.expect("connect client 1");
    let (mut write_1, mut read_1) = ws_1.split();

    // Client 2 connects
    let (ws_2, _) = connect_async(&url).await.expect("connect client 2");
    let (mut write_2, mut read_2) = ws_2.split();

    // 1. Client 1 Handshake
    let hello_1 = client_hello(1, "shared-room", "client-1");
    write_1.send(TungsteniteMsg::Binary(hello_1.encode().into())).await.unwrap();
    let _ = read_1.next().await.unwrap().unwrap();

    let join_1 = join_room(2, "shared-room", "Alice");
    write_1.send(TungsteniteMsg::Binary(join_1.encode().into())).await.unwrap();
    let join_res_1 = read_1.next().await.unwrap().unwrap();
    let env_res_1 = Envelope::decode(&join_res_1.into_data()).unwrap();
    assert!(matches!(env_res_1.payload(), Some(Payload::JoinRoomResult(_))));

    // 2. Client 2 Handshake
    let hello_2 = client_hello(1, "shared-room", "client-2");
    write_2.send(TungsteniteMsg::Binary(hello_2.encode().into())).await.unwrap();
    let _ = read_2.next().await.unwrap().unwrap();

    let join_2 = join_room(2, "shared-room", "Bob");
    write_2.send(TungsteniteMsg::Binary(join_2.encode().into())).await.unwrap();
    let join_res_2 = read_2.next().await.unwrap().unwrap();
    let env_res_2 = Envelope::decode(&join_res_2.into_data()).unwrap();
    assert!(matches!(env_res_2.payload(), Some(Payload::JoinRoomResult(_))));

    // Both clients successfully joined the room!
}

/// HTTP GET через живой TCP-сокет (статусная строка + заголовки + тело).
async fn raw_get(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect to server");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write request");

    let mut response_buf = Vec::new();
    stream.read_to_end(&mut response_buf).await.expect("read response");
    String::from_utf8_lossy(&response_buf).into_owned()
}

#[tokio::test]
async fn test_live_composition_root_serves_ui() {
    let addr = spawn_live_server().await;

    // `/` → 307 на SPA (`/pnm/`), а не demo-страница.
    let response = raw_get(addr, "/").await;
    assert!(response.starts_with("HTTP/1.1 307"), "root must redirect, got: {response}");
    assert!(response.contains("location: /pnm/"), "redirect target: {response}");

    // `/pnm/` → 200 + index.html фронтенда.
    let response = raw_get(addr, "/pnm/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "ui index must be served: {response}");
    assert!(response.contains("text/html"), "content type: {response}");
    assert!(response.contains("vks-frontend-fixture"), "fixture index body: {response}");

    // Регрессия v1: эндпоинты админки не перехвачены статикой.
    let response = raw_get(addr, "/api/v1/rooms").await;
    assert!(response.starts_with("HTTP/1.1 200"), "v1 regression: {response}");
}

/// Composition-проверка цепочки `/api/v3` через `build_app` (фаза 2, ADR-0027):
/// exempt → fail-closed для остальных → login → защищённый вызов с токеном →
/// catch-all. Доказывает, что адаптер реально смонтирован в композиционном корне,
/// а не «лежит мёртвым грузом» в крейте.
#[tokio::test]
async fn test_composition_api_v3_login_chain() {
    let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
    let mut state = AppState::with_storage(sqlite);
    let issuer = Arc::new(JwtIssuer::new(JwtConfig {
        secret: "composition_api_v3_secret_123".to_string(),
        ttl_secs: 86_400,
        issuer: "vks-composition".to_string(),
    }));
    let db = Arc::new(LocalDb::new());
    db.register("u-admin", "admin", "s3cret").expect("register admin");
    state.jwt_issuer = Some(issuer);
    state.local_db = Some(db);
    let state = Arc::new(state);

    let frontend = Frontend::load(&frontend_fixture_dir());
    let app = build_app(state, &frontend);

    let get = |uri: &str, token: Option<&str>| {
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(axum::body::Body::empty()).expect("build request")
    };

    // 1. Exempt-list работает без токена.
    let resp = app.clone().oneshot(get("/api/v3/login/status", None)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "login/status must be exempt");

    // 2. Защищённый путь без токена → 401-конверт UI.
    let resp = app.clone().oneshot(get("/api/v3/ui/sessions", None)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "sessions must require a token");

    // 3. Login пары admin/s3cret → {token}.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(r#"{"login":"admin","password":"s3cret"}"#))
                .expect("build login request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "login with bootstrapped admin must succeed");
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("login json");
    let token = body["token"].as_str().expect("token field").to_string();

    // 4. С токеном — защищённые ручки отвечают (дашборд: пустой список — не ошибка).
    let resp = app.clone().oneshot(get("/api/v3/ui/sessions", Some(&token))).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "sessions with token must succeed");
    let resp = app.clone().oneshot(get("/api/v3/profile", Some(&token))).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "profile with token must succeed");

    // 5. Security-заголовки — app-wide (включая /api/v3): CSP есть и на API-ответе.
    assert!(
        resp.headers().get(header::CONTENT_SECURITY_POLICY).is_some(),
        "security headers must be app-wide (build_app layer)"
    );

    // 6. Catch-all → 501 (UI не должен видеть 404 «метод не существует»).
    let resp = app.clone().oneshot(get("/api/v3/definitely/unknown", Some(&token))).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED, "catch-all must answer 501");
}

async fn spawn_live_server_with_auth() -> (SocketAddr, Arc<AppState>, String) {
    let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
    let mut state = AppState::with_storage(sqlite);
    let issuer = Arc::new(JwtIssuer::new(JwtConfig {
        secret: "auth_sync_secret_123".to_string(),
        ttl_secs: 86_400,
        issuer: "vks-integration".to_string(),
    }));
    let db = Arc::new(LocalDb::new());
    db.register("u-admin", "admin", "admin").expect("register admin");
    state.jwt_issuer = Some(issuer.clone());
    state.local_db = Some(db);
    let token = issuer.issue("admin", None).expect("issue");

    let state = Arc::new(state);
    let frontend = Frontend::load(&frontend_fixture_dir());
    let app = build_app(state.clone(), &frontend);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    (addr, state, token)
}

async fn reqwest_like_get(addr: SocketAddr, path: &str, token: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");

    let mut response_buf = Vec::new();
    stream.read_to_end(&mut response_buf).await.expect("read");
    let full = String::from_utf8_lossy(&response_buf).into_owned();
    if let Some((_, body)) = full.split_once("\r\n\r\n") {
        body.to_string()
    } else {
        full
    }
}

#[tokio::test]
async fn test_live_signaling_syncs_ui_sessions() {
    let (addr, _state, token) = spawn_live_server_with_auth().await;

    // 1. Initial sessions count is 0
    let client = reqwest_like_get(addr, "/api/v3/ui/sessions", &token).await;
    let json: serde_json::Value = serde_json::from_str(&client).unwrap();
    assert_eq!(json["count"], 0);

    // 2. Connect WebSocket client and join room
    let url = format!("ws://{addr}/ws");
    let (ws_stream, _) = connect_async(&url).await.expect("connect");
    let (mut write, mut read) = ws_stream.split();

    let hello = client_hello(1, "conf-sync", "alice-client");
    write.send(TungsteniteMsg::Binary(hello.encode().into())).await.unwrap();
    let _ = read.next().await.unwrap().unwrap();

    let join = join_room(2, "conf-sync", "Alice");
    write.send(TungsteniteMsg::Binary(join.encode().into())).await.unwrap();
    let _ = read.next().await.unwrap().unwrap();

    // 3. UI Sessions endpoint now lists the room with participants: 1!
    let client = reqwest_like_get(addr, "/api/v3/ui/sessions", &token).await;
    let json: serde_json::Value = serde_json::from_str(&client).unwrap();
    assert_eq!(json["count"], 1);
    assert_eq!(json["sessions"][0]["display_name"], "conf-sync");
    assert_eq!(json["sessions"][0]["participants"], 1);

    // 4. Drop websocket
    drop(write);
    drop(read);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 5. Participants count decreases back to 0
    let client = reqwest_like_get(addr, "/api/v3/ui/sessions", &token).await;
    let json: serde_json::Value = serde_json::from_str(&client).unwrap();
    assert_eq!(json["count"], 1);
    assert_eq!(json["sessions"][0]["participants"], 0);
}

#[tokio::test]
async fn test_sfu_mcu_mode_switcher() {
    use vks_core::room::{RoomId, RoomMode};
    use vks_core::transparency::{ModeSwitcher, SwitchOutcome};
    use vks_media_mcu::McuEngine;
    use vks_media_sfu::SfuEngine;

    let room_id = RoomId::new();
    let sfu = Box::new(SfuEngine::new());
    let mut switcher = ModeSwitcher::new(room_id, sfu, 100, 1);

    assert_eq!(switcher.mode(), RoomMode::Sfu);
    assert_eq!(switcher.epoch(), 0);

    let mcu = Box::new(McuEngine::new());
    let outcome = switcher.switch_mode(mcu);

    assert_eq!(outcome, SwitchOutcome::Switched { epoch: 1 });
    assert_eq!(switcher.mode(), RoomMode::Mcu);
    assert_eq!(switcher.epoch(), 1);

    let events = switcher.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].new_mode, RoomMode::Mcu);
    assert_eq!(events[0].switch_epoch, 1);
}
