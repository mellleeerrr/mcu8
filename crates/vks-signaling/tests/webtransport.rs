//! T13 native WebTransport acceptance: a real wtransport client performs a
//! QUIC/TLS 1.3 handshake against an in-process `WtServer` on `127.0.0.1:0`,
//! pins the SHA-256 certificate hash, sends `ClientHello` on a bi-stream and
//! parses the `ServerHello` answer. No TCP/HTTP mocks are involved.
//!
//! The test is compiled only with the `webtransport` feature (this is the same
//! feature that adds the UDP listener and the `/wt` HTTP contract).
#![cfg(feature = "webtransport")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use wtransport::endpoint::ConnectOptions;
use wtransport::tls::Sha256Digest;
use wtransport::{ClientConfig, Endpoint, RecvStream};

use vks_proto::media;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ClientHello, SignalingMessage};
use vks_signaling::envelope::Envelope;
use vks_signaling::handler::HandlerConfig;
use vks_signaling::webtransport::{
    WtConfig, WtError, WtIdentity, WtServer, DEFAULT_KEEPALIVE, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_FRAME_BYTES,
    WT_PATH,
};

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("vks-wt-signaling-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self { dir }
    }

    fn cert(&self) -> PathBuf {
        self.dir.join("cert.pem")
    }

    fn key(&self) -> PathBuf {
        self.dir.join("key.pem")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn config(cert: PathBuf, key: PathBuf) -> WtConfig {
    WtConfig {
        bind: "127.0.0.1:0".parse().expect("loopback"),
        cert_path: cert,
        key_path: key,
        path: WT_PATH.to_string(),
        max_connections: DEFAULT_MAX_CONNECTIONS,
        max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        handshake_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        keep_alive_interval: DEFAULT_KEEPALIVE,
    }
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

async fn read_frame(recv: &mut RecvStream) -> Vec<u8> {
    let mut length = [0u8; 4];
    recv.read_exact(&mut length).await.expect("frame length");
    let len = u32::from_be_bytes(length) as usize;
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await.expect("frame payload");
    payload
}

fn client_config(digest: Sha256Digest) -> ClientConfig {
    ClientConfig::builder().with_bind_default().with_server_certificate_hashes([digest]).build()
}

fn hello() -> Envelope {
    Envelope::new(SignalingMessage {
        msg_id: 41,
        reply_to: 0,
        payload: Some(Payload::ClientHello(ClientHello {
            proto_version: 1,
            client_id: "native-wt-test".into(),
            sdk_version: "0.1.0".into(),
            user_agent: "wtransport-test-client".into(),
            auth_token: "test-token".into(),
            room_id: "room-wt".into(),
            capabilities: vec!["mcu_renderer".into()],
            resume_nonce: None,
        })),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_hello_to_server_hello_over_real_quic() {
    let scratch = Scratch::new();
    let identity = WtIdentity::self_signed(["127.0.0.1"]).expect("self-signed identity");
    identity.store_pemfiles(scratch.cert(), scratch.key()).await.expect("store pem files");
    let digest = identity.cert_sha256_digest().expect("digest");
    let cert_hash = identity.cert_sha256_hex().expect("hex");

    let cfg = config(scratch.cert(), scratch.key());
    let server = WtServer::bind(cfg, HandlerConfig::default_test().with_room_mode(media::RoomMode::Mcu as i32), None)
        .await
        .expect("bind wt server");
    assert_eq!(server.cert_sha256_hex(), cert_hash, "served cert hash must be the pinning value");
    assert_eq!(server.port(), server.local_addr().port());
    assert_ne!(server.port(), 0, "ephemeral port must be resolved before advertising Alt-Svc");
    let port = server.port();
    let task = tokio::spawn(server.run());

    let endpoint = Endpoint::client(client_config(digest)).expect("client endpoint");
    let connection =
        tokio::time::timeout(Duration::from_secs(10), endpoint.connect(format!("https://127.0.0.1:{port}{WT_PATH}")))
            .await
            .expect("connect timeout")
            .expect("QUIC/WT handshake with pinned certificate hash");

    let (mut send, mut recv) = connection.open_bi().await.expect("open bi stream").await.expect("bi stream ready");
    send.write_all(&frame(&hello().encode())).await.expect("write ClientHello");
    let reply = read_frame(&mut recv).await;
    let envelope = Envelope::decode(&reply).expect("ServerHello envelope");
    assert_eq!(envelope.reply_to(), 41, "ServerHello must answer msg_id 41");
    match envelope.payload() {
        Some(Payload::ServerHello(hello)) => {
            assert_eq!(hello.proto_version, 1);
            assert_eq!(hello.server_id, "srv-0001");
            assert_eq!(hello.room_mode, media::RoomMode::Mcu as i32, "room_mode must be carried");
            assert!(hello.caps.is_some(), "caps must be carried");
            assert!(hello.ice.is_none(), "in-process test config has no ICE servers");
        }
        other => panic!("expected ServerHello, got {other:?}"),
    }

    // Sanity: the wire frame is a ServerHello, never a fabricated SDP answer.
    let text = String::from_utf8_lossy(&reply);
    assert!(!text.contains("v=0"), "WT must not fabricate SDP: {text}");

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_certificate_hash_is_rejected_by_the_client() {
    let scratch = Scratch::new();
    let served = WtIdentity::self_signed(["127.0.0.1"]).expect("served identity");
    served.store_pemfiles(scratch.cert(), scratch.key()).await.expect("store pem files");
    let port = {
        let server = WtServer::bind(config(scratch.cert(), scratch.key()), HandlerConfig::default_test(), None)
            .await
            .expect("bind");
        let port = server.port();
        tokio::spawn(server.run());
        port
    };

    let other = WtIdentity::self_signed(["127.0.0.1"]).expect("other identity");
    let wrong = other.cert_sha256_digest().expect("digest");
    assert_ne!(
        wrong.fmt(wtransport::tls::Sha256DigestFmt::DottedHex),
        served.cert_sha256_digest().expect("digest").fmt(wtransport::tls::Sha256DigestFmt::DottedHex)
    );
    let endpoint = Endpoint::client(client_config(wrong)).expect("client endpoint");
    let result = endpoint.connect(format!("https://127.0.0.1:{port}{WT_PATH}")).await;
    assert!(result.is_err(), "a client pinning a different certificate hash must not establish a session");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_accept_verifier_rejects_before_session_accept() {
    let scratch = Scratch::new();
    let identity = WtIdentity::self_signed(["127.0.0.1"]).expect("identity");
    identity.store_pemfiles(scratch.cert(), scratch.key()).await.expect("store pem files");
    let digest = identity.cert_sha256_digest().expect("digest");
    let server = WtServer::bind(
        config(scratch.cert(), scratch.key()),
        HandlerConfig::default_test(),
        Some(Arc::new(|token: &str| token == "let-me-in")),
    )
    .await
    .expect("bind");
    let port = server.port();
    tokio::spawn(server.run());

    let endpoint = Endpoint::client(client_config(digest)).expect("client");
    let denied = endpoint
        .connect(
            ConnectOptions::builder(format!("https://127.0.0.1:{port}{WT_PATH}"))
                .add_header("authorization", "Bearer wrong")
                .build(),
        )
        .await;
    assert!(denied.is_err(), "verifier must reject the session request before accept");

    let allowed = endpoint
        .connect(
            ConnectOptions::builder(format!("https://127.0.0.1:{port}{WT_PATH}"))
                .add_header("authorization", "Bearer let-me-in")
                .build(),
        )
        .await;
    assert!(allowed.is_ok(), "authorized token must establish the session: {allowed:?}");
}

#[tokio::test]
async fn bad_or_missing_certificate_files_fail_closed() {
    let scratch = Scratch::new();
    let cfg = config(scratch.dir.join("missing-cert.pem"), scratch.dir.join("missing-key.pem"));
    let error = WtServer::bind(cfg, HandlerConfig::default_test(), None).await.err().expect("missing PEM");
    assert!(matches!(error, WtError::Identity(_)), "unexpected error: {error:?}");

    // A certificate path that exists but is not a certificate/key pair.
    std::fs::write(scratch.cert(), b"not a certificate").expect("write junk cert");
    std::fs::write(scratch.key(), b"not a key").expect("write junk key");
    let error = WtServer::bind(config(scratch.cert(), scratch.key()), HandlerConfig::default_test(), None)
        .await
        .err()
        .expect("junk PEM");
    assert!(matches!(error, WtError::Identity(_)), "unexpected error: {error:?}");
}
