//! Native WebTransport/QUIC signaling endpoint (T13).
//!
//! This is the **native-client** WebTransport transport: a TLS 1.3 QUIC
//! listener that accepts a WebTransport session on `/wt`, reads one
//! length-prefixed protobuf `Envelope` per bidirectional-stream frame and
//! drives the same [`SignalingHandler`] as the WebSocket transport. The first
//! frame is a `ClientHello`; the handler answers with `ServerHello` (including
//! `room_mode`), never with a fabricated SDP answer.
//!
//! Deliberately out of scope (documented, not claimed):
//! * browsers need `serverCertificateHashes` + a trusted/≤14-day ECDSA
//!   certificate; this release targets native clients only (ADR-0001/T13);
//! * datagrams are not used for signaling: an SDP offer/answer (1–3 KiB) does
//!   not fit a 1200-byte datagram, so frames travel on a reliable bi-stream.
//!
//! Feature-gated by `webtransport`; without the feature this module does not
//! exist and `vks-server` serves the WS-only application.
//!
//! Wire framing (inside the WebTransport bi-stream): `u32` big-endian length +
//! protobuf bytes, max [`DEFAULT_MAX_FRAME_BYTES`] (same 64 KiB ceiling as WS).
//!
//! Environment (all read only when `VKS_WEBTRANSPORT` is enabled):
//! * `VKS_WT_BIND` — UDP bind address, default `0.0.0.0:8443`;
//! * `VKS_WT_PORT` — overrides the port part of `VKS_WT_BIND`;
//! * `VKS_WT_CERT` / `VKS_WT_KEY` — PEM certificate/private key (required);
//! * `VKS_WT_MAX_CONNECTIONS`, `VKS_WT_MAX_FRAME_BYTES`,
//!   `VKS_WT_HANDSHAKE_TIMEOUT_SECS`, `VKS_WT_IDLE_TIMEOUT_SECS`,
//!   `VKS_WT_KEEPALIVE_SECS` — limits.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use wtransport::endpoint::endpoint_side::Server;
use wtransport::endpoint::{IncomingSession, SessionRequest};
use wtransport::tls::{Sha256Digest, Sha256DigestFmt};
use wtransport::{Endpoint, Identity, RecvStream, SendStream, ServerConfig};

use crate::envelope::Envelope;
use crate::handler::{HandlerConfig, SignalingHandler};

/// Default UDP port for the native WebTransport listener.
pub const DEFAULT_WT_BIND: &str = "0.0.0.0:8443";
/// Session path. Browsers/native clients connect to `https://host:port/wt`.
pub const WT_PATH: &str = "/wt";
/// Per-frame ceiling; matches the WS signaling ceiling (`MAX_WS_MESSAGE_BYTES`).
pub const DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024;
/// Bounded number of concurrently open QUIC connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;
/// Time allowed for the QUIC/TLS handshake and session request.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// QUIC idle timeout; keeps dead clients from pinning a connection forever.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Keep-alive period so an idle-but-healthy signaling session survives NAT.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(15);

/// Pre-accept token check, supplied by the composition root. Returning `false`
/// rejects the WebTransport session with HTTP/3 403 **before** `accept()`.
pub type WtTokenVerifier = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// WebTransport configuration/limits. Constructed from the environment, or
/// directly by tests/deploy tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WtConfig {
    /// UDP address the QUIC endpoint binds (port `0` is allowed for tests).
    pub bind: SocketAddr,
    /// PEM certificate chain path (required).
    pub cert_path: PathBuf,
    /// PEM private key path (required).
    pub key_path: PathBuf,
    /// Session path; always [`WT_PATH`] for this release.
    pub path: String,
    /// Maximum concurrently open QUIC connections.
    pub max_connections: usize,
    /// Maximum length-prefixed frame size.
    pub max_frame_bytes: usize,
    /// Handshake/session-request timeout.
    pub handshake_timeout: Duration,
    /// QUIC idle timeout.
    pub idle_timeout: Duration,
    /// QUIC keep-alive interval.
    pub keep_alive_interval: Duration,
}

/// Configuration/endpoint errors. All are fatal at startup (exit 78 in the
/// binary), never silently downgraded to WS-only.
#[derive(Debug, thiserror::Error)]
pub enum WtError {
    /// An environment/config value is invalid.
    #[error("invalid WebTransport {field}: {reason}")]
    InvalidConfig {
        /// Environment variable or field name.
        field: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// Certificate/key loading or validation failed.
    #[error("WebTransport identity failed: {0}")]
    Identity(String),
    /// UDP socket/endpoint bind failed.
    #[error("WebTransport endpoint bind failed: {0}")]
    Bind(String),
    /// The certificate chain is empty (pinning would be meaningless).
    #[error("WebTransport certificate chain is empty")]
    EmptyCertificateChain,
    /// A frame did not fit the configured limit.
    #[error("WebTransport frame of {len} bytes exceeds the {max}-byte limit")]
    FrameTooLarge {
        /// Actual frame length.
        len: usize,
        /// Configured ceiling.
        max: usize,
    },
    /// A QUIC stream failed while reading/writing a frame.
    #[error("WebTransport stream error: {0}")]
    Stream(String),
}

fn enabled_value(value: Option<String>) -> Result<bool, WtError> {
    let Some(raw) = value else { return Ok(false) };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Ok(true),
        "" | "0" | "false" | "off" | "no" => Ok(false),
        other => Err(WtError::InvalidConfig {
            field: "VKS_WEBTRANSPORT",
            reason: format!("expected 1/true/on/yes or 0/false/off/no, got {other:?}"),
        }),
    }
}

fn required_env<F>(lookup: &F, field: &'static str) -> Result<String, WtError>
where
    F: Fn(&str) -> Option<String>,
{
    lookup(field).map(|value| value.trim().to_string()).filter(|value| !value.is_empty()).ok_or_else(|| {
        WtError::InvalidConfig {
            field,
            reason: "required when VKS_WEBTRANSPORT is enabled (no self-signed fallback in production)".into(),
        }
    })
}

fn parse_env<T, F>(lookup: &F, field: &'static str, default: T) -> Result<T, WtError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
    F: Fn(&str) -> Option<String>,
{
    match lookup(field).map(|value| value.trim().to_string()).filter(|value| !value.is_empty()) {
        Some(raw) => raw
            .parse()
            .map_err(|error| WtError::InvalidConfig { field, reason: format!("cannot parse {raw:?}: {error}") }),
        None => Ok(default),
    }
}

impl WtConfig {
    /// Reads `VKS_WT_*` from the process environment.
    ///
    /// `Ok(None)` means WebTransport is disabled (safe default). A malformed
    /// value is an error: a typo must not silently disable a transport the
    /// operator asked for.
    pub fn from_env() -> Result<Option<Self>, WtError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Testable variant of [`Self::from_env`] with an injectable lookup.
    pub fn from_lookup<F>(lookup: F) -> Result<Option<Self>, WtError>
    where
        F: Fn(&str) -> Option<String>,
    {
        if !enabled_value(lookup("VKS_WEBTRANSPORT"))? {
            return Ok(None);
        }
        let cert = required_env(&lookup, "VKS_WT_CERT")?;
        let key = required_env(&lookup, "VKS_WT_KEY")?;
        let bind_raw = lookup("VKS_WT_BIND").map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        let mut bind: SocketAddr = match bind_raw {
            Some(raw) => raw.parse().map_err(|error| WtError::InvalidConfig {
                field: "VKS_WT_BIND",
                reason: format!("cannot parse {raw:?} as an IP:port: {error}"),
            })?,
            None => DEFAULT_WT_BIND.parse().expect("DEFAULT_WT_BIND is a valid SocketAddr"),
        };
        if let Some(port) =
            lookup("VKS_WT_PORT").map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
        {
            let port: u16 = port.parse().map_err(|error| WtError::InvalidConfig {
                field: "VKS_WT_PORT",
                reason: format!("cannot parse {port:?} as u16: {error}"),
            })?;
            bind.set_port(port);
        }

        let config = Self {
            bind,
            cert_path: PathBuf::from(cert),
            key_path: PathBuf::from(key),
            path: WT_PATH.to_string(),
            max_connections: parse_env(&lookup, "VKS_WT_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?,
            max_frame_bytes: parse_env(&lookup, "VKS_WT_MAX_FRAME_BYTES", DEFAULT_MAX_FRAME_BYTES)?,
            handshake_timeout: Duration::from_secs(parse_env(
                &lookup,
                "VKS_WT_HANDSHAKE_TIMEOUT_SECS",
                DEFAULT_HANDSHAKE_TIMEOUT.as_secs(),
            )?),
            idle_timeout: Duration::from_secs(parse_env(&lookup, "VKS_WT_IDLE_TIMEOUT_SECS", {
                DEFAULT_IDLE_TIMEOUT.as_secs()
            })?),
            keep_alive_interval: Duration::from_secs(parse_env(
                &lookup,
                "VKS_WT_KEEPALIVE_SECS",
                DEFAULT_KEEPALIVE.as_secs(),
            )?),
        };
        config.validate()?;
        Ok(Some(config))
    }

    /// Rejects limits that would make the listener a resource hazard.
    pub fn validate(&self) -> Result<(), WtError> {
        if self.cert_path.as_os_str().is_empty() || self.key_path.as_os_str().is_empty() {
            return Err(WtError::InvalidConfig {
                field: "VKS_WT_CERT/VKS_WT_KEY",
                reason: "both certificate and key paths are required".into(),
            });
        }
        if self.path != WT_PATH {
            return Err(WtError::InvalidConfig { field: "path", reason: format!("only {WT_PATH:?} is supported") });
        }
        if self.max_connections == 0 {
            return Err(WtError::InvalidConfig {
                field: "VKS_WT_MAX_CONNECTIONS",
                reason: "must be at least 1".into(),
            });
        }
        if !(1024..=16 * 1024 * 1024).contains(&self.max_frame_bytes) {
            return Err(WtError::InvalidConfig {
                field: "VKS_WT_MAX_FRAME_BYTES",
                reason: "must be in [1024, 16777216]".into(),
            });
        }
        if self.handshake_timeout.is_zero() || self.idle_timeout.is_zero() {
            return Err(WtError::InvalidConfig {
                field: "timeouts",
                reason: "handshake and idle timeouts must be non-zero".into(),
            });
        }
        if self.keep_alive_interval >= self.idle_timeout {
            return Err(WtError::InvalidConfig {
                field: "VKS_WT_KEEPALIVE_SECS",
                reason: "keep-alive must be shorter than the idle timeout".into(),
            });
        }
        Ok(())
    }
}

/// TLS identity for the QUIC endpoint: PEM loading plus the SHA-256 of the DER
/// certificate (the pinning value served by `/wt/cert-hash`).
pub struct WtIdentity {
    inner: Identity,
}

impl WtIdentity {
    /// Loads a certificate chain and private key from PEM files.
    pub async fn load_pemfiles(cert: impl AsRef<Path>, key: impl AsRef<Path>) -> Result<Self, WtError> {
        let inner = Identity::load_pemfiles(cert, key).await.map_err(|error| WtError::Identity(error.to_string()))?;
        let identity = Self { inner };
        // Fail at startup, not on the first pinning request, when the PEM has
        // no certificate (an empty chain would hash to a meaningless value).
        let _ = identity.certificate_der()?;
        Ok(identity)
    }

    /// Generates a WebTransport-compatible self-signed identity (ECDSA P-256,
    /// ≤14 days). Intended for dev/tests and for provisioning a native-client
    /// scratch endpoint; production supplies operator certificates instead.
    pub fn self_signed<I, S>(subject_alt_names: I) -> Result<Self, WtError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let inner = Identity::self_signed(subject_alt_names).map_err(|error| WtError::Identity(error.to_string()))?;
        Ok(Self { inner })
    }

    /// Writes the chain and key as PEM files (owner policy is the caller's).
    pub async fn store_pemfiles(&self, cert: impl AsRef<Path>, key: impl AsRef<Path>) -> std::io::Result<()> {
        self.inner.certificate_chain().store_pemfile(cert).await?;
        self.inner.private_key().store_secret_pemfile(key).await
    }

    /// DER bytes of the leaf certificate.
    pub fn certificate_der(&self) -> Result<&[u8], WtError> {
        self.inner
            .certificate_chain()
            .as_slice()
            .first()
            .map(|certificate| certificate.der())
            .ok_or(WtError::EmptyCertificateChain)
    }

    /// SHA-256 of the DER leaf certificate as a [`Sha256Digest`].
    pub fn cert_sha256_digest(&self) -> Result<Sha256Digest, WtError> {
        self.inner
            .certificate_chain()
            .as_slice()
            .first()
            .map(|certificate| certificate.hash())
            .ok_or(WtError::EmptyCertificateChain)
    }

    /// Lowercase hex SHA-256 of the DER leaf certificate (no separators).
    pub fn cert_sha256_hex(&self) -> Result<String, WtError> {
        Ok(self.cert_sha256_digest()?.fmt(Sha256DigestFmt::DottedHex).replace(':', ""))
    }

    fn into_inner(self) -> Identity {
        self.inner
    }
}

/// A bound WebTransport endpoint. [`Self::run`] accepts sessions forever; the
/// caller owns the task and can drop it with the process.
pub struct WtServer {
    endpoint: Endpoint<Server>,
    handler_config: HandlerConfig,
    config: WtConfig,
    cert_sha256_hex: String,
    local_addr: SocketAddr,
    verifier: Option<WtTokenVerifier>,
}

impl WtServer {
    /// Loads the identity, builds the QUIC/TLS endpoint and binds the UDP
    /// socket. Any failure is fatal for the caller (no WS-only downgrade).
    pub async fn bind(
        config: WtConfig,
        handler_config: HandlerConfig,
        verifier: Option<WtTokenVerifier>,
    ) -> Result<Self, WtError> {
        config.validate()?;
        let identity = WtIdentity::load_pemfiles(&config.cert_path, &config.key_path).await?;
        let cert_sha256_hex = identity.cert_sha256_hex()?;

        let builder = ServerConfig::builder()
            .with_bind_address(config.bind)
            .with_identity(identity.into_inner())
            .keep_alive_interval(Some(config.keep_alive_interval));
        let builder = builder.max_idle_timeout(Some(config.idle_timeout)).map_err(|_| WtError::InvalidConfig {
            field: "VKS_WT_IDLE_TIMEOUT_SECS",
            reason: "QUIC idle timeout is out of range".into(),
        })?;
        let endpoint = Endpoint::server(builder.build()).map_err(|error| WtError::Bind(error.to_string()))?;
        let local_addr = endpoint.local_addr().map_err(|error| WtError::Bind(error.to_string()))?;

        Ok(Self { endpoint, handler_config, config, cert_sha256_hex, local_addr, verifier })
    }

    /// Actual bound UDP address (useful when configured with port `0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Actual bound UDP port (Alt-Svc/cert-hash advertise this, not `:0`).
    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Lowercase hex SHA-256 of the served leaf certificate.
    pub fn cert_sha256_hex(&self) -> &str {
        &self.cert_sha256_hex
    }

    /// Accepts connections until the endpoint is closed. Each connection gets
    /// its own [`SignalingHandler`]; streams on one connection share it.
    pub async fn run(self) {
        let Self { endpoint, handler_config, config, cert_sha256_hex: _, local_addr: _, verifier } = self;
        loop {
            let incoming = endpoint.accept().await;
            if endpoint.open_connections() > config.max_connections {
                tracing::warn!(
                    open = endpoint.open_connections(),
                    max = config.max_connections,
                    remote = %incoming.remote_address(),
                    "WebTransport connection limit reached; refusing incoming session"
                );
                incoming.refuse();
                continue;
            }
            let handler_config = handler_config.clone();
            let config = config.clone();
            let verifier = verifier.clone();
            tokio::spawn(async move {
                handle_incoming(incoming, handler_config, config, verifier).await;
            });
        }
    }
}

async fn handle_incoming(
    incoming: IncomingSession,
    handler_config: HandlerConfig,
    config: WtConfig,
    verifier: Option<WtTokenVerifier>,
) {
    let remote = incoming.remote_address();
    let request = match tokio::time::timeout(config.handshake_timeout, incoming).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            tracing::debug!(%remote, %error, "WebTransport session request failed");
            return;
        }
        Err(_) => {
            tracing::warn!(%remote, timeout = ?config.handshake_timeout, "WebTransport handshake timed out");
            return;
        }
    };

    // Pre-accept guards: only `/wt`, and only an authenticated token when the
    // composition root supplied a verifier. Rejections happen before
    // `accept()`, so an unauthorized peer never gets a WebTransport session.
    if request.path().split('?').next().unwrap_or("") != config.path {
        tracing::debug!(%remote, path = request.path(), "WebTransport request for unknown path");
        request.not_found().await;
        return;
    }
    if let Some(verifier) = verifier.as_ref() {
        let token = session_token(&request);
        if !token.is_some_and(|token| verifier(token)) {
            tracing::debug!(%remote, "WebTransport request rejected before accept: invalid token");
            request.forbidden().await;
            return;
        }
    }

    let connection = match tokio::time::timeout(config.handshake_timeout, request.accept()).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            tracing::debug!(%remote, %error, "WebTransport session accept failed");
            return;
        }
        Err(_) => {
            tracing::warn!(%remote, "WebTransport accept timed out");
            return;
        }
    };
    tracing::info!(%remote, "WebTransport session established on /wt");

    let handler = Arc::new(Mutex::new(SignalingHandler::new(handler_config)));
    loop {
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                let handler = Arc::clone(&handler);
                let max_frame_bytes = config.max_frame_bytes;
                tokio::spawn(async move {
                    serve_stream(send, recv, handler, max_frame_bytes).await;
                });
            }
            Err(error) => {
                tracing::debug!(%remote, %error, "WebTransport connection closed");
                break;
            }
        }
    }
}

fn session_token(request: &SessionRequest) -> Option<&str> {
    let header = request
        .headers()
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")));
    header.or_else(|| {
        request
            .path()
            .split_once('?')
            .and_then(|(_, query)| query.split('&').find_map(|pair| pair.strip_prefix("token=")))
            .filter(|token| !token.is_empty())
    })
}

async fn serve_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    handler: Arc<Mutex<SignalingHandler>>,
    max_frame_bytes: usize,
) {
    loop {
        let frame = match read_frame(&mut recv, max_frame_bytes).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(WtError::FrameTooLarge { len, max }) => {
                tracing::warn!(len, max, "WebTransport frame exceeds limit; closing stream");
                break;
            }
            Err(error) => {
                tracing::debug!(%error, "WebTransport stream read ended");
                break;
            }
        };
        let envelope = match Envelope::decode(&frame) {
            Ok(envelope) => envelope,
            Err(error) => {
                tracing::warn!(%error, "WebTransport frame is not a protobuf Envelope; closing stream");
                break;
            }
        };
        let replies = handler.lock().await.handle(envelope);
        for reply in replies {
            if let Err(error) = write_frame(&mut send, &reply.encode()).await {
                tracing::debug!(%error, "WebTransport reply write failed");
                return;
            }
        }
    }
    let _ = send.finish().await;
}

async fn read_frame(recv: &mut RecvStream, max_frame_bytes: usize) -> Result<Option<Vec<u8>>, WtError> {
    let mut length = [0u8; 4];
    match recv.read_exact(&mut length).await {
        Ok(()) => {}
        // A stream that ends before a full length prefix is a normal close.
        Err(wtransport::error::StreamReadExactError::FinishedEarly(_)) => return Ok(None),
        Err(error) => return Err(WtError::Stream(format!("stream read error: {error}"))),
    }
    let len = u32::from_be_bytes(length) as usize;
    if len > max_frame_bytes {
        return Err(WtError::FrameTooLarge { len, max: max_frame_bytes });
    }
    let mut frame = vec![0u8; len];
    recv.read_exact(&mut frame).await.map_err(|error| WtError::Stream(format!("stream frame truncated: {error}")))?;
    Ok(Some(frame))
}

async fn write_frame(send: &mut SendStream, payload: &[u8]) -> Result<(), WtError> {
    let len = u32::try_from(payload.len())
        .map_err(|_| WtError::FrameTooLarge { len: payload.len(), max: u32::MAX as usize })?;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|error| WtError::Stream(format!("stream write error: {error}")))?;
    send.write_all(payload).await.map_err(|error| WtError::Stream(format!("stream write error: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn disabled_when_env_absent_or_false() {
        assert!(WtConfig::from_lookup(lookup(&[])).expect("disabled").is_none());
        for value in ["0", "false", "off", "no", ""] {
            assert!(WtConfig::from_lookup(lookup(&[("VKS_WEBTRANSPORT", value)])).expect("disabled").is_none());
        }
    }

    #[test]
    fn garbage_enable_value_is_an_error_not_a_silent_disable() {
        let error = WtConfig::from_lookup(lookup(&[("VKS_WEBTRANSPORT", "maybe")])).expect_err("must fail");
        assert!(matches!(error, WtError::InvalidConfig { field: "VKS_WEBTRANSPORT", .. }));
    }

    #[test]
    fn missing_certificate_is_fail_closed() {
        let error = WtConfig::from_lookup(lookup(&[("VKS_WEBTRANSPORT", "1"), ("VKS_WT_KEY", "/tmp/key.pem")]))
            .expect_err("must fail");
        assert!(matches!(error, WtError::InvalidConfig { field: "VKS_WT_CERT", .. }));
    }

    #[test]
    fn env_parses_limits_and_port_override() {
        let config = WtConfig::from_lookup(lookup(&[
            ("VKS_WEBTRANSPORT", "1"),
            ("VKS_WT_BIND", "127.0.0.1:8443"),
            ("VKS_WT_PORT", "9443"),
            ("VKS_WT_CERT", "/etc/vks/wt-cert.pem"),
            ("VKS_WT_KEY", "/etc/vks/wt-key.pem"),
            ("VKS_WT_MAX_CONNECTIONS", "8"),
            ("VKS_WT_MAX_FRAME_BYTES", "4096"),
        ]))
        .expect("valid")
        .expect("enabled");
        assert_eq!(config.bind, "127.0.0.1:9443".parse().unwrap());
        assert_eq!(config.max_connections, 8);
        assert_eq!(config.max_frame_bytes, 4096);
        assert_eq!(config.path, WT_PATH);
    }

    #[test]
    fn keepalive_longer_than_idle_is_rejected() {
        let error = WtConfig::from_lookup(lookup(&[
            ("VKS_WEBTRANSPORT", "1"),
            ("VKS_WT_CERT", "/tmp/cert.pem"),
            ("VKS_WT_KEY", "/tmp/key.pem"),
            ("VKS_WT_IDLE_TIMEOUT_SECS", "5"),
            ("VKS_WT_KEEPALIVE_SECS", "15"),
        ]))
        .expect_err("must fail");
        assert!(matches!(error, WtError::InvalidConfig { field: "VKS_WT_KEEPALIVE_SECS", .. }));
    }

    #[tokio::test]
    async fn self_signed_certificate_hash_is_sha256_of_der() {
        let identity = WtIdentity::self_signed(["127.0.0.1"]).expect("identity");
        let digest = identity.cert_sha256_digest().expect("digest");
        let hex = identity.cert_sha256_hex().expect("hex");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(digest.fmt(Sha256DigestFmt::DottedHex).replace(':', ""), hex);
        assert!(!identity.certificate_der().expect("der").is_empty());
    }

    #[tokio::test]
    async fn missing_pem_files_fail_closed() {
        let error =
            WtIdentity::load_pemfiles("/nonexistent/cert.pem", "/nonexistent/key.pem").await.err().expect("must fail");
        assert!(matches!(error, WtError::Identity(_)));
    }
}
