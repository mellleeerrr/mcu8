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
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message as TungsteniteMsg;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;

use vks_admin::handlers::router as admin_router;
use vks_admin::state::AppState;
use vks_admin::users::Users;
use vks_auth::jwt::{JwtConfig, JwtIssuer};
use vks_auth::local_db::LocalDb;
use vks_core::Role;
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
    // v1 защищён роутером (JWT + RBAC); запись оператора — как в main.rs.
    Users::create(&state, "admin_user", Role::Operator);
    let shared_state = Arc::new(state);

    let app = admin_router(shared_state.clone()).route("/ws", get(ws_handler));

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

/// These integration tests build the legacy fake-SDP answer path (they send
/// ICE-less offers); the real WebRTC transport is ON by default, so opt out
/// once per test binary. Process env is the only switch the server reads.
fn force_legacy_webrtc_transport() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var("VKS_WEBRTC_TRANSPORT", "0"));
}

/// Helper to run live server on an ephemeral port (composition root: `build_app`).
async fn spawn_live_server() -> SocketAddr {
    force_legacy_webrtc_transport();
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
    let (addr, _state, token) = spawn_live_server_with_auth().await;
    let url = format!("ws://{addr}/ws");

    let ws_stream = connect_ws(&url, Some(&token)).await;
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
            // T08: the media plane is part of the join contract so the SDK can
            // pick its pipeline before the first RoomModeChanged push.
            assert_eq!(res.room_mode, vks_proto::media::RoomMode::Sfu as i32);
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
    let (addr, _state, token) = spawn_live_server_with_auth().await;
    let url = format!("ws://{addr}/ws");

    // Client 1 connects
    let ws_1 = connect_ws(&url, Some(&token)).await;
    let (mut write_1, mut read_1) = ws_1.split();

    // Client 2 connects
    let ws_2 = connect_ws(&url, Some(&token)).await;
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

/// Подключение к `/ws` с опциональным Bearer-токеном (auth-at-handshake, T1.2).
async fn connect_ws(url: &str, token: Option<&str>) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
    let mut request = url.into_client_request().expect("client request");
    if let Some(token) = token {
        request.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {token}").parse().expect("auth header"));
    }
    let (ws, _resp) = connect_async(request).await.expect("connect to websocket");
    ws
}

/// Ожидает отказ WS-handshake и возвращает HTTP-статус отказа (негативные кейсы).
async fn connect_ws_expect_reject(url: &str, token: Option<&str>) -> StatusCode {
    let mut request = url.into_client_request().expect("client request");
    if let Some(token) = token {
        request.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {token}").parse().expect("auth header"));
    }
    match connect_async(request).await {
        Err(WsError::Http(response)) => response.status(),
        Err(other) => panic!("expected HTTP rejection, got {other:?}"),
        Ok(_) => panic!("expected HTTP rejection, but handshake succeeded"),
    }
}

/// T1.2: `/ws` требует валидный Bearer (сетевой сигналинг, fail-closed).
#[tokio::test]
async fn test_signaling_ws_auth_required() {
    let (addr, _state, token) = spawn_live_server_with_auth().await;
    let url = format!("ws://{addr}/ws");

    // Без заголовка — 401, без апгрейда.
    assert_eq!(connect_ws_expect_reject(&url, None).await, StatusCode::UNAUTHORIZED);

    // Битый токен — 401.
    assert_eq!(connect_ws_expect_reject(&url, Some("not-a-jwt")).await, StatusCode::UNAUTHORIZED);

    // Валидный токен — апгрейд и ServerHello.
    let ws_stream = connect_ws(&url, Some(&token)).await;
    let (mut write, mut read) = ws_stream.split();
    write.send(TungsteniteMsg::Binary(client_hello(1, "authed-room", "c1").encode().into())).await.unwrap();
    let reply = read.next().await.unwrap().unwrap();
    let env = Envelope::decode(&reply.into_data()).unwrap();
    assert!(matches!(env.payload(), Some(Payload::ServerHello(_))));
}

/// T1.2: отклонённый JoinRoom (до ClientHello) не создаёт комнату и не считает участника.
#[tokio::test]
async fn test_signaling_join_before_hello_does_not_create_room() {
    let (addr, state, token) = spawn_live_server_with_auth().await;
    let url = format!("ws://{addr}/ws");
    let ws_stream = connect_ws(&url, Some(&token)).await;
    let (mut write, mut read) = ws_stream.split();

    // JoinRoom до ClientHello — FSM отвечает Error, сайд-эффектов быть не должно.
    write.send(TungsteniteMsg::Binary(join_room(1, "ghost-room", "Ghost").encode().into())).await.unwrap();
    let reply = read.next().await.unwrap().unwrap();
    let env = Envelope::decode(&reply.into_data()).unwrap();
    assert!(matches!(env.payload(), Some(Payload::Error(_))), "join before hello must be rejected");
    let ghost = state.rooms.lock().unwrap().values().any(|r| r.name == "ghost-room");
    assert!(!ghost, "rejected JoinRoom must not create a room");

    // Валидный флоу: hello → join → комната создана, participants = 1.
    write.send(TungsteniteMsg::Binary(client_hello(2, "ghost-room", "c1").encode().into())).await.unwrap();
    let _ = read.next().await.unwrap().unwrap();
    write.send(TungsteniteMsg::Binary(join_room(3, "ghost-room", "Alice").encode().into())).await.unwrap();
    let reply = read.next().await.unwrap().unwrap();
    let env = Envelope::decode(&reply.into_data()).unwrap();
    assert!(matches!(env.payload(), Some(Payload::JoinRoomResult(_))));
    let participants = state.rooms.lock().unwrap().values().find(|r| r.name == "ghost-room").map(|r| r.participants);
    assert_eq!(participants, Some(1), "room must exist with exactly one participant");
}

/// T1.2: лимиты сигналинг-сессии — частота (1008) и размер кадра (1009).
#[tokio::test]
async fn test_signaling_ws_limits_close_connection() {
    let (addr, _state, token) = spawn_live_server_with_auth().await;
    let url = format!("ws://{addr}/ws");

    // 1) Частота: >60 сообщений/сек → close с причиной.
    let ws_stream = connect_ws(&url, Some(&token)).await;
    let (mut write, mut read) = ws_stream.split();
    write.send(TungsteniteMsg::Binary(client_hello(1, "rl-room", "c1").encode().into())).await.unwrap();
    for msg_id in 2..=70u64 {
        let spam = client_hello(msg_id, "rl-room", "c1");
        if write.send(TungsteniteMsg::Binary(spam.encode().into())).await.is_err() {
            break;
        }
    }
    let mut close_reason = None;
    for _ in 0..300 {
        match read.next().await {
            Some(Ok(TungsteniteMsg::Close(frame))) => {
                close_reason = Some(format!("{frame:?}"));
                break;
            }
            None | Some(Err(_)) => break,
            Some(Ok(_)) => continue,
        }
    }
    assert!(
        close_reason.as_deref().unwrap_or_default().contains("message rate exceeded"),
        "rate limiter must close with reason, got {close_reason:?}"
    );

    // 2) Размер сообщения: 65 КиБ > лимита 64 КиБ (§2.4) → graceful close 4008.
    let ws_stream = connect_ws(&url, Some(&token)).await;
    let (mut write, mut read) = ws_stream.split();
    write.send(TungsteniteMsg::Binary(vec![0u8; 65 * 1024].into())).await.unwrap();
    let mut close_reason = None;
    for _ in 0..50 {
        match read.next().await {
            Some(Ok(TungsteniteMsg::Close(frame))) => {
                close_reason = Some(format!("{frame:?}"));
                break;
            }
            None | Some(Err(_)) => break,
            Some(Ok(_)) => continue,
        }
    }
    assert!(
        close_reason.as_deref().unwrap_or_default().contains("message too large"),
        "message size limiter must close with 4008 and reason, got {close_reason:?}"
    );
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

    // Регрессия v1: публичные эндпоинты админки доступны и не перехвачены статикой.
    let response = raw_get(addr, "/openapi.json").await;
    assert!(response.starts_with("HTTP/1.1 200"), "v1 openapi regression: {response}");
    assert!(response.contains("application/json"), "openapi content type: {response}");

    // Защищённые эндпоинты v1 требуют JWT: без секрета — строгий fail-closed 503
    // (не 401 и не перехват статикой).
    let response = raw_get(addr, "/api/v1/rooms").await;
    assert!(response.starts_with("HTTP/1.1 503"), "v1 protected regression: {response}");

    // WS-сигналинг без настроенного issuer — тоже fail-closed 503 (T1.2).
    let url = format!("ws://{addr}/ws");
    assert_eq!(
        connect_ws_expect_reject(&url, None).await,
        StatusCode::SERVICE_UNAVAILABLE,
        "ws must fail closed without issuer"
    );
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
    // Оператор в Users-хранилище (как main.rs) — RBAC/профиль видят роль.
    Users::create(&state, "admin", Role::Operator);
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
    let csp_on_profile = resp.headers().get(header::CONTENT_SECURITY_POLICY).is_some();
    let profile = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("profile body");
    let profile: serde_json::Value = serde_json::from_slice(&profile).expect("profile json");
    assert_eq!(profile["user"]["role"], "operator", "login admin must resolve to Operator via Users store");
    // Operator сохраняет wildcard "all": им бандл гейтит «Diagnostics»/«Date and time»
    // (T1.1; калибровку прячет index.html и серверный редирект, не права).
    assert_eq!(profile["user"]["permissions"], serde_json::json!(["all"]));

    // 5. Security-заголовки — app-wide (включая /api/v3): CSP есть и на API-ответе.
    assert!(csp_on_profile, "security headers must be app-wide (build_app layer)");

    // 6. Catch-all → 501 (UI не должен видеть 404 «метод не существует»).
    let resp = app.clone().oneshot(get("/api/v3/definitely/unknown", Some(&token))).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED, "catch-all must answer 501");
}

async fn spawn_live_server_with_auth() -> (SocketAddr, Arc<AppState>, String) {
    force_legacy_webrtc_transport();
    let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
    let mut state = AppState::with_storage(sqlite);
    let issuer = Arc::new(JwtIssuer::new(JwtConfig {
        secret: "auth_sync_secret_123".to_string(),
        ttl_secs: 86_400,
        issuer: "vks-integration".to_string(),
    }));
    let db = Arc::new(LocalDb::new());
    db.register("u-admin", "admin", "admin").expect("register admin");
    // T7: списки комнат гейтятся Moderator+; без записи в Users токен
    // резолвится в Viewer (least privilege) и /ui/sessions отдаёт 403.
    vks_admin::users::Users::bootstrap_operator(&state, "admin");
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

/// Читает следующее сообщение комнаты, пропуская push-раскладки (S9).
///
/// Смена раскладки теперь доезжает до клиентов отдельным `LayoutChanged`,
/// поэтому тесты, ожидающие конкретный push, должны её пропустить.
async fn next_non_layout(
    stream: &mut futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
) -> vks_signaling::envelope::Envelope {
    loop {
        let msg = stream.next().await.unwrap().unwrap();
        let env = vks_signaling::envelope::Envelope::decode(&msg.into_data()).unwrap();
        if matches!(env.payload(), Some(Payload::LayoutChanged(_))) {
            continue;
        }
        return env;
    }
}

async fn reqwest_like_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let body_str = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
        body_str.len()
    );
    stream.write_all(request.as_bytes()).await.expect("write");

    let mut response_buf = Vec::new();
    stream.read_to_end(&mut response_buf).await.expect("read");
    let full = String::from_utf8_lossy(&response_buf).into_owned();
    let status = full
        .lines()
        .next()
        .and_then(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            if parts.len() >= 2 {
                parts[1].parse::<u16>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let body = full.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
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
    let ws_stream = connect_ws(&url, Some(&token)).await;
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
async fn test_m5_gateway_sip_h323_integration() {
    use vks_admin::rooms::Rooms;
    use vks_gateway_h323::H323Codec;
    use vks_gateway_sip::SipCodec;
    use vks_server::GatewayManager;

    let state = Arc::new(AppState::new());
    let room = Rooms::create_async(&state, "gw-conf".to_string()).await.expect("room created");
    let room_id = room.id;

    let gw = GatewayManager::new();

    // 1. T8: без живого listener'а статус честный DISABLED, а не фиктивный READY
    // (раньше `register_uas()`/`H323Gateway::default` выставляли bool без транспорта).
    let sip_st = gw.sip_status();
    assert_eq!(sip_st["status"], "DISABLED");
    assert_eq!(sip_st["registration"], false);
    let h323_st = gw.h323_status();
    assert_eq!(h323_st["status"], "DISABLED");
    assert_eq!(h323_st["registration"], false);

    // 2. Входящий SIP-вызов
    let sip_call = gw
        .incoming_sip_call(&state, "call-sip-1", "alice_sip", &room_id, &[SipCodec::new("PCMU")])
        .expect("sip call landed");
    assert!(sip_call.participant_id.contains("@sip-gateway"));

    // 3. Участник зарегистрирован в RoomHub
    let participants = state.room_hub.list_participants(&room_id);
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0].participant_id, sip_call.participant_id);

    // 4. Входящий H.323-вызов
    let h323_call = gw
        .incoming_h323_call(&state, "call-h323-1", "bob_h323", &room_id, &[H323Codec::new("PCMU")])
        .expect("h323 call landed");
    assert!(h323_call.participant_id.contains("@h323-gateway"));

    // 5. Теперь 2 участника
    let participants = state.room_hub.list_participants(&room_id);
    assert_eq!(participants.len(), 2);

    // 6. Завершение SIP-вызова
    let term_sip = gw.terminate_sip_call(&state, "call-sip-1").expect("sip terminated");
    assert_eq!(term_sip.state, vks_gateway_sip::CallState::Terminated);

    let remaining = state.room_hub.list_participants(&room_id);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].participant_id, h323_call.participant_id);

    // 7. Завершение H.323-вызова
    let term_h323 = gw.terminate_h323_call(&state, "call-h323-1").expect("h323 terminated");
    assert_eq!(term_h323.state, vks_gateway_h323::H323CallState::Terminated);

    let empty = state.room_hub.list_participants(&room_id);
    assert_eq!(empty.len(), 0);
}

#[tokio::test]
async fn test_m5_recorder_lifecycle_integration() {
    use vks_admin::rooms::Rooms;
    use vks_recorder::RecordingMode;
    use vks_server::RecorderManager;

    let state = Arc::new(AppState::new());
    let room = Rooms::create_async(&state, "rec-conf".to_string()).await.expect("room created");
    let room_id = room.id;

    let rec = RecorderManager::new();
    assert!(!rec.is_recording(&room_id));

    // 1. Старт записи
    let rec_id = rec.start_recording(&state, &room_id, RecordingMode::Mcu).expect("start recording");
    // The stable identifier is the RECORD id (one room can be recorded twice
    // across restarts), not the room id.
    assert_ne!(rec_id, *room_id.as_uuid(), "must return the record id");
    assert!(!rec_id.is_nil());
    assert!(rec.is_recording(&room_id));

    // 2. Рекордер отображается в RoomHub как участник с флагом is_recorder
    let participants = state.room_hub.list_participants(&room_id);
    assert_eq!(participants.len(), 1);
    assert!(participants[0].is_recorder);

    // 3. Останов записи
    let meta = rec.stop_recording(&state, &room_id).expect("stop recording");
    assert!(!rec.is_recording(&room_id));
    assert_eq!(meta.room_id, *room_id.as_uuid());
    assert_eq!(meta.mode, RecordingMode::Mcu);

    // 4. Рекордер удален из участников
    let remaining = state.room_hub.list_participants(&room_id);
    assert_eq!(remaining.len(), 0);

    // 5. Метаданные записаны в AppState.records
    let records = state.records.lock().unwrap();
    assert!(records.contains_key(&meta.id.to_string()));
}

#[tokio::test]
async fn test_m5_cluster_leases_and_backpressure() {
    use vks_cluster::{FencingError, LeaseResult, PushResult};
    use vks_server::ClusterManager;

    let cluster = ClusterManager::new("node-alpha");
    assert_eq!(cluster.node_id(), "node-alpha");

    // 1. Приобретение лиза
    let res = cluster.acquire_room("room-100", 10, 30);
    assert_eq!(res, LeaseResult::Acquired);

    // 2. Fencing с правильной эпохой проходит
    assert!(cluster.verify_fencing("room-100", 1, 15).is_ok());

    // 3. Fencing с устаревшей эпохой отклоняется
    let stale_err = cluster.verify_fencing("room-100", 0, 15).unwrap_err();
    assert!(matches!(stale_err, FencingError::StaleEpoch { .. }));

    // 4. Публикация в шину событий кластера
    let push_res = cluster.publish_event("vks.cluster.room.created", vec![1, 2, 3], true);
    assert_eq!(push_res, PushResult::Accepted);

    // 5. Мониторинг статуса кластера
    let status = cluster.status(10);
    assert_eq!(status["node_id"], "node-alpha");
    assert_eq!(status["status"], "ACTIVE");
    assert_eq!(status["owned_rooms_count"], 1);

    // 6. Освобождение лиза
    assert!(cluster.release_room("room-100"));
}

async fn spawn_live_server_with_sfu_udp() -> (SocketAddr, SocketAddr, Arc<AppState>, String) {
    force_legacy_webrtc_transport();
    let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
    let mut state = AppState::with_storage(sqlite);
    let issuer = Arc::new(JwtIssuer::new(JwtConfig {
        secret: "sfu_test_secret_123".to_string(),
        ttl_secs: 86_400,
        issuer: "vks-integration".to_string(),
    }));
    let db = Arc::new(LocalDb::new());
    db.register("u-admin", "admin", "admin").expect("register admin");
    // T7: списки комнат гейтятся Moderator+; без записи в Users токен
    // резолвится в Viewer (least privilege) и /ui/sessions отдаёт 403.
    vks_admin::users::Users::bootstrap_operator(&state, "admin");
    state.jwt_issuer = Some(issuer.clone());
    state.local_db = Some(db);
    let token = issuer.issue("admin", None).expect("issue");

    let state = Arc::new(state);
    let frontend = Frontend::load(&frontend_fixture_dir());
    let sfu_engine = Arc::new(vks_media_sfu::SfuEngine::new());

    let udp_loop = Arc::new(vks_media_sfu::udp_loop::UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind udp loop"));
    let udp_addr = udp_loop.local_addr();

    let sfu_clone = sfu_engine.clone();
    let _task = udp_loop.start_routing_loop(move |src, packet| sfu_clone.route_udp_packet(src, packet.payload));

    let mut handler_config = HandlerConfig::default_test();
    handler_config.candidate_addr = udp_addr.ip().to_string();
    handler_config.candidate_port = udp_addr.port();

    let app = vks_server::build_app_full(state.clone(), &frontend, handler_config, sfu_engine.clone(), Some(udp_loop));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let tcp_addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    (tcp_addr, udp_addr, state, token)
}

#[tokio::test]
async fn test_package_c_real_sfu_session_with_media_loopback_and_room_isolation() {
    let (tcp_addr, udp_addr, _state, token) = spawn_live_server_with_sfu_udp().await;
    let ws_url = format!("ws://{tcp_addr}/ws");

    // 1. Client 1 (Alice) connects over live WebSocket
    let ws_alice = connect_ws(&ws_url, Some(&token)).await;
    let (mut write_alice, mut read_alice) = ws_alice.split();

    let hello_alice = client_hello(1, "room-pkg-c", "alice-client");
    write_alice.send(TungsteniteMsg::Binary(hello_alice.encode().into())).await.unwrap();
    let res_hello = read_alice.next().await.unwrap().unwrap();
    let env_hello = Envelope::decode(&res_hello.into_data()).unwrap();
    assert!(matches!(env_hello.payload(), Some(Payload::ServerHello(_))));

    let join_alice = join_room(2, "room-pkg-c", "Alice");
    write_alice.send(TungsteniteMsg::Binary(join_alice.encode().into())).await.unwrap();
    let res_join = read_alice.next().await.unwrap().unwrap();
    let env_join = Envelope::decode(&res_join.into_data()).unwrap();
    assert!(matches!(env_join.payload(), Some(Payload::JoinRoomResult(_))));

    // Alice binds local UDP socket and negotiates SDP Offer (Video SSRC: 2222)
    let alice_udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let alice_udp_port = alice_udp.local_addr().unwrap().port();

    let sdp_offer_alice = "v=0\r\no=- 1000 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 50000 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 127.0.0.1\r\na=rtpmap:96 VP8/90000\r\na=ssrc:2222 cname:alice\r\n";
    let offer_msg = SignalingMessage {
        msg_id: 3,
        reply_to: 0,
        payload: Some(Payload::SdpOffer(sig::SdpOffer { sdp: sdp_offer_alice.into() })),
    };
    write_alice.send(TungsteniteMsg::Binary(Envelope::new(offer_msg).encode().into())).await.unwrap();
    let res_ans = read_alice.next().await.unwrap().unwrap();
    let env_ans = Envelope::decode(&res_ans.into_data()).unwrap();
    assert!(matches!(env_ans.payload(), Some(Payload::SdpAnswer(_))));

    // Alice sends ICE Candidate with local UDP port -> server registers Alice's endpoint
    let cand_alice = format!("candidate:1 1 UDP 2122260223 127.0.0.1 {alice_udp_port} typ host");
    let cand_msg = SignalingMessage {
        msg_id: 4,
        reply_to: 0,
        payload: Some(Payload::IceCandidate(sig::IceCandidate {
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
            candidate: cand_alice,
            end_of_candidates: true,
        })),
    };
    write_alice.send(TungsteniteMsg::Binary(Envelope::new(cand_msg).encode().into())).await.unwrap();
    let res_cand_alice = read_alice.next().await.unwrap().unwrap();
    let env_cand_alice = Envelope::decode(&res_cand_alice.into_data()).unwrap();
    assert!(matches!(env_cand_alice.payload(), Some(Payload::Ack(_))));

    // 2. Client 2 (Bob) connects over live WebSocket
    let ws_bob = connect_ws(&ws_url, Some(&token)).await;
    let (mut write_bob, mut read_bob) = ws_bob.split();

    let hello_bob = client_hello(1, "room-pkg-c", "bob-client");
    write_bob.send(TungsteniteMsg::Binary(hello_bob.encode().into())).await.unwrap();
    let _ = read_bob.next().await.unwrap().unwrap();

    let join_bob = join_room(2, "room-pkg-c", "Bob");
    write_bob.send(TungsteniteMsg::Binary(join_bob.encode().into())).await.unwrap();
    let res_join_bob = read_bob.next().await.unwrap().unwrap();
    let env_join_bob = Envelope::decode(&res_join_bob.into_data()).unwrap();
    assert!(matches!(env_join_bob.payload(), Some(Payload::JoinRoomResult(_))));

    // Alice receives ParticipantJoined for Bob!
    let alice_notif = read_alice.next().await.unwrap().unwrap();
    let env_notif = Envelope::decode(&alice_notif.into_data()).unwrap();
    assert!(matches!(env_notif.payload(), Some(Payload::ParticipantJoined(_))));

    // Bob binds local UDP socket and negotiates SDP Offer (Video SSRC: 3333)
    let bob_udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bob_udp_port = bob_udp.local_addr().unwrap().port();

    let sdp_offer_bob = "v=0\r\no=- 2000 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 50002 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 127.0.0.1\r\na=rtpmap:96 VP8/90000\r\na=ssrc:3333 cname:bob\r\n";
    let offer_msg_bob = SignalingMessage {
        msg_id: 3,
        reply_to: 0,
        payload: Some(Payload::SdpOffer(sig::SdpOffer { sdp: sdp_offer_bob.into() })),
    };
    write_bob.send(TungsteniteMsg::Binary(Envelope::new(offer_msg_bob).encode().into())).await.unwrap();
    let res_ans_bob = read_bob.next().await.unwrap().unwrap();
    let env_ans_bob = Envelope::decode(&res_ans_bob.into_data()).unwrap();
    assert!(matches!(env_ans_bob.payload(), Some(Payload::SdpAnswer(_))));

    // Alice receives TrackAdded for Bob's video track!
    let alice_track_notif = read_alice.next().await.unwrap().unwrap();
    let env_track_notif = Envelope::decode(&alice_track_notif.into_data()).unwrap();
    assert!(matches!(env_track_notif.payload(), Some(Payload::TrackAdded(_))));

    // Bob sends ICE Candidate with local UDP port -> server registers Bob's endpoint
    let cand_bob = format!("candidate:2 1 UDP 2122260223 127.0.0.1 {bob_udp_port} typ host");
    let cand_msg_bob = SignalingMessage {
        msg_id: 4,
        reply_to: 0,
        payload: Some(Payload::IceCandidate(sig::IceCandidate {
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
            candidate: cand_bob,
            end_of_candidates: true,
        })),
    };
    write_bob.send(TungsteniteMsg::Binary(Envelope::new(cand_msg_bob).encode().into())).await.unwrap();
    let res_cand_bob = read_bob.next().await.unwrap().unwrap();
    let env_cand_bob = Envelope::decode(&res_cand_bob.into_data()).unwrap();
    assert!(matches!(env_cand_bob.payload(), Some(Payload::Ack(_))));

    // Small delay to allow endpoint registration
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Real UDP Media Forwarding: Alice -> Bob
    let mut rtp_from_alice = vec![0x80, 0x60, 0x00, 0x01, 0x00, 0x00, 0x03, 0xe8];
    rtp_from_alice.extend_from_slice(&2222u32.to_be_bytes()); // SSRC 2222
    rtp_from_alice.extend_from_slice(b"ALICE_LIVE_VIDEO_FRAME_BYTES");
    alice_udp.send_to(&rtp_from_alice, udp_addr).await.unwrap();

    let mut buf = [0u8; 1024];
    let (len, from_addr) = tokio::time::timeout(Duration::from_millis(500), bob_udp.recv_from(&mut buf))
        .await
        .expect("Bob timed out waiting for Alice's media packet")
        .expect("bob recv");
    assert_eq!(from_addr, udp_addr, "packet must arrive from server UDP media loop");
    assert_eq!(&buf[..len], &rtp_from_alice[..], "packet payload must match Alice's exact RTP packet");

    // 4. Real UDP Media Forwarding: Bob -> Alice & RTCP PLI verification (M2.6)
    let mut rtp_from_bob = vec![0x80, 0x60, 0x00, 0x02, 0x00, 0x00, 0x07, 0xd0];
    rtp_from_bob.extend_from_slice(&3333u32.to_be_bytes()); // SSRC 3333
    rtp_from_bob.extend_from_slice(b"BOB_LIVE_VIDEO_FRAME_BYTES");
    bob_udp.send_to(&rtp_from_bob, udp_addr).await.unwrap();

    let mut got_bob_media = false;
    let mut got_rtcp_pli = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1000);
    while tokio::time::Instant::now() < deadline {
        let Ok(Ok((len_a, from_addr_a))) =
            tokio::time::timeout(Duration::from_millis(200), alice_udp.recv_from(&mut buf)).await
        else {
            if got_bob_media {
                break;
            }
            continue;
        };
        assert_eq!(from_addr_a, udp_addr);
        // RTCP PLI packet sent by server to Alice requesting keyframe for SSRC 2222
        if len_a == 12 && buf[1] == 206 {
            let ssrc_target = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
            assert_eq!(ssrc_target, 2222, "RTCP PLI must target Alice's video SSRC 2222");
            got_rtcp_pli = true;
            continue;
        }
        if buf[..len_a] == rtp_from_bob[..] {
            got_bob_media = true;
            break;
        }
    }
    assert!(got_rtcp_pli, "Alice must receive RTCP PLI keyframe request triggered by Bob joining");
    assert!(got_bob_media, "Alice must receive Bob's forwarded RTP packet");

    // 5. Anti-spoofing and Room Isolation: Rogue UDP packet from unknown sender
    let rogue_udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut spoofed_pkt = vec![0x80, 0x60, 0x00, 0x03, 0x00, 0x00, 0x0a, 0x00];
    spoofed_pkt.extend_from_slice(&2222u32.to_be_bytes()); // Alice's SSRC spoofed
    spoofed_pkt.extend_from_slice(b"ROGUE_INJECTION_ATTEMPT");
    rogue_udp.send_to(&spoofed_pkt, udp_addr).await.unwrap();

    // Bob must NOT receive this spoofed packet!
    let rogue_res = tokio::time::timeout(Duration::from_millis(100), bob_udp.recv_from(&mut buf)).await;
    assert!(rogue_res.is_err(), "spoofed packet from rogue endpoint must be dropped by anti-spoofing");

    // 6. Bob leaves room -> Alice receives ParticipantLeft and media forwarding terminates
    let leave_msg = SignalingMessage {
        msg_id: 5,
        reply_to: 0,
        payload: Some(Payload::LeaveRoom(sig::LeaveRoom { room_id: "room-pkg-c".into() })),
    };
    write_bob.send(TungsteniteMsg::Binary(Envelope::new(leave_msg).encode().into())).await.unwrap();
    let res_leave_ack = read_bob.next().await.unwrap().unwrap();
    let env_leave_ack = Envelope::decode(&res_leave_ack.into_data()).unwrap();
    assert!(matches!(env_leave_ack.payload(), Some(Payload::Ack(_))));

    let alice_left_notif = read_alice.next().await.unwrap().unwrap();
    let env_left_notif = Envelope::decode(&alice_left_notif.into_data()).unwrap();
    assert!(matches!(env_left_notif.payload(), Some(Payload::ParticipantLeft(_))));

    // Alice sends another packet -> Bob must NOT receive it
    alice_udp.send_to(&rtp_from_alice, udp_addr).await.unwrap();
    let bob_after_leave = tokio::time::timeout(Duration::from_millis(100), bob_udp.recv_from(&mut buf)).await;
    assert!(bob_after_leave.is_err(), "departed participant must no longer receive room media");
}

async fn spawn_live_server_with_mcu_sfu(
    mcu_engine: Arc<vks_media_mcu::McuEngine>,
    sfu_engine: Arc<vks_media_sfu::SfuEngine>,
) -> (SocketAddr, SocketAddr, Arc<AppState>, String) {
    force_legacy_webrtc_transport();
    let mut state = AppState::new();
    let issuer = Arc::new(JwtIssuer::new(vks_auth::jwt::JwtConfig {
        secret: "mcu_pkg_d_secret_456".to_string(),
        ttl_secs: 86_400,
        issuer: "vks-integration".to_string(),
    }));
    let db = Arc::new(LocalDb::new());
    db.register("u-admin", "admin", "admin").expect("register admin");
    // T7: списки комнат гейтятся Moderator+; без записи в Users токен
    // резолвится в Viewer (least privilege) и /ui/sessions отдаёт 403.
    vks_admin::users::Users::bootstrap_operator(&state, "admin");
    state.jwt_issuer = Some(issuer.clone());
    state.local_db = Some(db);
    Users::bootstrap_operator(&state, "admin");
    let token = issuer.issue("admin", None).expect("issue");

    let state = Arc::new(state);
    let frontend = Frontend::load(&frontend_fixture_dir());

    let udp_loop = Arc::new(vks_media_sfu::udp_loop::UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind udp loop"));
    let udp_addr = udp_loop.local_addr();

    let sfu_clone = sfu_engine.clone();
    let _task = udp_loop.start_routing_loop(move |src, packet| sfu_clone.route_udp_packet(src, packet.payload));

    let mut handler_config = HandlerConfig::default_test();
    handler_config.candidate_addr = udp_addr.ip().to_string();
    handler_config.candidate_port = udp_addr.port();

    let app = vks_server::build_app_with_mcu(
        state.clone(),
        &frontend,
        handler_config,
        sfu_engine.clone(),
        mcu_engine.clone(),
        Some(udp_loop),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let tcp_addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    (tcp_addr, udp_addr, state, token)
}

#[tokio::test]
async fn test_package_d_mcu_pipeline_handover_and_pnm_control_events() {
    let mcu_engine = Arc::new(vks_media_mcu::McuEngine::new());
    let sfu_engine = Arc::new(vks_media_sfu::SfuEngine::new());
    let (tcp_addr, _udp_addr, state, token) =
        spawn_live_server_with_mcu_sfu(mcu_engine.clone(), sfu_engine.clone()).await;
    let ws_url = format!("ws://{tcp_addr}/ws");

    // 1. Client 1 (Alice) connects over live WebSocket
    let ws_alice = connect_ws(&ws_url, Some(&token)).await;
    let (mut write_alice, mut read_alice) = ws_alice.split();

    let hello_alice = client_hello(1, "room-pkg-d", "alice-client");
    write_alice.send(TungsteniteMsg::Binary(hello_alice.encode().into())).await.unwrap();
    let res_hello = read_alice.next().await.unwrap().unwrap();
    let env_hello = Envelope::decode(&res_hello.into_data()).unwrap();
    assert!(matches!(env_hello.payload(), Some(Payload::ServerHello(_))));

    let join_alice = join_room(2, "room-pkg-d", "Alice");
    write_alice.send(TungsteniteMsg::Binary(join_alice.encode().into())).await.unwrap();
    let res_join = read_alice.next().await.unwrap().unwrap();
    let env_join = Envelope::decode(&res_join.into_data()).unwrap();
    assert!(matches!(env_join.payload(), Some(Payload::JoinRoomResult(_))));

    let sdp_offer_alice = "v=0\r\no=- 1000 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 50000 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 127.0.0.1\r\na=rtpmap:96 VP8/90000\r\na=ssrc:4001 cname:alice\r\n";
    let offer_msg_alice = SignalingMessage {
        msg_id: 3,
        reply_to: 0,
        payload: Some(Payload::SdpOffer(sig::SdpOffer { sdp: sdp_offer_alice.into() })),
    };
    write_alice.send(TungsteniteMsg::Binary(Envelope::new(offer_msg_alice).encode().into())).await.unwrap();
    let res_ans = read_alice.next().await.unwrap().unwrap();
    let env_ans = Envelope::decode(&res_ans.into_data()).unwrap();
    assert!(matches!(env_ans.payload(), Some(Payload::SdpAnswer(_))));

    // 2. Client 2 (Bob) connects over live WebSocket
    let ws_bob = connect_ws(&ws_url, Some(&token)).await;
    let (mut write_bob, mut read_bob) = ws_bob.split();

    let hello_bob = client_hello(1, "room-pkg-d", "bob-client");
    write_bob.send(TungsteniteMsg::Binary(hello_bob.encode().into())).await.unwrap();
    let _ = read_bob.next().await.unwrap().unwrap();

    let join_bob = join_room(2, "room-pkg-d", "Bob");
    write_bob.send(TungsteniteMsg::Binary(join_bob.encode().into())).await.unwrap();
    let res_join_bob = read_bob.next().await.unwrap().unwrap();
    let env_join_bob = Envelope::decode(&res_join_bob.into_data()).unwrap();
    assert!(matches!(env_join_bob.payload(), Some(Payload::JoinRoomResult(_))));

    // Alice receives ParticipantJoined for Bob
    let alice_notif = read_alice.next().await.unwrap().unwrap();
    let env_notif = Envelope::decode(&alice_notif.into_data()).unwrap();
    assert!(matches!(env_notif.payload(), Some(Payload::ParticipantJoined(_))));

    let sdp_offer_bob = "v=0\r\no=- 2000 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=audio 50002 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 127.0.0.1\r\na=rtpmap:111 opus/48000/2\r\na=ssrc:5001 cname:bob\r\n";
    let offer_msg_bob = SignalingMessage {
        msg_id: 3,
        reply_to: 0,
        payload: Some(Payload::SdpOffer(sig::SdpOffer { sdp: sdp_offer_bob.into() })),
    };
    write_bob.send(TungsteniteMsg::Binary(Envelope::new(offer_msg_bob).encode().into())).await.unwrap();
    let res_ans_bob = read_bob.next().await.unwrap().unwrap();
    let env_ans_bob = Envelope::decode(&res_ans_bob.into_data()).unwrap();
    assert!(matches!(env_ans_bob.payload(), Some(Payload::SdpAnswer(_))));

    // Alice receives TrackAdded for Bob's audio track
    let alice_track_notif = read_alice.next().await.unwrap().unwrap();
    let env_track_notif = Envelope::decode(&alice_track_notif.into_data()).unwrap();
    assert!(matches!(env_track_notif.payload(), Some(Payload::TrackAdded(_))));

    // 3. REST API / Dynamic Roster Verification (M4.1)
    let (code_ab, body_ab) =
        reqwest_like_request(tcp_addr, "GET", "/api/v3/sessions/room-pkg-d/abonents", &token, None).await;
    assert_eq!(code_ab, 200);
    let abonents_json: serde_json::Value = serde_json::from_str(&body_ab).unwrap();
    assert_eq!(abonents_json["count"], 2);
    let connected = abonents_json["connected"].as_array().unwrap();
    assert_eq!(connected.len(), 2);

    let bob_abonent = connected.iter().find(|a| a["display_name"] == "Bob").unwrap();
    let bob_pid = bob_abonent["abonent_id"].as_str().unwrap().to_string();
    let alice_abonent = connected.iter().find(|a| a["display_name"] == "Alice").unwrap();
    let alice_pid = alice_abonent["abonent_id"].as_str().unwrap().to_string();

    assert_eq!(bob_abonent["payload"]["audio_tx_enabled"], true);

    // 4. Operator Commands: Mute Bob audio, Rename Alice, Engine Command (M4.2, M4.4)
    let (code_mute, _) = reqwest_like_request(
        tcp_addr,
        "POST",
        &format!("/api/v3/abonents/{bob_pid}/audio"),
        &token,
        Some(r#"{"enable":false}"#),
    )
    .await;
    assert_eq!(code_mute, 200);

    let (_, body_ab_muted) =
        reqwest_like_request(tcp_addr, "GET", "/api/v3/sessions/room-pkg-d/abonents", &token, None).await;
    let ab_muted_json: serde_json::Value = serde_json::from_str(&body_ab_muted).unwrap();
    let bob_muted = ab_muted_json["connected"].as_array().unwrap().iter().find(|a| a["display_name"] == "Bob").unwrap();
    assert_eq!(bob_muted["payload"]["audio_tx_enabled"], false);

    let (code_rename, _) = reqwest_like_request(
        tcp_addr,
        "PATCH",
        &format!("/api/v3/abonents/{alice_pid}"),
        &token,
        Some(r#"{"display_name":"Alice In Wonderland"}"#),
    )
    .await;
    assert_eq!(code_rename, 200);

    let (code_cmd, body_cmd) =
        reqwest_like_request(tcp_addr, "POST", "/api/v3/engine/command", &token, Some(r#"{"command":"reload"}"#)).await;
    assert_eq!(code_cmd, 200);
    let cmd_json: serde_json::Value = serde_json::from_str(&body_cmd).unwrap();
    assert_eq!(cmd_json["status"], "ok");

    // 5. Live SFU -> MCU Mode Switch Handover (M3.3, M3.8, M4.4).
    // Vendor-контракт требует id раскладки, привязанной к сессии (UI всегда
    // её шлёт). Создаём раскладку и привязываем, иначе сервер честно даёт 422.
    let (code_create_layout, body_create_layout) = reqwest_like_request(
        tcp_addr,
        "POST",
        "/api/v3/ui/video_layouts",
        &token,
        Some(r#"{"display_name":"PkgD Layout","payload":{"mode":"auto","preset":"preset_default_13","type":"default","size":13}}"#),
    )
    .await;
    assert_eq!(code_create_layout, 201, "{body_create_layout}");
    let layout_json: serde_json::Value = serde_json::from_str(&body_create_layout).unwrap();
    let layout_id = layout_json["video_layout"]["id"].as_str().unwrap().to_string();
    let (code_attach, body_attach) = reqwest_like_request(
        tcp_addr,
        "POST",
        &format!("/api/v3/sessions/room-pkg-d/video_layouts/{layout_id}"),
        &token,
        Some("{}"),
    )
    .await;
    assert!(matches!(code_attach, 200 | 204), "{code_attach}: {body_attach}");
    let (code_mode, body_mode) = reqwest_like_request(
        tcp_addr,
        "POST",
        "/api/v3/ui/video_layouts/mode",
        &token,
        Some(&format!(r#"{{"session_id":"room-pkg-d","mode":"mcu","video_layout_id":"{layout_id}"}}"#)),
    )
    .await;
    assert_eq!(code_mode, 200);
    let mode_json: serde_json::Value = serde_json::from_str(&body_mode).unwrap();
    assert_eq!(mode_json["payload"]["mode"], "auto", "топология mcu не меняет режим раскладки");

    // Alice and Bob both receive RoomModeChanged(new_mode: Mcu, switch_epoch: 1) on their live WebSockets!
    // S9: перед этим приходит push-сообщение LayoutChanged (раскладка комнаты
    // теперь доезжает до клиентов), поэтому читаем до RoomModeChanged.
    let alice_mode = match next_non_layout(&mut read_alice).await.payload() {
        Some(Payload::RoomModeChanged(rmc)) => rmc.clone(),
        other => panic!("expected RoomModeChanged for Alice, got: {other:?}"),
    };
    assert_eq!(alice_mode.new_mode, vks_proto::media::RoomMode::Mcu as i32);
    assert_eq!(alice_mode.switch_epoch, 1);

    let bob_mode = match next_non_layout(&mut read_bob).await.payload() {
        Some(Payload::RoomModeChanged(rmc)) => rmc.clone(),
        other => panic!("expected RoomModeChanged for Bob, got: {other:?}"),
    };
    assert_eq!(bob_mode.new_mode, vks_proto::media::RoomMode::Mcu as i32);
    assert_eq!(bob_mode.switch_epoch, 1);

    // Verify engine_abonents populates main participant monitor roster (M4.2)
    let (code_eng, body_eng) = reqwest_like_request(tcp_addr, "GET", "/api/v3/engine/abonents", &token, None).await;
    assert_eq!(code_eng, 200);
    let eng_json: serde_json::Value = serde_json::from_str(&body_eng).unwrap();
    assert_eq!(eng_json["count"], 2);
    assert_eq!(eng_json["abonents"].as_array().unwrap().len(), 2);

    // Verify engine_diagnostics contains required resources for UI fetchEngineDiagnostics (C11, M4.2)
    let (code_diag, body_diag) =
        reqwest_like_request(tcp_addr, "GET", "/api/v3/engine/diagnostics", &token, None).await;
    assert_eq!(code_diag, 200);
    let diag_json: serde_json::Value = serde_json::from_str(&body_diag).unwrap();
    assert_eq!(diag_json["diagnostics"]["vsm"]["resources"]["required"]["encoders"], 0);
    assert_eq!(diag_json["diagnostics"]["vsm"]["resources"]["required"]["decoders"], 0);

    // Verify layout preset change (e.g. "auto") preserves MCU mode without demoting to SFU (M4.5)
    let (code_layout_preset, _) = reqwest_like_request(
        tcp_addr,
        "POST",
        "/api/v3/ui/video_layouts/mode",
        &token,
        Some(&format!(r#"{{"session_id":"room-pkg-d","mode":"auto","video_layout_id":"{layout_id}"}}"#)),
    )
    .await;
    assert_eq!(code_layout_preset, 200);
    let current_room_mode = state.rooms.lock().unwrap().values().find(|r| r.name == "room-pkg-d").unwrap().mode;
    assert_eq!(current_room_mode, Some(vks_core::RoomMode::Mcu), "auto preset must not demote MCU room to SFU");

    // Negative test: 404 on unknown session for video_layouts/mode (truthful errors)
    let (code_bad_room, _) = reqwest_like_request(
        tcp_addr,
        "POST",
        "/api/v3/ui/video_layouts/mode",
        &token,
        Some(r#"{"session_id":"nonexistent-uuid-1234","mode":"mcu"}"#),
    )
    .await;
    assert_eq!(code_bad_room, 404, "unknown session must return 404");

    // Verify abonents/count reflects connections accurately
    let (code_count, body_count) = reqwest_like_request(tcp_addr, "GET", "/api/v3/abonents/count", &token, None).await;
    assert_eq!(code_count, 200);
    let count_json: serde_json::Value = serde_json::from_str(&body_count).unwrap();
    assert_eq!(count_json["connections"]["total"], 2);
    assert_eq!(count_json["connections"]["webrtc"], 2);

    // 6. McuEngine Media Pipeline: Clocked Mix-Minus Audio (20ms cadence) & Compositing (M3.2, M3.4, M3.6, M3.7)
    let room_id = state.rooms.lock().unwrap().values().find(|r| r.name == "room-pkg-d").unwrap().id;
    let alice_pid_core = vks_server::parse_participant_id(&alice_pid);
    let bob_pid_core = vks_server::parse_participant_id(&bob_pid);

    // Provide 960 audio samples (20ms @ 48kHz)
    let alice_pcm = vec![0.4f32; 960];
    let bob_pcm = vec![0.6f32; 960];
    mcu_engine.set_audio(room_id, alice_pid_core, alice_pcm);
    mcu_engine.set_audio(room_id, bob_pid_core, bob_pcm);

    // Verify McuEngine advances audio on 20ms cadence
    let stepped = mcu_engine.tick_audio_at(room_id, 100);
    assert!(stepped, "McuEngine tick_audio_at should succeed on 20ms cadence");

    // Mix-minus check: Alice receives Bob's audio, Bob receives Alice's audio
    let alice_mix_minus = mcu_engine.participant_audio(room_id, alice_pid_core).expect("alice mix-minus");
    assert_eq!(alice_mix_minus.len(), 960);
    assert!(alice_mix_minus[0] > 0.0, "Alice must hear Bob's audio mix-minus");

    let bob_mix_minus = mcu_engine.participant_audio(room_id, bob_pid_core).expect("bob mix-minus");
    assert_eq!(bob_mix_minus.len(), 960);
    assert!(bob_mix_minus[0] > 0.0, "Bob must hear Alice's audio mix-minus");

    // Apply normalized layout transition (M3.7)
    let mut assignments = std::collections::BTreeMap::new();
    assignments.insert(alice_pid_core, 0);
    assignments.insert(bob_pid_core, 1);
    let layout_desc = vks_core::LayoutDescriptor {
        layout_id: "grid_2x2".into(),
        version: 1,
        assignments,
        geometry: Vec::new(),
        preset: "vks_grid_4".into(),
        mode: "manual".into(),
    };
    use vks_core::ports::media_engine::MediaEngine;
    mcu_engine.apply_layout(room_id, layout_desc).await.expect("apply_layout");

    // Tick video composite
    let frame = mcu_engine.tick_video_at(room_id, 100);
    assert!(frame.is_some(), "McuEngine composite video frame rendered");

    // Render stats
    let stats = mcu_engine.stats(room_id).expect("stats");
    assert!(stats.frames_rendered >= 1);

    // 7. Operator Forced Kick (M4.3)
    let (code_kick, _) =
        reqwest_like_request(tcp_addr, "DELETE", &format!("/api/v3/abonents/{bob_pid}"), &token, None).await;
    assert_eq!(code_kick, 204);

    // Bob receives ParticipantLeft { reason: 2 (kicked) } followed by WebSocket CloseFrame(4003)
    let env_bob_kick = next_non_layout(&mut read_bob).await;
    match env_bob_kick.payload() {
        Some(Payload::ParticipantLeft(left)) => {
            assert_eq!(left.participant_id, bob_pid);
            assert_eq!(left.reason, 2, "reason 2 indicates kicked by operator");
        }
        other => panic!("expected ParticipantLeft for Bob, got: {other:?}"),
    }

    let bob_close = read_bob.next().await.unwrap().unwrap();
    assert!(
        matches!(bob_close, TungsteniteMsg::Close(Some(ref frame)) if frame.code == 4003.into()),
        "Bob must receive CloseFrame 4003"
    );

    // Alice receives ParticipantLeft for Bob
    let env_alice_left = next_non_layout(&mut read_alice).await;
    match env_alice_left.payload() {
        Some(Payload::ParticipantLeft(left)) => {
            assert_eq!(left.participant_id, bob_pid);
        }
        other => panic!("expected ParticipantLeft on Alice socket, got: {other:?}"),
    }

    // Dynamic roster count drops to 1
    let (_, body_after_kick) =
        reqwest_like_request(tcp_addr, "GET", "/api/v3/sessions/room-pkg-d/abonents", &token, None).await;
    let ab_after_kick: serde_json::Value = serde_json::from_str(&body_after_kick).unwrap();
    assert_eq!(ab_after_kick["count"], 1);

    // S9: поздний участник получает снимок раскладки в самом JoinRoomResult —
    // без отдельного push-кадра, который мог прийти позже ответа.
    let ws_carol = connect_ws(&ws_url, Some(&token)).await;
    let (mut write_carol, mut read_carol) = ws_carol.split();
    let hello_carol = client_hello(1, "room-pkg-d", "carol-client");
    write_carol.send(TungsteniteMsg::Binary(hello_carol.encode().into())).await.unwrap();
    let _ = read_carol.next().await.unwrap().unwrap();
    let join_carol = join_room(2, "room-pkg-d", "Carol");
    write_carol.send(TungsteniteMsg::Binary(join_carol.encode().into())).await.unwrap();
    let res_join_carol = read_carol.next().await.unwrap().unwrap();
    let env_join_carol = Envelope::decode(&res_join_carol.into_data()).unwrap();
    match env_join_carol.payload() {
        Some(Payload::JoinRoomResult(res)) => {
            let layout = res.layout.as_ref().expect("поздний участник получает снимок раскладки");
            assert!(!layout.slots.is_empty(), "снимок раскладки непуст");
            // Позитивный случай третьего производителя: снимок `JoinRoomResult`
            // несёт те же метаданные, что и применённый дескриптор (T10a).
            assert_eq!(layout.mode, "manual", "снимок позднего участника несёт режим");
            assert_eq!(layout.preset, "vks_grid_4", "снимок несёт raw preset");
            assert_eq!(layout.slots[0].id, 0, "slot id — 0-based");
            let rect = layout.slots[0].rect.as_ref().expect("нормализованный rect");
            assert!(rect.w > 0.0 && rect.h > 0.0, "у слота есть размер");
            // T08: per-room MCU override must be visible on the wire, not only
            // the process-wide HandlerConfig value.
            assert_eq!(res.room_mode, vks_proto::media::RoomMode::Mcu as i32, "per-room mode wins");
        }
        other => panic!("expected JoinRoomResult, got {other:?}"),
    }
}

#[tokio::test]
async fn test_m5_gateway_dual_call_same_from_isolation_and_mcu_bridging() {
    use vks_admin::rooms::Rooms;
    use vks_admin::state::AppState;
    use vks_gateway_h323::{H323Codec, H323Gateway};
    use vks_gateway_sip::{SipCodec, SipGateway};
    use vks_media_mcu::McuEngine;
    use vks_server::GatewayManager;

    let state = Arc::new(AppState::new());
    let room_1 = Rooms::create_async(&state, "gw-iso-1".to_string()).await.expect("room 1 created");
    let room_2 = Rooms::create_async(&state, "gw-iso-2".to_string()).await.expect("room 2 created");

    let mcu = Arc::new(McuEngine::new());
    let gw = GatewayManager::new().with_mcu(Arc::clone(&mcu));

    // 1. T8: in-memory register/unregister — не транспорт и не факт статуса.
    let sip_st = gw.sip_status();
    assert_eq!(sip_st["status"], "DISABLED");
    assert_eq!(sip_st["connected"], false);
    assert_eq!(sip_st["active_calls"], 0);
    gw.sip().lock().unwrap().unregister();
    assert_eq!(gw.sip_status()["status"], "DISABLED", "unregister не делает шлюз доступнее");
    gw.sip().lock().unwrap().register_uas().expect("register_uas");
    assert_eq!(gw.sip_status()["status"], "DISABLED", "register_uas без listener'а не READY");

    // 2. Strict codec offer rejection: unsupported codec returns Err
    let mut sip_gw = SipGateway::new();
    let unsupported_res = sip_gw.on_invite("call-bad", "alice", &[SipCodec::new("G729")]);
    assert!(unsupported_res.is_err(), "Unsupported SIP codec offer must be rejected");

    let mut h323_gw = H323Gateway::new();
    let unsupported_h323 = h323_gw.on_setup("call-bad-h323", "bob", &[H323Codec::new("UNSUPPORTED_CODEC")]);
    assert!(unsupported_h323.is_err(), "Unsupported H.323 codec offer must be rejected");

    // 3. Same 'from' ("alice") places two calls: one to room_1, one to room_2
    let call_sip_1 = gw
        .incoming_sip_call(&state, "call-sip-1", "alice", &room_1.id, &[SipCodec::new("PCMU")])
        .expect("call 1 accepted");
    let call_sip_2 = gw
        .incoming_sip_call(&state, "call-sip-2", "alice", &room_2.id, &[SipCodec::new("PCMU")])
        .expect("call 2 accepted");

    assert_ne!(call_sip_1.participant_id, call_sip_2.participant_id);
    assert_eq!(state.room_hub.list_participants(&room_1.id).len(), 1);
    assert_eq!(state.room_hub.list_participants(&room_2.id).len(), 1);
    assert_eq!(gw.sip_status()["active_calls"], 2);

    // Verify MCU engine bridging
    let pid_1 = vks_server::parse_participant_id(&call_sip_1.participant_id);
    let pid_2 = vks_server::parse_participant_id(&call_sip_2.participant_id);
    assert!(mcu.has_participant(room_1.id, pid_1));
    assert!(mcu.has_participant(room_2.id, pid_2));

    // 4. Terminate call 1: room 1 drops to 0, but room 2 MUST stay intact (no cross-room ejection)
    let term_1 = gw.terminate_sip_call(&state, "call-sip-1").expect("term call 1");
    assert_eq!(term_1.call_id, "call-sip-1");
    assert_eq!(state.room_hub.list_participants(&room_1.id).len(), 0);
    assert_eq!(state.room_hub.list_participants(&room_2.id).len(), 1);
    assert_eq!(state.room_hub.list_participants(&room_2.id)[0].participant_id, call_sip_2.participant_id);
    assert!(!mcu.has_participant(room_1.id, pid_1));
    assert!(mcu.has_participant(room_2.id, pid_2));
    assert_eq!(gw.sip_status()["active_calls"], 1);

    // 5. Terminate call 2 cleanly
    let term_2 = gw.terminate_sip_call(&state, "call-sip-2").expect("term call 2");
    assert_eq!(term_2.call_id, "call-sip-2");
    assert_eq!(state.room_hub.list_participants(&room_2.id).len(), 0);
    assert!(!mcu.has_participant(room_2.id, pid_2));
    assert_eq!(gw.sip_status()["active_calls"], 0);
}

#[tokio::test]
async fn test_m5_recorder_idempotent_stop_persistent_storage_and_playable_container() {
    use vks_admin::rooms::Rooms;
    use vks_admin::state::AppState;
    use vks_core::ports::Storage;
    use vks_recorder::container::{ContainerConfig, PlayableContainerWriter};
    use vks_recorder::RecordingMode;
    use vks_server::RecorderManager;

    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let state = Arc::new(AppState::with_storage(storage.clone()));
    let room = Rooms::create_async(&state, "rec-durable".to_string()).await.expect("room created");
    let room_id = room.id;

    let rec = RecorderManager::new();
    let rec_id = rec.start_recording(&state, &room_id, RecordingMode::Mcu).expect("start recording");
    assert_ne!(rec_id, *room_id.as_uuid(), "start returns the record id");
    assert!(!rec_id.is_nil());
    assert!(rec.is_recording(&room_id));

    // First stop
    let meta_1 = rec.stop_recording_async(&state, &room_id).await.expect("first stop");
    assert!(!rec.is_recording(&room_id));
    assert_eq!(meta_1.room_id, *room_id.as_uuid());

    // Second stop: MUST BE IDEMPOTENT (returns cached metadata without error or drift)
    let meta_2 = rec.stop_recording_async(&state, &room_id).await.expect("second stop idempotent");
    assert_eq!(meta_1.id, meta_2.id);
    assert_eq!(meta_1.duration_ms, meta_2.duration_ms);
    assert_eq!(meta_1.started_at_ms, meta_2.started_at_ms);
    assert_eq!(meta_1.ended_at_ms, meta_2.ended_at_ms);

    // Verify durable persistence in Storage port
    let stored = storage.get_record(&meta_1.id.to_string()).await.expect("fetch from storage");
    assert!(stored.is_some(), "RecordEntry must be persisted durably");
    let entry = stored.unwrap();
    assert_eq!(entry.id, meta_1.id.to_string());
    assert_eq!(entry.room_id, room_id);

    // Verify PlayableContainerWriter generates valid ISOBMFF container with complete sample tables
    let config = ContainerConfig {
        width: 1280,
        height: 720,
        fps: 30,
        sample_rate: 48000,
        channels: 2,
        max_size_bytes: 5 * 1024 * 1024,
    };
    let mut writer = PlayableContainerWriter::new(config);
    writer.write_video_sample(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88], 0, true).unwrap();
    writer.write_audio_sample(&[0xf8, 0xff, 0xfe], 20).unwrap();
    writer.write_video_sample(&[0x00, 0x00, 0x00, 0x01, 0x41, 0x99], 33, false).unwrap();
    writer.write_audio_sample(&[0xf8, 0xff, 0xfd], 40).unwrap();
    let mp4_bytes = writer.finalize().unwrap();

    assert!(mp4_bytes.len() > 64);
    assert_eq!(&mp4_bytes[4..8], b"ftyp");
    assert!(mp4_bytes.windows(4).any(|w| w == b"moov"));
    assert!(mp4_bytes.windows(4).any(|w| w == b"mdat"));
    assert!(mp4_bytes.windows(4).any(|w| w == b"stts"));
    assert!(mp4_bytes.windows(4).any(|w| w == b"stss"));
    assert!(mp4_bytes.windows(4).any(|w| w == b"stsc"));
    assert!(mp4_bytes.windows(4).any(|w| w == b"stsz"));
    assert!(mp4_bytes.windows(4).any(|w| w == b"stco"));

    // Save and retrieve container artifact in RecorderManager
    rec.save_container(&meta_1.id.to_string(), mp4_bytes.clone());
    let fetched = rec.get_container(&meta_1.id.to_string());
    assert_eq!(fetched, Some(mp4_bytes));
}

#[tokio::test]
async fn test_m5_cluster_monotonic_fencing_epoch_and_graceful_migration() {
    use vks_cluster::{FencingError, LeaseResult};
    use vks_server::ClusterManager;

    let cluster_a = ClusterManager::with_heartbeat_deadline("node-a", 10);
    let cluster_b = ClusterManager::with_shared(
        "node-b",
        Arc::clone(cluster_a.owner()),
        Arc::clone(cluster_a.registry()),
        Arc::clone(cluster_a.bus()),
    );

    // Initial status: ACTIVE when registered
    cluster_a.send_heartbeat(0);
    assert_eq!(cluster_a.status(0)["status"], "ACTIVE");

    // 1. node-a acquires room-mig-1 at tick 0 -> epoch 1
    let r1 = cluster_a.acquire_room("room-mig-1", 0, 30);
    assert_eq!(r1, LeaseResult::Acquired);
    assert!(cluster_a.verify_fencing("room-mig-1", 1, 5).is_ok());

    // 2. node-a releases room-mig-1
    assert!(cluster_a.release_room("room-mig-1"));

    // 3. node-b acquires room-mig-1 at tick 10 -> epoch must be 2 (never reset to 1)
    let r2 = cluster_b.acquire_room("room-mig-1", 10, 30);
    assert_eq!(r2, LeaseResult::Acquired);
    assert!(cluster_b.verify_fencing("room-mig-1", 2, 15).is_ok());

    // Stale epoch 1 rejected
    assert_eq!(
        cluster_b.verify_fencing("room-mig-1", 1, 15),
        Err(FencingError::StaleEpoch { room_id: "room-mig-1".into(), current: 2, provided: 1 })
    );

    // 4. Graceful migration from node-b to node-c: epoch advances to 3
    let lease_c = cluster_b.migrate_room("room-mig-1", "node-c", 20, 30).expect("migrate to node-c");
    assert_eq!(lease_c.owner, "node-c");
    assert_eq!(lease_c.epoch, 3);
    assert_eq!(lease_c.expiry_tick, 50);

    // 5. Configurable heartbeat deadline and dead node pruning
    let cluster_monitor = ClusterManager::with_heartbeat_deadline("monitor-1", 5);
    cluster_monitor.registry().lock().unwrap().heartbeat("dead-node", 0);
    assert!(cluster_monitor.registry().lock().unwrap().is_alive("dead-node", 2));
    assert!(cluster_monitor.registry().lock().unwrap().is_dead("dead-node", 10));

    // send_heartbeat prunes dead nodes
    cluster_monitor.send_heartbeat(10);
    assert!(!cluster_monitor.registry().lock().unwrap().all().contains(&"dead-node".to_string()));

    // Pruned/unregistered node reports DEGRADED
    let degraded_status = cluster_monitor.status(10);
    assert_eq!(degraded_status["status"], "ACTIVE"); // monitor-1 is active
    let dead_cluster = ClusterManager::with_heartbeat_deadline("dead-cluster", 5);
    dead_cluster.send_heartbeat(0);
    assert_eq!(dead_cluster.status(20)["status"], "DEGRADED"); // expired heartbeat is DEGRADED
}

#[tokio::test]
async fn test_step3_gateway_dial_outbound_sip_uac_media_bridge() {
    use tokio::net::UdpSocket;
    use vks_admin::rooms::Rooms;
    use vks_admin::state::OutboundDialer;
    use vks_media_mcu::McuEngine;
    use vks_server::GatewayManager;

    // 1. Создаем мок SIP терминал на 127.0.0.1
    let term_sip_sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind term sip");
    let term_sip_addr = term_sip_sock.local_addr().unwrap();

    let term_rtp_sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind term rtp");
    let term_rtp_port = term_rtp_sock.local_addr().unwrap().port();

    let term_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        // 1. Ожидаем INVITE
        let (len, src) = term_sip_sock.recv_from(&mut buf).await.expect("recv invite");
        let req = String::from_utf8_lossy(&buf[..len]);
        assert!(req.starts_with("INVITE"));
        assert!(req.contains("Call-ID:"));

        // Извлекаем Call-ID, From, Via, CSeq
        let call_id = req.lines().find(|l| l.starts_with("Call-ID:")).map(|l| l[8..].trim().to_string()).unwrap();
        let from = req.lines().find(|l| l.starts_with("From:")).map(|l| l[5..].trim().to_string()).unwrap();
        let via = req.lines().find(|l| l.starts_with("Via:")).map(|l| l[4..].trim().to_string()).unwrap();
        let cseq = req.lines().find(|l| l.starts_with("CSeq:")).map(|l| l[5..].trim().to_string()).unwrap();

        // Отправляем 180 Ringing
        let ringing = format!(
            "SIP/2.0 180 Ringing\r\nVia: {via}\r\nFrom: {from}\r\nTo: <sip:127.0.0.1>;tag=term-tag-123\r\nCall-ID: {call_id}\r\nCSeq: {cseq}\r\nContent-Length: 0\r\n\r\n"
        );
        term_sip_sock.send_to(ringing.as_bytes(), src).await.unwrap();

        // Формируем 200 OK с SDP Answer
        let sdp_answer = format!(
            "v=0\r\no=term 1 1 IN IP4 127.0.0.1\r\ns=Session\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n",
            term_rtp_port
        );
        let ok_resp = format!(
            "SIP/2.0 200 OK\r\nVia: {via}\r\nFrom: {from}\r\nTo: <sip:127.0.0.1>;tag=term-tag-123\r\nCall-ID: {call_id}\r\nCSeq: {cseq}\r\nContact: <sip:term@127.0.0.1:{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            term_sip_addr.port(),
            sdp_answer.len(),
            sdp_answer
        );
        term_sip_sock.send_to(ok_resp.as_bytes(), src).await.unwrap();

        // 2. Ожидаем ACK
        let (ack_len, _) = term_sip_sock.recv_from(&mut buf).await.expect("recv ack");
        let ack_str = String::from_utf8_lossy(&buf[..ack_len]);
        assert!(ack_str.starts_with("ACK"));

        // 3. Ожидаем BYE в конце
        let (bye_len, bye_src) = term_sip_sock.recv_from(&mut buf).await.expect("recv bye");
        let bye_str = String::from_utf8_lossy(&buf[..bye_len]);
        assert!(bye_str.starts_with("BYE"));

        let bye_via = bye_str.lines().find(|l| l.starts_with("Via:")).map(|l| l[4..].trim().to_string()).unwrap();
        let bye_from = bye_str.lines().find(|l| l.starts_with("From:")).map(|l| l[5..].trim().to_string()).unwrap();
        let bye_to = bye_str.lines().find(|l| l.starts_with("To:")).map(|l| l[3..].trim().to_string()).unwrap();
        let bye_cseq = bye_str.lines().find(|l| l.starts_with("CSeq:")).map(|l| l[5..].trim().to_string()).unwrap();
        let bye_ok = format!(
            "SIP/2.0 200 OK\r\nVia: {bye_via}\r\nFrom: {bye_from}\r\nTo: {bye_to}\r\nCall-ID: {call_id}\r\nCSeq: {bye_cseq}\r\nContent-Length: 0\r\n\r\n"
        );
        term_sip_sock.send_to(bye_ok.as_bytes(), bye_src).await.unwrap();
    });

    // 2. Ожидаем RTP пакеты от сервера на мок терминале
    let rtp_task = tokio::spawn(async move {
        let mut rtp_buf = [0u8; 1500];
        // Должен прийти хотя бы 1 RTP пакет с тишиной (µ-law 0xFF) от сервера
        let (rtp_len, rtp_src) = tokio::time::timeout(Duration::from_secs(3), term_rtp_sock.recv_from(&mut rtp_buf))
            .await
            .expect("timeout waiting for server RTP")
            .expect("recv rtp packet");

        assert!(rtp_len >= 12, "RTP packet must have at least 12-byte header");
        let payload_type = rtp_buf[1] & 0x7F;
        assert_eq!(payload_type, 0, "Payload type must be PCMU (0)");
        assert_eq!(rtp_buf[12], 0xFF, "Comfort noise must be 0xFF for µ-law silence");

        // Отправляем ответный RTP пакет серверу
        let mut resp_rtp = [0u8; 172];
        vks_gateway_sip::write_rtp_header(&mut resp_rtp[..12], 0, 100, 1000, 0x12345678, false);
        resp_rtp[12..].fill(0xFF);
        let _ = term_rtp_sock.send_to(&resp_rtp, rtp_src).await;
    });

    // 3. Серверная часть
    let admin_state = Arc::new(AppState::new());
    let room = Rooms::create(&admin_state, "Room for Dial Test");
    let mcu_engine = Arc::new(McuEngine::new());
    let gateway_manager = Arc::new(GatewayManager::new().with_mcu(mcu_engine.clone()).with_candidate_ip("127.0.0.1"));
    gateway_manager.bind_state(Arc::downgrade(&admin_state));
    admin_state.set_dialer(gateway_manager.clone());

    let target_uri = format!("sip:127.0.0.1:{}", term_sip_addr.port());
    let dial_res = gateway_manager.dial(room.id, &target_uri, Some("Test Terminal 132")).await;
    assert!(dial_res.is_ok(), "dial must succeed: {:?}", dial_res);

    let pid = dial_res.unwrap();
    assert!(pid.starts_with("sip-"));

    // Проверяем, что участник зарегистрирован в RoomHub
    let participants = admin_state.room_hub.list_participants(&room.id);
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0].participant_id, pid);
    assert_eq!(participants[0].display_name, "Test Terminal 132");

    // Ждем подтверждения RTP
    rtp_task.await.expect("rtp_task completed successfully");

    // 4. Завершаем вызов через hangup
    let hung_up = gateway_manager.hangup(&pid);
    assert!(hung_up, "hangup must return true");

    // Ждем завершения term_task
    let _ = tokio::time::timeout(Duration::from_secs(3), term_task).await.expect("term_task finished");
}

#[tokio::test]
async fn test_step4_gateway_media_controls_dtmf_rtp() {
    use tokio::net::UdpSocket;
    use vks_admin::rooms::Rooms;
    use vks_admin::state::OutboundDialer;
    use vks_media_mcu::McuEngine;
    use vks_server::GatewayManager;

    let term_sip_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind mock sip"));
    let term_sip_addr = term_sip_sock.local_addr().unwrap();

    let term_rtp_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind mock rtp"));
    let term_rtp_addr = term_rtp_sock.local_addr().unwrap();

    let sip_sock_clone = term_sip_sock.clone();
    let term_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        let (len, src) = sip_sock_clone.recv_from(&mut buf).await.expect("recv invite");
        let invite_str = String::from_utf8_lossy(&buf[..len]);
        assert!(invite_str.starts_with("INVITE"));

        let via = invite_str.lines().find(|l| l.starts_with("Via:")).map(|l| l[4..].trim().to_string()).unwrap();
        let from = invite_str.lines().find(|l| l.starts_with("From:")).map(|l| l[5..].trim().to_string()).unwrap();
        let to = invite_str.lines().find(|l| l.starts_with("To:")).map(|l| l[3..].trim().to_string()).unwrap();
        let call_id =
            invite_str.lines().find(|l| l.starts_with("Call-ID:")).map(|l| l[8..].trim().to_string()).unwrap();
        let cseq = invite_str.lines().find(|l| l.starts_with("CSeq:")).map(|l| l[5..].trim().to_string()).unwrap();

        let sdp_answer = format!(
            "v=0\r\no=- 2000 2000 IN IP4 127.0.0.1\r\ns=MockTerminal\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\n",
            term_rtp_addr.port()
        );

        let ok_resp = format!(
            "SIP/2.0 200 OK\r\nVia: {via}\r\nFrom: {from}\r\nTo: {to};tag=mocktag\r\nCall-ID: {call_id}\r\nCSeq: {cseq}\r\nContact: <sip:127.0.0.1:{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            sip_sock_clone.local_addr().unwrap().port(),
            sdp_answer.len(),
            sdp_answer
        );
        sip_sock_clone.send_to(ok_resp.as_bytes(), src).await.unwrap();

        let (ack_len, _) = sip_sock_clone.recv_from(&mut buf).await.expect("recv ack");
        let ack_str = String::from_utf8_lossy(&buf[..ack_len]);
        assert!(ack_str.starts_with("ACK"));

        let (bye_len, bye_src) = sip_sock_clone.recv_from(&mut buf).await.expect("recv bye");
        let bye_str = String::from_utf8_lossy(&buf[..bye_len]);
        assert!(bye_str.starts_with("BYE"));

        let bye_via = bye_str.lines().find(|l| l.starts_with("Via:")).map(|l| l[4..].trim().to_string()).unwrap();
        let bye_from = bye_str.lines().find(|l| l.starts_with("From:")).map(|l| l[5..].trim().to_string()).unwrap();
        let bye_to = bye_str.lines().find(|l| l.starts_with("To:")).map(|l| l[3..].trim().to_string()).unwrap();
        let bye_cseq = bye_str.lines().find(|l| l.starts_with("CSeq:")).map(|l| l[5..].trim().to_string()).unwrap();
        let bye_ok = format!(
            "SIP/2.0 200 OK\r\nVia: {bye_via}\r\nFrom: {bye_from}\r\nTo: {bye_to}\r\nCall-ID: {call_id}\r\nCSeq: {bye_cseq}\r\nContent-Length: 0\r\n\r\n"
        );
        sip_sock_clone.send_to(bye_ok.as_bytes(), bye_src).await.unwrap();
    });

    let admin_state = Arc::new(AppState::new());
    let room = Rooms::create(&admin_state, "Room for DTMF Test");
    let mcu_engine = Arc::new(McuEngine::new());
    let gateway_manager = Arc::new(GatewayManager::new().with_mcu(mcu_engine.clone()).with_candidate_ip("127.0.0.1"));
    gateway_manager.bind_state(Arc::downgrade(&admin_state));
    admin_state.set_dialer(gateway_manager.clone());

    let target_uri = format!("sip:127.0.0.1:{}", term_sip_addr.port());
    let pid = gateway_manager.dial(room.id, &target_uri, Some("DTMF Terminal")).await.expect("dial");

    // Тестируем set_media_control (Mute & Volume)
    let ctrl_res = gateway_manager.set_media_control(&pid, Some(true), Some(false), None, None, Some(85), Some(110));
    assert!(ctrl_res, "set_media_control must succeed");

    // Отправляем DTMF "5"
    let dtmf_res = gateway_manager.send_dtmf(&pid, "5");
    assert!(dtmf_res.is_ok(), "send_dtmf must succeed");

    // Ожидаем DTMF пакет на сокете терминала
    let mut rtp_buf = [0u8; 1500];
    let mut got_dtmf = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);

    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok((len, _))) =
            tokio::time::timeout(Duration::from_millis(200), term_rtp_sock.recv_from(&mut rtp_buf)).await
        {
            if len >= 16 {
                let pt = rtp_buf[1] & 0x7F;
                if pt == 101 {
                    let event = rtp_buf[12];
                    assert_eq!(event, 5, "DTMF event digit 5");
                    got_dtmf = true;
                    break;
                }
            }
        }
    }
    assert!(got_dtmf, "Terminal must receive RFC 4733 telephone-event (PT 101) for DTMF digit 5");

    let fecc_res = gateway_manager.send_fecc(&pid, "pan_left");
    assert!(
        fecc_res.unwrap_err().contains("not negotiated"),
        "A gateway without an H.224/FECC channel must report unsupported instead of acknowledging a fictitious camera move"
    );

    let hung_up = gateway_manager.hangup(&pid);
    assert!(hung_up, "hangup must return true");
    let _ = tokio::time::timeout(Duration::from_secs(3), term_task).await.expect("term_task finished");
}

#[tokio::test]
async fn test_step5_h323_rejects_invented_faststart_eof_and_timeout() {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use vks_admin::rooms::Rooms;
    use vks_admin::state::OutboundDialer;
    use vks_gateway_h323::q931::{encode_tpkt, parse_q931_message, MSG_SETUP};
    use vks_gateway_h323::H323UacClient;
    use vks_media_mcu::McuEngine;
    use vks_server::GatewayManager;

    // This is a real sidecar boundary test, not a replacement fake dialer.
    // Fail with an actionable dependency message instead of silently passing
    // when a build host has not installed its separately pinned ASN.1 runtime.
    let python = std::env::var_os("VKS_H323_PYTHON").map(PathBuf::from).unwrap_or_else(|| {
        let installed = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local/share/vks-mcu/h323-venv/bin/python3"));
        installed
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }))
    });
    let dependency = std::process::Command::new(&python)
        .args(["-c", "import importlib.metadata,pycrate_asn1rt;assert importlib.metadata.version('pycrate')=='0.8.1'"])
        .output()
        .expect("H323 regression requires Python; set VKS_H323_PYTHON and install python/requirements.lock");
    assert!(dependency.status.success(), "H323 regression requires installed Pycrate0.8.1; run crates/vks-gateway-h323/python/install_runtime.py or use requirements.lock. stderr: {}", String::from_utf8_lossy(&dependency.stderr));

    async fn receive_setup(socket: &mut tokio::net::TcpStream) -> u16 {
        let mut header = [0u8; 4];
        socket.read_exact(&mut header).await.expect("complete TPKT header");
        let length = u16::from_be_bytes([header[2], header[3]]) as usize;
        assert!((9..=65535).contains(&length), "bounded TPKT SETUP");
        let mut packet = header.to_vec();
        packet.resize(length, 0);
        socket.read_exact(&mut packet[4..]).await.expect("complete TPKT SETUP");
        let setup = parse_q931_message(&packet).expect("strict Q931 framing");
        assert_eq!(setup.message_type, MSG_SETUP);
        assert!(setup.h225_user_information.is_some(), "real ASN.1 SETUP was sent");
        setup.call_reference
    }

    for invented_faststart in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback terminal");
        let address = listener.local_addr().unwrap();
        let terminal = tokio::spawn(async move {
            // Запас на холодный старт python-sidecar (импорт pycrate): тест
            // проверяет контракт негативного сценария, а не скорость спавна.
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
                .await
                .expect("sidecar reached terminal")
                .expect("accept");
            let call_ref = receive_setup(&mut socket).await;
            if invented_faststart {
                // Well-framed Q931, but the old implementation invented this
                // 0xa0 structure instead of encoding H225/H245 aligned PER.
                let opaque = [5, 0xa0, 8, 1, 0, 127, 0, 0, 1, 0x30, 0x39];
                let mut packet = vec![8, 2];
                packet.extend_from_slice(&(call_ref | 0x8000).to_be_bytes());
                packet.extend_from_slice(&[7, 0x7e]);
                packet.extend_from_slice(&(opaque.len() as u16).to_be_bytes());
                packet.extend_from_slice(&opaque);
                socket.write_all(&encode_tpkt(&packet)).await.expect("write invalid PER CONNECT");
            } else {
                let mut alerting = vec![8, 2];
                alerting.extend_from_slice(&(call_ref | 0x8000).to_be_bytes());
                alerting.push(1);
                socket.write_all(&encode_tpkt(&alerting)).await.expect("write ALERTING");
            }
            socket.shutdown().await.expect("terminal EOF");
        });
        let state = Arc::new(AppState::new());
        let room = Rooms::create(&state, "H323 negative interoperability regression");
        let gateway =
            Arc::new(GatewayManager::new().with_mcu(Arc::new(McuEngine::new())).with_candidate_ip("127.0.0.1"));
        gateway.bind_state(Arc::downgrade(&state));
        state.set_dialer(gateway.clone());
        let result = tokio::time::timeout(
            Duration::from_secs(6),
            gateway.dial(room.id, &format!("h323:{address}"), Some("Invalid H323 peer")),
        )
        .await
        .expect("failure must be bounded");
        assert!(result.is_err(), "private 0xa0/ALERTING+EOF must never create a connected call");
        assert!(state.room_hub.list_participants(&room.id).is_empty(), "failed H323 dial must not leave a participant");
        terminal.await.expect("negative terminal completed");
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("timeout terminal");
    let address = listener.local_addr().unwrap();
    let terminal = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept timeout peer");
        receive_setup(&mut socket).await;
        // Keep TCP open without CONNECT. Real H225 setup/media timeout must
        // fail; an absent media endpoint must never become guessed UDP5004.
        tokio::time::sleep(Duration::from_secs(4)).await;
    });
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        H323UacClient::new("127.0.0.1").dial(&format!("h323:{address}"), 45120, Duration::from_millis(600)),
    )
    .await
    .expect("negotiation timeout must be bounded");
    assert!(result.is_err(), "TCP connection without CONNECT cannot be a successful call");
    terminal.abort();
    let _ = terminal.await;
}

/// `P4a`: ферма энкодеров видна в `/metrics` (`encoder_group_*`, `viewer_rtp_*`)
/// и в JSON `/api/v3/mcu/farm`, а после ухода зрителя серии исчезают.
#[tokio::test]
async fn test_farm_metrics_and_api_v3_snapshot_reflect_encoder_group_lifecycle() {
    use std::sync::atomic::Ordering;

    use vks_media_mcu::{EncoderSpec, FarmCodec, McuEngine};

    // Тест требует реальный FFmpeg (лимит 1 группа: `acquire` поднимает child).
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }

    let engine = Arc::new(McuEngine::new());
    let room = vks_core::RoomId::new();
    let pid = vks_core::ParticipantId::new();
    engine.bridge_participant(room, pid, vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)]);
    let spec = EncoderSpec {
        codec: FarmCodec::H264,
        width: 640,
        height: 360,
        fps: 30,
        bitrate_kbps: 1024,
        maxrate_kbps: 1024,
        bufsize_kbps: 2048,
        profile: "baseline".into(),
        level: "3.1".into(),
        keyint: 30,
        encoder_params: "repeat-headers=1:aud=1:scenecut=0".into(),
    };
    let viewer = engine.acquire_video_viewer(room, &spec).await.expect("farm encoder must start");
    assert_eq!(viewer.rtp_snapshot(), (0, 0, 0), "новый зритель без RTP");
    // Эмулируем per-viewer RTP-обвязчик: 5 пакетов / 500 байт и 2 потерянных кадра.
    viewer.note_rtp(5, 500);
    viewer.note_dropped(2);
    assert_eq!(viewer.rtp_snapshot(), (5, 500, 2));

    // P1-7: `/api/v3/*` служебные endpoint'ы теперь под auth+Moderator —
    // тесту нужен сконфигурированный issuer и записи в Users.
    // VKS_METRICS_TOKEN из окружения CI сделал бы loopback-ветку токен-зависимой:
    // изолируем env на время теста и вернём как было.
    let saved_metrics_token = std::env::var_os("VKS_METRICS_TOKEN");
    std::env::remove_var("VKS_METRICS_TOKEN");
    let mut state = AppState::new();
    let issuer = Arc::new(JwtIssuer::new(JwtConfig {
        secret: "farm_metrics_secret_123".to_string(),
        ttl_secs: 86_400,
        issuer: "vks-integration".to_string(),
    }));
    vks_admin::users::Users::bootstrap_operator(&state, "admin");
    vks_admin::users::Users::create(&state, "viewer", Role::Viewer);
    state.jwt_issuer = Some(issuer.clone());
    let admin_token = issuer.issue("admin", None).expect("issue admin token");
    let viewer_token = issuer.issue("viewer", None).expect("issue viewer token");
    let state = Arc::new(state);
    let frontend = Frontend::load(&frontend_fixture_dir());
    let app = vks_server::build_app_with_services(
        state,
        &frontend,
        HandlerConfig::default_test(),
        Arc::new(vks_media_sfu::SfuEngine::new()),
        engine.clone(),
        None,
        Arc::new(vks_server::GatewayManager::new().with_mcu(engine.clone())),
        Arc::new(vks_server::RecorderManager::new()),
        Arc::new(vks_server::ClusterManager::new("vks-node-1")),
    );

    // P1-7: служебные endpoint'ы loopback-only — тесту нужен ConnectInfo
    // (в бою его кладёт `into_make_service_with_connect_info`).
    let metrics = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot /metrics");
    assert_eq!(metrics.status(), StatusCode::OK);
    let body =
        String::from_utf8(axum::body::to_bytes(metrics.into_body(), 1 << 20).await.expect("metrics body").to_vec())
            .expect("utf8 metrics");
    // Text-формат Prometheus сортирует метки по имени.
    let labels = format!("codec=\"h264\",height=\"360\",room=\"{room}\",width=\"640\"");
    assert!(body.contains("encoder_group_active"), "нет encoder_group_active:\n{body}");
    assert!(body.contains(&labels), "нет метки группы {labels}:\n{body}");
    assert!(body.contains(&format!("viewer_rtp_packets{{{labels}}} 5")), "viewer_rtp_packets должен быть 5:\n{body}");
    assert!(
        body.contains(&format!("viewer_rtp_dropped_frames{{{labels}}} 2")),
        "viewer_rtp_dropped_frames должен быть 2"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/mcu/farm")
                .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot /api/v3/mcu/farm");
    assert_eq!(response.status(), StatusCode::OK, "снимок фермы должен отвечать 200");
    let json_bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("json body");
    let json: serde_json::Value = serde_json::from_slice(&json_bytes).expect("json");
    let groups = json["groups"].as_array().expect("groups array");
    let group = groups
        .iter()
        .find(|group| group["room"] == serde_json::json!(room.to_string()))
        .expect("группа комнаты должна быть в снимке");
    assert_eq!(group["viewers"], serde_json::json!(1));
    assert_eq!(group["viewer_rtp"]["packets"], serde_json::json!(5));
    assert!(json["totals"]["groups"].as_u64().unwrap_or(0) >= 1, "в totals должен быть хотя бы один группа");

    // P1-7: `/metrics` с LAN без токена — 403; три `/api/v3/*` — 401 анонимно,
    // 403 для роли ниже Moderator и 200 для admin.
    let forbidden = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .extension(ConnectInfo(SocketAddr::from(([192, 168, 1, 50], 40000))))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot /metrics");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN, "/metrics must be loopback-only without token");
    for uri in ["/api/v3/mcu/farm", "/api/v3/gateways/status", "/api/v3/cluster/status"] {
        let unauthorized = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(axum::body::Body::empty()).expect("request"))
            .await
            .expect("oneshot internal endpoint");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED, "{uri}: анонимный запрос обязан быть 401");
        let low_role = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {viewer_token}"))
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot internal endpoint");
        assert_eq!(low_role.status(), StatusCode::FORBIDDEN, "{uri}: Viewer не должен видеть операционные данные");
        let allowed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot internal endpoint");
        assert_eq!(allowed.status(), StatusCode::OK, "{uri}: Operator обязан получать данные");
    }

    // Зритель ушёл → группа ретируется, серии из /metrics исчезают.
    assert_eq!(viewer.refcount(), 2, "зритель + энкодер до release");
    drop(viewer);
    let metrics = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot /metrics");
    let body =
        String::from_utf8(axum::body::to_bytes(metrics.into_body(), 1 << 20).await.expect("metrics body").to_vec())
            .expect("utf8 metrics");
    assert!(!body.contains(&room.to_string()), "серии ретированной группы не должны оставаться в /metrics");
    assert_eq!(engine.video_farm_stats().len(), 0, "группа должна быть удалена из фермы");
    // Против «оптимизации» с неиспользуемым импортом.
    let _ = Ordering::Relaxed;
    match saved_metrics_token {
        Some(token) => std::env::set_var("VKS_METRICS_TOKEN", token),
        None => std::env::remove_var("VKS_METRICS_TOKEN"),
    }
}

/// T09a: the recording control port persists synchronously, finalizes a row
/// left unfinalized by a restart, and never accumulates duplicate ACTIVE rows.
#[tokio::test]
async fn recording_control_persists_immediately_and_reuses_the_room_slot_after_restart() {
    use vks_admin::state::RecordingControl as _;
    use vks_core::ports::Storage as _;
    use vks_server::RecorderControl;
    use vks_server::RecorderManager;

    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let room_id = vks_core::RoomId::new();

    let state_a = Arc::new(AppState::with_storage(storage.clone()));
    let control_a = RecorderControl::new(
        RecorderManager::new(),
        Arc::downgrade(&state_a),
        Arc::new(vks_media_mcu::McuEngine::new()),
    );
    let record_id = control_a.start(room_id).await.expect("start");
    assert!(!record_id.is_empty());
    // Persistence is awaited: the row is visible without polling.
    let row = storage.get_record(&record_id).await.expect("get").expect("persisted immediately");
    assert_eq!(row.room_id, room_id);
    assert_eq!(row.duration_sec, 0);
    assert!(row.bitrate_kbps > 0 && row.width > 0 && row.height > 0, "profile is stored: {row:?}");

    // Path A: "restart" then start again without stopping. The unfinalized row
    // from the dead process must be replaced, not duplicated.
    drop(state_a);
    let state_b = Arc::new(AppState::with_storage(storage.clone()));
    let control_b = RecorderControl::new(
        RecorderManager::new(),
        Arc::downgrade(&state_b),
        Arc::new(vks_media_mcu::McuEngine::new()),
    );
    let second_id = control_b.start(room_id).await.expect("start after restart");
    assert_ne!(second_id, record_id, "a new recording gets a new record id");
    let rows = storage.list_records(Some(room_id)).await.expect("list");
    assert_eq!(rows.len(), 1, "restart must not leave a stale unfinalized duplicate: {rows:?}");
    assert_eq!(rows[0].id, second_id);

    // Path B: restart again, then DELETE. The unfinalized row is finalized by
    // the stop path (the in-memory session no longer exists after restart).
    drop(state_b);
    let state_c = Arc::new(AppState::with_storage(storage.clone()));
    let control_c = RecorderControl::new(
        RecorderManager::new(),
        Arc::downgrade(&state_c),
        Arc::new(vks_media_mcu::McuEngine::new()),
    );
    control_c.stop(room_id).await.expect("stop after restart finalizes the row");
    let finalized = storage.get_record(&second_id).await.expect("get").expect("row");
    assert!(finalized.duration_sec >= 1, "restarted stop must finalize duration: {finalized:?}");
    let active =
        storage.list_records(Some(room_id)).await.expect("list").into_iter().filter(|e| e.duration_sec == 0).count();
    assert_eq!(active, 0, "no row may stay active after DELETE");
}
