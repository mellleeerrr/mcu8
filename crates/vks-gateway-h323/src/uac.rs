//! Outbound H.323 calls backed by an isolated, bounded ASN.1/PER sidecar.
//!
//! `connected` is returned only after H.225 CONNECT and bidirectional H.245
//! audio channel negotiation. ALERTING, TCP EOF and timeouts are never success.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{timeout, Instant};
use tracing::{debug, info};
use vks_common::Error;

const MAX_EVENT_BYTES: usize = 32 * 1024;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

type CallResult<T> = Result<T, Error>;

fn failure(message: impl std::fmt::Display) -> Error {
    Error::Internal(anyhow::anyhow!("H.323: {message}"))
}

struct SessionControl {
    commands: mpsc::Sender<()>,
    closed: Arc<AtomicBool>,
    disconnected: watch::Receiver<Option<String>>,
}

impl Drop for SessionControl {
    fn drop(&mut self) {
        // Last session owner leaves: request protocol shutdown. If the queue
        // already contains a hangup, that request is sufficient. The process
        // owner enforces a two-second grace and then kills/reaps the helper.
        let _ = self.commands.try_send(());
    }
}

/// Negotiated outbound H.323 session. Clones share one process and one call.
#[derive(Clone)]
pub struct ActiveH323Session {
    pub call_id: String,
    pub call_ref: u16,
    pub remote_ip: String,
    pub remote_audio_port: Option<u16>,
    pub remote_audio_payload_type: u8,
    pub local_audio_payload_type: u8,
    pub remote_video_ip: Option<String>,
    pub remote_video_port: Option<u16>,
    pub remote_video_payload_type: Option<u8>,
    pub local_video_payload_type: Option<u8>,
    control: Arc<SessionControl>,
}

impl ActiveH323Session {
    pub fn is_closed(&self) -> bool {
        self.control.closed.load(Ordering::Acquire)
    }

    /// One terminal notification for remote release, process failure or hangup.
    /// Call owners must subscribe so the roster and media bridge are removed
    /// when the peer ends the call, independently of UI polling.
    pub fn subscribe_disconnect(&self) -> watch::Receiver<Option<String>> {
        self.control.disconnected.clone()
    }

    pub async fn hangup(&self) -> CallResult<()> {
        if self.is_closed() {
            return Ok(());
        }
        let mut disconnected = self.subscribe_disconnect();
        let _ = self.control.commands.try_send(());
        timeout(SHUTDOWN_GRACE + Duration::from_secs(2), async {
            loop {
                if disconnected.borrow().is_some() || self.is_closed() {
                    return Ok(());
                }
                disconnected.changed().await.map_err(|_| failure("session supervisor disappeared"))?;
            }
        })
        .await
        .map_err(|_| failure("sidecar shutdown deadline exceeded"))?
    }
}

#[derive(Deserialize)]
struct ConnectedMedia {
    call_id: String,
    call_ref: u16,
    remote_ip: String,
    remote_audio_port: u16,
    remote_audio_payload_type: u8,
    local_audio_payload_type: u8,
    remote_video_ip: Option<String>,
    remote_video_port: Option<u16>,
    remote_video_payload_type: Option<u8>,
    local_video_payload_type: Option<u8>,
}

impl ConnectedMedia {
    fn validate(&self, expected_id: &str, expected_ref: u16, target: SocketAddrV4, video: bool) -> Result<(), String> {
        if self.call_id != expected_id || self.call_ref != expected_ref {
            return Err("sidecar returned a foreign call identity".into());
        }
        if self.remote_ip.parse::<Ipv4Addr>().ok().as_ref() != Some(target.ip()) || self.remote_audio_port == 0 {
            return Err("sidecar returned an invalid audio endpoint".into());
        }
        if self.remote_audio_payload_type != 0 || self.local_audio_payload_type != 0 {
            return Err("sidecar negotiated an unsupported audio codec".into());
        }
        let has_video = self.remote_video_port.is_some();
        if has_video != self.remote_video_ip.is_some()
            || has_video != self.remote_video_payload_type.is_some()
            || has_video != self.local_video_payload_type.is_some()
            || (has_video && !video)
        {
            return Err("sidecar returned incomplete/unrequested video negotiation".into());
        }
        if has_video
            && (self.remote_video_port == Some(0)
                || self.remote_video_ip.as_ref().and_then(|ip| ip.parse::<Ipv4Addr>().ok()).as_ref()
                    != Some(target.ip())
                || !self.remote_video_payload_type.is_some_and(|pt| (96..=127).contains(&pt))
                || !self.local_video_payload_type.is_some_and(|pt| (96..=127).contains(&pt)))
        {
            return Err("sidecar returned an invalid video endpoint/payload type".into());
        }
        Ok(())
    }
}

/// H.241 video capability, derived from the live gateway VideoProfile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct H323VideoProfile {
    /// H.241 `maxBitRate` source, kbit/s; wire value is `* 10` (100 bit/s).
    pub max_bitrate_kbps: u32,
    /// H.264 level_idc (31 = 3.1, 40 = 4.0); generic parameter 42 is +40.
    pub level_idc: u8,
}

impl Default for H323VideoProfile {
    fn default() -> Self {
        Self { max_bitrate_kbps: 512, level_idc: 31 }
    }
}

impl H323VideoProfile {
    fn h241_level(self) -> u32 {
        u32::from(self.level_idc.clamp(30, 51)) + 40
    }

    fn max_bitrate_100bps(self) -> u32 {
        self.max_bitrate_kbps.saturating_mul(10).clamp(1, 192_400)
    }
}

pub struct H323UacClient {
    pub local_ip: String,
}

impl H323UacClient {
    pub fn new(local_ip: &str) -> Self {
        Self { local_ip: local_ip.to_string() }
    }

    pub async fn dial(
        &self,
        target_uri: &str,
        local_rtp_port: u16,
        dial_timeout: Duration,
    ) -> CallResult<ActiveH323Session> {
        self.dial_with_media(target_uri, local_rtp_port, None, dial_timeout).await
    }

    pub async fn dial_with_media(
        &self,
        target_uri: &str,
        local_audio_port: u16,
        local_video_port: Option<u16>,
        dial_timeout: Duration,
    ) -> CallResult<ActiveH323Session> {
        self.dial_with_media_profile(
            target_uri,
            local_audio_port,
            local_video_port,
            dial_timeout,
            H323VideoProfile::default(),
        )
        .await
    }

    pub async fn dial_with_media_profile(
        &self,
        target_uri: &str,
        local_audio_port: u16,
        local_video_port: Option<u16>,
        dial_timeout: Duration,
        video_profile: H323VideoProfile,
    ) -> CallResult<ActiveH323Session> {
        let python = std::env::var_os("VKS_H323_PYTHON").map(PathBuf::from).unwrap_or_else(default_python);
        let helper = resolve_helper()?;
        info!(helper = %helper.display(), python = %python.display(), "H.323 sidecar resolved");
        let mut command = Command::new(python);
        command.arg("-u").arg(helper);
        self.dial_command(target_uri, local_audio_port, local_video_port, dial_timeout, video_profile, command).await
    }

    async fn dial_command(
        &self,
        target_uri: &str,
        audio_port: u16,
        video_port: Option<u16>,
        dial_timeout: Duration,
        video_profile: H323VideoProfile,
        mut command: Command,
    ) -> CallResult<ActiveH323Session> {
        let target = parse_target(target_uri)?;
        let local_ip = self
            .local_ip
            .parse::<Ipv4Addr>()
            .map_err(|_| Error::InvalidInput("Invalid H.323 local IPv4 candidate".into()))?;
        if local_ip.is_unspecified() || audio_port == 0 || audio_port == u16::MAX || dial_timeout.is_zero() {
            return Err(Error::InvalidInput("Invalid H.323 local media parameters".into()));
        }
        if let Some(port) = video_port {
            if port == 0 || port == u16::MAX || port.abs_diff(audio_port) < 2 {
                return Err(Error::InvalidInput("Overlapping/invalid H.323 RTP port pairs".into()));
            }
        }
        let guid = uuid::Uuid::new_v4();
        let call_id = guid.to_string();
        let call_ref = ((guid.as_u128() & 0x7fff) as u16).max(1);
        let config = json!({
            "target_ip": target.ip().to_string(), "target_port": target.port(),
            "local_ip": local_ip.to_string(), "audio_port": audio_port,
            "video_port": video_port, "timeout_seconds": dial_timeout.as_secs_f64().min(180.0),
            "call_id": call_id, "call_ref": call_ref,
            "video_max_bitrate_100bps": video_profile.max_bitrate_100bps(),
            "video_h264_level": video_profile.h241_level(),
        });
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = command.spawn().map_err(|err| failure(format!("cannot start ASN.1 sidecar: {err}")))?;
        let mut stdin = child.stdin.take().ok_or_else(|| failure("sidecar stdin unavailable"))?;
        let stdout = child.stdout.take().ok_or_else(|| failure("sidecar stdout unavailable"))?;
        let stderr = child.stderr.take().ok_or_else(|| failure("sidecar stderr unavailable"))?;
        let mut initial = serde_json::to_vec(&config).map_err(failure)?;
        initial.push(b'\n');
        timeout(Duration::from_secs(2), stdin.write_all(&initial))
            .await
            .map_err(|_| failure("sidecar configuration write timed out"))?
            .map_err(failure)?;

        // Drain stderr even after the retained diagnostic bound, so a helper
        // cannot deadlock on a full pipe. It never contains login credentials.
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut retained = Vec::new();
            let mut buffer = [0u8; 1024];
            while let Ok(count) = stderr.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                let keep = count.min(8192usize.saturating_sub(retained.len()));
                retained.extend_from_slice(&buffer[..keep]);
            }
            if !retained.is_empty() {
                debug!(diagnostic = %String::from_utf8_lossy(&retained), "H.323 sidecar stderr");
            }
        });
        let (commands, receive_commands) = mpsc::channel(1);
        let (disconnect_tx, disconnect_rx) = watch::channel(None);
        let closed = Arc::new(AtomicBool::new(false));
        let control = Arc::new(SessionControl { commands, closed: closed.clone(), disconnected: disconnect_rx });
        let (result_tx, result_rx) = oneshot::channel();
        let supervisor = Supervisor {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            commands: receive_commands,
            disconnect: disconnect_tx,
            closed,
            result: Some(result_tx),
            expected_id: call_id.clone(),
            expected_ref: call_ref,
            target,
            video_requested: video_port.is_some(),
            setup_deadline: Instant::now() + dial_timeout + Duration::from_secs(2),
        };
        tokio::spawn(async move {
            supervisor.run().await;
            stderr_task.abort();
        });
        let negotiated = timeout(dial_timeout + Duration::from_secs(5), result_rx)
            .await
            .map_err(|_| failure("sidecar negotiation deadline exceeded"))?
            .map_err(|_| failure("sidecar supervisor ended before negotiation"))?
            .map_err(failure)?;
        // `control` is dropped on every error/cancellation path above, which
        // requests shutdown even if the caller abandoned its dial future.
        info!(call_id = %call_id, audio_port = negotiated.remote_audio_port, video_port = ?negotiated.remote_video_port, "H.323 CONNECT and bidirectional media channels negotiated");
        Ok(ActiveH323Session {
            call_id: format!("h323-call-{call_id}"),
            call_ref,
            remote_ip: negotiated.remote_ip,
            remote_audio_port: Some(negotiated.remote_audio_port),
            remote_audio_payload_type: negotiated.remote_audio_payload_type,
            local_audio_payload_type: negotiated.local_audio_payload_type,
            remote_video_ip: negotiated.remote_video_ip,
            remote_video_port: negotiated.remote_video_port,
            remote_video_payload_type: negotiated.remote_video_payload_type,
            local_video_payload_type: negotiated.local_video_payload_type,
            control,
        })
    }
}

fn default_python() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/share/vks-mcu/h323-venv/bin/python3");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
}

/// Locate the sidecar without baking a build-machine path into the binary.
///
/// `CARGO_MANIFEST_DIR` points at the checkout used to compile the binary. That
/// is a build cache path on CI and may not exist at runtime. Prefer an explicit
/// override, then the installed layout next to the executable, then the
/// source-tree layouts used by `cargo run`/`cargo test`, and keep the
/// compile-time path as a last resort for unusual build setups.
fn resolve_helper() -> CallResult<PathBuf> {
    let executable = std::env::current_exe().ok();
    let current_dir = std::env::current_dir().ok();
    let override_path = std::env::var_os("VKS_H323_HELPER");
    resolve_helper_from(
        executable.as_deref(),
        current_dir.as_deref(),
        override_path.as_deref(),
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    )
}

fn resolve_helper_from(
    executable: Option<&std::path::Path>,
    current_dir: Option<&std::path::Path>,
    override_path: Option<&std::ffi::OsStr>,
    manifest_dir: &std::path::Path,
) -> CallResult<PathBuf> {
    if let Some(raw) = override_path {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Ok(path);
        }
        return Err(failure(format!("VKS_H323_HELPER does not point to an existing file: {}", path.display())));
    }

    let mut searched = Vec::new();
    let mut candidates = Vec::new();
    if let Some(dir) = executable.and_then(std::path::Path::parent) {
        candidates.push(dir.join("h323_sidecar.py"));
        candidates.push(dir.join("python/h323_sidecar.py"));
        candidates.push(dir.join("../share/vks-mcu/h323_sidecar.py"));
    }
    // A daemon started from a user-writable cwd must not execute a planted
    // helper. cwd fallbacks are a developer convenience only.
    if cfg!(debug_assertions) {
        if let Some(dir) = current_dir {
            candidates.push(dir.join("crates/vks-gateway-h323/python/h323_sidecar.py"));
            candidates.push(dir.join("python/h323_sidecar.py"));
        }
    }
    candidates.push(manifest_dir.join("python/h323_sidecar.py"));

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        searched.push(candidate.display().to_string());
    }
    Err(failure(format!("H.323 sidecar not found; set VKS_H323_HELPER. Searched: {}", searched.join(", "))))
}

fn parse_target(uri: &str) -> CallResult<SocketAddrV4> {
    let address = uri.strip_prefix("h323://").or_else(|| uri.strip_prefix("h323:")).unwrap_or(uri);
    let address = address.rsplit('@').next().unwrap_or(address);
    let target = if address.contains(':') { address.to_string() } else { format!("{address}:1720") };
    let target = target
        .parse::<SocketAddrV4>()
        .map_err(|_| Error::InvalidInput("H.323 target must be an IPv4 address with an optional TCP port".into()))?;
    if target.ip().is_unspecified() || target.port() == 0 {
        return Err(Error::InvalidInput("Invalid H.323 target".into()));
    }
    Ok(target)
}

async fn read_event(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Value, String> {
    let mut line = Vec::new();
    let length = reader
        .take((MAX_EVENT_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|err| format!("sidecar read failed: {err}"))?;
    if length == 0 {
        return Err("sidecar stdout closed".into());
    }
    if length > MAX_EVENT_BYTES || !line.ends_with(b"\n") {
        return Err("oversized/truncated sidecar event".into());
    }
    serde_json::from_slice(&line).map_err(|err| format!("invalid sidecar JSON: {err}"))
}

struct Supervisor {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    commands: mpsc::Receiver<()>,
    disconnect: watch::Sender<Option<String>>,
    closed: Arc<AtomicBool>,
    result: Option<oneshot::Sender<Result<ConnectedMedia, String>>>,
    expected_id: String,
    expected_ref: u16,
    target: SocketAddrV4,
    video_requested: bool,
    setup_deadline: Instant,
}

impl Supervisor {
    async fn run(mut self) {
        let mut failure_reason = None;
        let mut closing_deadline = None;
        let mut established = false;
        let reason = loop {
            let deadline = closing_deadline.unwrap_or_else(|| {
                if established {
                    Instant::now() + Duration::from_secs(86400)
                } else {
                    self.setup_deadline
                }
            });
            tokio::select! {
                event = read_event(&mut self.stdout) => {
                    let event = match event {
                        Ok(value) => value,
                        Err(error) => { break failure_reason.take().unwrap_or(error); }
                    };
                    match event.get("event").and_then(Value::as_str) {
                        Some("connected") if !established && closing_deadline.is_none() => {
                            let negotiated = serde_json::from_value::<ConnectedMedia>(event).map_err(|err| err.to_string()).and_then(|media| {
                                media.validate(&self.expected_id, self.expected_ref, self.target, self.video_requested)?;
                                Ok(media)
                            });
                            match negotiated {
                                Ok(media) => {
                                    established = true;
                                    if self.result.take().is_none_or(|result| result.send(Ok(media)).is_err()) {
                                        break "dial caller cancelled".into();
                                    }
                                }
                                Err(error) => { break error; }
                            }
                        }
                        Some("connected") => { break "duplicate/late connected event".into(); }
                        Some("failed") => {
                            let error = event.get("reason").and_then(Value::as_str).unwrap_or("sidecar negotiation failed").to_string();
                            if let Some(result) = self.result.take() { let _ = result.send(Err(error.clone())); }
                            failure_reason = Some(error);
                            closing_deadline = Some(Instant::now() + SHUTDOWN_GRACE);
                        }
                        Some("disconnected") => {
                            break event.get("reason").and_then(Value::as_str).unwrap_or("remote disconnected").to_string();
                        }
                        Some("ringing" | "negotiating" | "channel" | "capabilities" | "video_unavailable" | "wire") => {}
                        _ => { break "unknown sidecar event".into(); }
                    }
                }
                _ = self.commands.recv(), if closing_deadline.is_none() => {
                    let _ = timeout(Duration::from_millis(500), self.stdin.write_all(b"{\"command\":\"hangup\"}\n")).await;
                    closing_deadline = Some(Instant::now() + SHUTDOWN_GRACE);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    break if established { "sidecar shutdown timed out" } else { "H.323 media negotiation timed out" }.into();
                }
            }
        };
        if let Some(result) = self.result.take() {
            let _ = result.send(Err(reason.clone()));
        }
        // EOF is a second shutdown signal if the explicit command was lost.
        let _ = self.stdin.shutdown().await;
        let remaining_grace =
            closing_deadline.map_or(SHUTDOWN_GRACE, |deadline| deadline.saturating_duration_since(Instant::now()));
        if !matches!(timeout(remaining_grace, self.child.wait()).await, Ok(Ok(_))) {
            let _ = self.child.start_kill();
            let _ = timeout(Duration::from_secs(1), self.child.wait()).await;
        }
        self.closed.store(true, Ordering::Release);
        let _ = self.disconnect.send(Some(reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h323_video_profile_maps_to_h241_limits() {
        let profile = H323VideoProfile { max_bitrate_kbps: 4096, level_idc: 40 };
        assert_eq!(profile.h241_level(), 80);
        assert_eq!(profile.max_bitrate_100bps(), 40_960);
        let default = H323VideoProfile::default();
        assert_eq!(default.h241_level(), 71);
        assert_eq!(default.max_bitrate_100bps(), 5120);
        assert_eq!(H323VideoProfile { max_bitrate_kbps: 0, level_idc: 1 }.max_bitrate_100bps(), 1);
        assert_eq!(H323VideoProfile { max_bitrate_kbps: 999_999, level_idc: 99 }.h241_level(), 91);
        assert_eq!(H323VideoProfile { max_bitrate_kbps: 2048, level_idc: 30 }.h241_level(), 70);
    }

    fn python(code: &str) -> Command {
        let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
        command.arg("-u").arg("-c").arg(code);
        command
    }

    const CONNECTED: &str = "import json,sys\nc=json.loads(sys.stdin.readline())\ne=dict(event='connected',call_id=c['call_id'],call_ref=c['call_ref'],remote_ip=c['target_ip'],remote_audio_port=3230,remote_audio_payload_type=0,local_audio_payload_type=0,remote_video_ip=None,remote_video_port=None,remote_video_payload_type=None,local_video_payload_type=None)\nprint(json.dumps(e),flush=True)\n";

    #[tokio::test]
    async fn eof_after_alerting_never_becomes_connected() {
        let result = H323UacClient::new("127.0.0.1")
            .dial_command(
                "h323:127.0.0.1",
                41000,
                None,
                Duration::from_secs(1),
                H323VideoProfile::default(),
                python("import sys\nsys.stdin.readline()\nprint('{\"event\":\"ringing\"}',flush=True)\n"),
            )
            .await;
        assert!(result.is_err(), "EOF after ringing must fail the dial");
    }

    #[tokio::test]
    async fn oversized_helper_event_is_rejected() {
        let result = H323UacClient::new("127.0.0.1")
            .dial_command(
                "h323:127.0.0.1",
                41000,
                None,
                Duration::from_secs(1),
                H323VideoProfile::default(),
                python("import sys\nsys.stdin.readline()\nprint('x'*40000,flush=True)\n"),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remote_process_exit_notifies_call_owner() {
        let session = H323UacClient::new("127.0.0.1")
            .dial_command(
                "h323:127.0.0.1",
                41000,
                None,
                Duration::from_secs(2),
                H323VideoProfile::default(),
                python(CONNECTED),
            )
            .await
            .expect("connected event");
        let mut notice = session.subscribe_disconnect();
        timeout(Duration::from_secs(3), async {
            while notice.borrow().is_none() {
                notice.changed().await.expect("supervisor notice");
            }
        })
        .await
        .expect("EOF must propagate");
        assert!(session.is_closed());
        session.hangup().await.expect("repeat hangup is idempotent");
    }

    #[tokio::test]
    async fn hangup_sends_command_and_waits_for_process_shutdown() {
        let code = format!("{CONNECTED}c=json.loads(sys.stdin.readline())\nassert c['command']=='hangup'\nprint(json.dumps(dict(event='disconnected',reason='local_hangup')),flush=True)\n");
        let session = H323UacClient::new("127.0.0.1")
            .dial_command(
                "h323:127.0.0.1",
                41000,
                None,
                Duration::from_secs(2),
                H323VideoProfile::default(),
                python(&code),
            )
            .await
            .expect("connected");
        session.hangup().await.expect("graceful shutdown");
        assert!(session.is_closed());
        assert_eq!(session.subscribe_disconnect().borrow().as_deref(), Some("local_hangup"));
    }

    #[tokio::test]
    async fn parent_cancellation_stops_pending_process() {
        let marker = std::env::temp_dir().join(format!("vks-h323-cancel-{}", uuid::Uuid::new_v4()));
        let code = format!("import sys,json,pathlib\nc=json.loads(sys.stdin.readline())\nprint('{{\"event\":\"ringing\"}}',flush=True)\nx=json.loads(sys.stdin.readline())\nassert x['command']=='hangup'\npathlib.Path({:?}).write_text('stopped')\n", marker.to_string_lossy());
        let task = tokio::spawn(async move {
            H323UacClient::new("127.0.0.1")
                .dial_command(
                    "h323:127.0.0.1",
                    41000,
                    None,
                    Duration::from_secs(10),
                    H323VideoProfile::default(),
                    python(&code),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        task.abort();
        let _ = task.await;
        timeout(Duration::from_secs(3), async {
            while !marker.is_file() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("cancelled dial must send hangup");
        std::fs::remove_file(marker).expect("remove owned test marker");
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("vks-h323-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }
        fn file(&self, name: &str) -> std::path::PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("helper parent");
            }
            std::fs::write(&path, b"# helper").expect("helper file");
            path
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn helper_override_wins_and_must_exist() {
        let dir = TempDir::new("override");
        let helper = dir.file("h323_sidecar.py");
        let missing = dir.0.join("missing.py");
        assert_eq!(resolve_helper_from(None, None, Some(helper.as_os_str()), &dir.0).expect("override"), helper);
        let error = resolve_helper_from(None, None, Some(missing.as_os_str()), &dir.0).unwrap_err().to_string();
        assert!(error.contains("VKS_H323_HELPER"), "unexpected error: {error}");
        assert!(error.contains("missing.py"), "error must name the path: {error}");
    }

    #[test]
    fn exe_adjacent_helper_beats_cwd_and_manifest() {
        let installed = TempDir::new("installed");
        let cwd = TempDir::new("cwd");
        let manifest = TempDir::new("manifest");
        let helper = installed.file("h323_sidecar.py");
        let _ = cwd.file("python/h323_sidecar.py");
        let _ = manifest.file("python/h323_sidecar.py");
        let resolved =
            resolve_helper_from(Some(&installed.0.join("vks-server")), Some(&cwd.0), None, &manifest.0).expect("exe");
        assert_eq!(resolved, helper);
    }

    #[test]
    fn missing_helper_returns_a_diagnostic_instead_of_a_python_eof() {
        let empty = TempDir::new("empty");
        let error = resolve_helper_from(Some(&empty.0.join("vks-server")), Some(&empty.0), None, &empty.0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not found"), "unexpected error: {error}");
        assert!(error.contains("VKS_H323_HELPER"), "error must suggest the override: {error}");
    }
}
