//! T13 acceptance for the composition root: `/wt` 404 + no Alt-Svc when the
//! transport is off, and a real QUIC endpoint + cert-hash + Alt-Svc when it is
//! on. The WS signaling path is exercised in both modes (regression guard).
//!
//! These tests spawn the real `vks-server` binary with a scratch SQLite DB and
//! loopback ports; no live service/DB/port from the host is touched.
#![cfg(feature = "webtransport")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use wtransport::endpoint::ConnectOptions;
use wtransport::tls::Sha256DigestFmt;
use wtransport::{ClientConfig, Endpoint, RecvStream};

use vks_proto::media;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ClientHello, SignalingMessage};
use vks_signaling::envelope::Envelope;
use vks_signaling::webtransport::{WtIdentity, WT_PATH};

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

struct ServerProc {
    child: Child,
    dir: PathBuf,
    port: u16,
    log: PathBuf,
}

impl ServerProc {
    fn spawn(extra_env: &[(&str, String)]) -> Self {
        let dir = std::env::temp_dir().join(format!("vks-server-wt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let log = dir.join("server.log");
        let log_file = std::fs::File::create(&log).expect("log file");
        let db = dir.join("vks.db");
        let port = pick_port();
        let db_url = format!("sqlite://{}?mode=rwc", db.display());
        let jwt_secret = "integration-wt-secret-0123456789abcdef";
        let config = format!(
            "server_name = \"wt-test\"\n\
             listen_addr = \"127.0.0.1:{port}\"\n\
             candidate_addr = \"127.0.0.1\"\n\
             allow_insecure_defaults = true\n\
             auth_provider = \"local\"\n\
             jwt_secret = \"{jwt_secret}\"\n\
             jwt_ttl_secs = 3600\n\
             admin_login = \"admin\"\n\
             admin_password = \"admin_password\"\n\
             frontend_dir = \"{}\"\n\
             database_url = \"{db_url}\"\n\
             storage_allow_fallback = false\n\
             default_room_mode = \"sfu\"\n",
            dir.join("missing-frontend").display(),
        );
        let config_path = dir.join("server.toml");
        std::fs::write(&config_path, config).expect("config");

        let mut command = Command::new(env!("CARGO_BIN_EXE_vks-server"));
        command
            .arg("--config")
            .arg(&config_path)
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::null())
            .env("VKS_WEBRTC_TRANSPORT", "1")
            .env("VKS_LISTEN_ADDR", format!("127.0.0.1:{port}"))
            .env("VKS_DATABASE_URL", &db_url)
            .env("VKS_JWT_SECRET", jwt_secret)
            .env("VKS_ADMIN_PASSWORD", "admin_password")
            .env("VKS_ALLOW_INSECURE_DEFAULTS", "true")
            .env("VKS_CANDIDATE_ADDR", "127.0.0.1")
            .env_remove("VKS_WEBTRANSPORT")
            .env_remove("VKS_WT_BIND")
            .env_remove("VKS_WT_PORT")
            .env_remove("VKS_WT_CERT")
            .env_remove("VKS_WT_KEY");
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let child = command.spawn().expect("spawn vks-server");
        Self { child, dir, port, log }
    }

    async fn wait_ready(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                panic!("server exited before /healthz ({status:?}); log:\n{}", self.log_text());
            }
            if let Ok(response) = http_request(self.port, "GET", "/healthz", &[], None).await {
                if response.status == 200 {
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("server did not become healthy; log:\n{}", self.log_text());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn pick_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port").local_addr().expect("addr").port()
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vks-wt-cert-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("cert dir");
    dir
}

async fn http_request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> std::io::Result<HttpResponse> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let body = body.unwrap_or("");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let mut content_length = None;
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        if header_end.is_none() {
            if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                let head = String::from_utf8_lossy(&raw[..position]).to_string();
                content_length = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
                });
            }
        }
        if let (Some(end), Some(length)) = (header_end, content_length) {
            if raw.len() >= end + length {
                break;
            }
        }
    }
    let header_end = header_end.ok_or_else(|| std::io::Error::other("HTTP headers not received"))?;
    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other(format!("bad status line: {head:?}")))?;
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    let body = String::from_utf8_lossy(&raw[header_end..]).to_string();
    Ok(HttpResponse { status, headers, body })
}

async fn login_token(port: u16) -> String {
    let response = http_request(
        port,
        "POST",
        "/api/v3/login",
        &[("content-type", "application/json")],
        Some(r#"{"login":"admin","password":"admin_password"}"#),
    )
    .await
    .expect("login request");
    assert_eq!(response.status, 200, "login failed: {}", response.body);
    let value: Value = serde_json::from_str(&response.body).expect("login json");
    value["token"].as_str().expect("token").to_string()
}

fn hello_envelope() -> Envelope {
    Envelope::new(SignalingMessage {
        msg_id: 77,
        reply_to: 0,
        payload: Some(Payload::ClientHello(ClientHello {
            proto_version: 1,
            client_id: "native-wt-e2e".into(),
            sdk_version: "0.1.0".into(),
            user_agent: "wtransport-e2e".into(),
            auth_token: "jwt".into(),
            room_id: "room-wt-e2e".into(),
            capabilities: Vec::new(),
            resume_nonce: None,
        })),
    })
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

async fn read_frame(recv: &mut RecvStream) -> Vec<u8> {
    let mut length = [0u8; 4];
    recv.read_exact(&mut length).await.expect("frame length");
    let mut payload = vec![0u8; u32::from_be_bytes(length) as usize];
    recv.read_exact(&mut payload).await.expect("frame payload");
    payload
}

async fn ws_server_hello(port: u16, token: &str) -> Envelope {
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?token={token}")).await.expect("WS connect");
    ws.send(Message::Binary(hello_envelope().encode().into())).await.expect("send ClientHello");
    let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("WS reply timeout")
        .expect("WS closed")
        .expect("WS frame");
    let Message::Binary(bytes) = message else { panic!("expected binary WS frame, got {message:?}") };
    Envelope::decode(&bytes).expect("ServerHello envelope")
}

fn alt_svc_port(response: &HttpResponse) -> Option<u16> {
    let value = response.header("alt-svc")?;
    let rest = value.strip_prefix("h3=\":")?;
    let (port, _) = rest.split_once('"')?;
    port.parse().ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wt_off_returns_404_no_alt_svc_and_ws_still_works() {
    let mut server = ServerProc::spawn(&[]);
    server.wait_ready().await;

    let health = http_request(server.port, "GET", "/healthz", &[], None).await.expect("healthz");
    assert_eq!(health.status, 200);
    assert!(health.header("alt-svc").is_none(), "Alt-Svc must not be advertised when WT is off");

    for path in ["/wt", "/wt/cert-hash"] {
        let response = http_request(server.port, "GET", path, &[], None).await.expect("GET");
        assert_eq!(response.status, 404, "{path} must be 404 when WT is off; body={}", response.body);
        assert!(response.header("alt-svc").is_none(), "404 must not advertise Alt-Svc");
    }

    let token = login_token(server.port).await;
    let hello = ws_server_hello(server.port, &token).await;
    assert_eq!(hello.reply_to(), 77);
    match hello.payload() {
        Some(Payload::ServerHello(hello)) => assert_eq!(hello.server_id, "srv-0001"),
        other => panic!("WS must still answer ServerHello, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wt_on_serves_cert_hash_alt_svc_and_real_quic_handshake() {
    let cert_dir = scratch_dir();
    let cert = cert_dir.join("cert.pem");
    let key = cert_dir.join("key.pem");
    let identity = WtIdentity::self_signed(["127.0.0.1"]).expect("identity");
    identity.store_pemfiles(&cert, &key).await.expect("store cert");
    let expected_hash = identity.cert_sha256_hex().expect("hash");
    let digest = identity.cert_sha256_digest().expect("digest");

    let mut server = ServerProc::spawn(&[
        ("VKS_WEBTRANSPORT", "1".to_string()),
        ("VKS_WT_BIND", "127.0.0.1:0".to_string()),
        ("VKS_WT_CERT", cert.display().to_string()),
        ("VKS_WT_KEY", key.display().to_string()),
    ]);
    server.wait_ready().await;

    let health = http_request(server.port, "GET", "/healthz", &[], None).await.expect("healthz");
    assert_eq!(health.status, 200);
    let wt_port = alt_svc_port(&health).expect("Alt-Svc h3 port on /healthz");
    assert_ne!(wt_port, 0);

    let info = http_request(server.port, "GET", "/wt", &[], None).await.expect("GET /wt");
    assert_eq!(info.status, 200, "body={}", info.body);
    let info_json: Value = serde_json::from_str(&info.body).expect("wt info json");
    assert_eq!(info_json["path"].as_str(), Some(WT_PATH));
    assert_eq!(info_json["port"].as_u64(), Some(u64::from(wt_port)));

    let hash_response = http_request(server.port, "GET", "/wt/cert-hash", &[], None).await.expect("cert-hash");
    assert_eq!(hash_response.status, 200);
    let hash_json: Value = serde_json::from_str(&hash_response.body).expect("cert-hash json");
    assert_eq!(hash_json["algorithm"].as_str(), Some("sha-256"));
    assert_eq!(hash_json["sha256"].as_str(), Some(expected_hash.as_str()), "cert-hash must be SHA-256(DER leaf)");
    assert_eq!(hash_json["port"].as_u64(), Some(u64::from(wt_port)));
    assert_eq!(
        digest.fmt(Sha256DigestFmt::DottedHex).replace(':', ""),
        expected_hash,
        "client pinning digest must equal the served hash"
    );

    let token = login_token(server.port).await;
    let client = ClientConfig::builder().with_bind_default().with_server_certificate_hashes([digest]).build();
    let endpoint = Endpoint::client(client).expect("client endpoint");
    let connection = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(
            ConnectOptions::builder(format!("https://127.0.0.1:{wt_port}{WT_PATH}"))
                .add_header("authorization", format!("Bearer {token}"))
                .build(),
        ),
    )
    .await
    .expect("WT connect timeout")
    .expect("real QUIC/WT handshake with pinned cert");

    let (mut send, mut recv) = connection.open_bi().await.expect("open bi").await.expect("bi ready");
    send.write_all(&frame(&hello_envelope().encode())).await.expect("write ClientHello");
    let reply = read_frame(&mut recv).await;
    let hello = Envelope::decode(&reply).expect("ServerHello");
    assert_eq!(hello.reply_to(), 77);
    match hello.payload() {
        Some(Payload::ServerHello(hello)) => {
            assert_eq!(hello.server_id, "srv-0001");
            assert_eq!(hello.room_mode, media::RoomMode::Sfu as i32);
            assert!(hello.caps.is_some());
        }
        other => panic!("expected ServerHello, got {other:?}"),
    }

    // WS must keep working while WT is on (both transports share the FSM).
    let ws_hello = ws_server_hello(server.port, &token).await;
    assert!(matches!(ws_hello.payload(), Some(Payload::ServerHello(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wt_enabled_with_bad_cert_path_exits_78() {
    let mut server = ServerProc::spawn(&[
        ("VKS_WEBTRANSPORT", "1".to_string()),
        ("VKS_WT_BIND", "127.0.0.1:0".to_string()),
        ("VKS_WT_CERT", "/nonexistent/wt-cert.pem".to_string()),
        ("VKS_WT_KEY", "/nonexistent/wt-key.pem".to_string()),
    ]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = server.child.try_wait().expect("try_wait") {
            assert_eq!(status.code(), Some(78), "bad WT cert must exit EX_CONFIG=78; log:\n{}", server.log_text());
            assert!(
                server.log_text().contains("WebTransport"),
                "error must name WebTransport; log:\n{}",
                server.log_text()
            );
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("server did not reject bad WT cert; log:\n{}", server.log_text());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
