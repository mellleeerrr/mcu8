//! Inbound H.323 UAS process boundary.
//!
//! A single long-lived Python/Pycrate listener owns Q.931/H.225/H.245
//! signalling. Rust chooses the room and RTP ports and never shares media
//! samples with Python. The listener forks one child per call, so one parent
//! process serves concurrent calls; the server respawns it if it exits.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::uac::{default_python, failure, CallResult};
use crate::H323VideoProfile;

const MAX_EVENT_BYTES: usize = 64 * 1024;

/// Buffered event stream of the listener process. It is separated from the
/// command handle so a dedicated task can read lines without ever being
/// cancelled in the middle of `read_until` by a `select!` timer.
pub type H323EventStream = BufReader<ChildStdout>;

/// Reads exactly one JSON event. Callers must own the stream across this await
/// (never place it directly inside `select!`).
pub async fn read_event<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value, String> {
    let mut line = Vec::new();
    let length = reader
        .take((MAX_EVENT_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|error| format!("H.323 UAS event read failed: {error}"))?;
    if length == 0 {
        return Err("H.323 UAS listener closed stdout".into());
    }
    if length > MAX_EVENT_BYTES || !line.ends_with(b"\n") {
        return Err("H.323 UAS oversized/truncated event".into());
    }
    serde_json::from_slice(&line).map_err(|error| format!("H.323 UAS invalid JSON event: {error}"))
}

/// Cloneable command channel to the listener process.
#[derive(Clone)]
pub struct H323UasHandle {
    stdin: Arc<Mutex<ChildStdin>>,
    closed: Arc<AtomicBool>,
}

impl H323UasHandle {
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub async fn send(&self, value: Value) -> Result<(), String> {
        if self.is_closed() {
            return Err("H.323 UAS listener is closed".into());
        }
        let mut line = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await.map_err(|error| format!("H.323 UAS command write failed: {error}"))?;
        stdin.flush().await.map_err(|error| format!("H.323 UAS command flush failed: {error}"))
    }

    pub async fn accept(
        &self,
        call_id: &str,
        local_ip: &str,
        audio_port: u16,
        video_port: Option<u16>,
    ) -> Result<(), String> {
        self.send(json!({
            "command": "accept", "call_id": call_id, "local_ip": local_ip,
            "audio_port": audio_port, "video_port": video_port,
        }))
        .await
    }

    pub async fn reject(&self, call_id: &str) -> Result<(), String> {
        self.send(json!({"command": "reject", "call_id": call_id})).await
    }

    pub async fn hangup(&self, call_id: &str) {
        let _ = self.send(json!({"command": "hangup", "call_id": call_id})).await;
    }

    pub async fn shutdown(&self) {
        let _ = self.send(json!({"command": "shutdown"})).await;
    }
}

/// Owned listener process and event stream.
pub struct H323Uas {
    child: Child,
    pid: Option<u32>,
    stdout: Option<H323EventStream>,
    closed: Arc<AtomicBool>,
    handle: H323UasHandle,
}

impl H323Uas {
    /// Spawns the listener and waits for its `listening` readiness event.
    pub async fn spawn(bind: &str, local_ip: &str, video_profile: H323VideoProfile) -> CallResult<Self> {
        let python = std::env::var_os("VKS_H323_PYTHON").map(PathBuf::from).unwrap_or_else(default_python);
        let helper = resolve_listener_helper()?;
        let mut command = Command::new(python);
        command.arg("-u").arg(helper);
        Self::spawn_command(bind, local_ip, video_profile, command).await
    }

    async fn spawn_command(
        bind: &str,
        local_ip: &str,
        video_profile: H323VideoProfile,
        mut command: Command,
    ) -> CallResult<Self> {
        let local_ip =
            local_ip.parse::<std::net::Ipv4Addr>().map_err(|_| failure("Invalid H.323 UAS local IPv4 address"))?;
        if local_ip.is_unspecified() {
            return Err(failure("H.323 UAS local IPv4 address must be concrete"));
        }
        let allow_cidrs: Vec<String> = std::env::var("VKS_H323_UAS_ALLOW")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        if allow_cidrs.is_empty() {
            return Err(failure("VKS_H323_UAS_ALLOW is required when inbound H.323 UAS is enabled"));
        }
        let max_children =
            std::env::var("VKS_H323_UAS_MAX_CHILDREN").ok().and_then(|value| value.parse::<u32>().ok()).unwrap_or(64);
        let max_children_per_ip =
            std::env::var("VKS_H323_UAS_MAX_PER_IP").ok().and_then(|value| value.parse::<u32>().ok()).unwrap_or(8);
        let mut config = json!({
            "bind": bind, "local_ip": local_ip.to_string(),
            "trace": std::env::var("VKS_H323_TRACE").is_ok_and(|value| value == "1" || value == "true"),
            "allow_cidrs": allow_cidrs,
            "max_children": max_children,
            "max_children_per_ip": max_children_per_ip,
        });
        if let (Some(object), Some(video)) =
            (config.as_object_mut(), video_profile.sidecar_video_fields().as_object().cloned())
        {
            object.extend(video);
        }
        #[cfg(unix)]
        {
            // Whole-tree ownership: on timeout Rust kills the process group,
            // not only the Python parent, so forked call children cannot leak.
            command.process_group(0);
        }
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| failure(format!("cannot start H.323 UAS sidecar: {error}")))?;
        let mut stdin = child.stdin.take().ok_or_else(|| failure("H.323 UAS stdin unavailable"))?;
        let stdout = child.stdout.take().ok_or_else(|| failure("H.323 UAS stdout unavailable"))?;
        let stderr = child.stderr.take().ok_or_else(|| failure("H.323 UAS stderr unavailable"))?;
        let mut initial = serde_json::to_vec(&config).map_err(failure)?;
        initial.push(b'\n');
        stdin
            .write_all(&initial)
            .await
            .map_err(|error| failure(format!("H.323 UAS configuration write failed: {error}")))?;

        tokio::spawn(async move {
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
                debug!(diagnostic = %String::from_utf8_lossy(&retained), "H.323 UAS sidecar stderr");
            }
        });

        let stdin = Arc::new(Mutex::new(stdin));
        let closed = Arc::new(AtomicBool::new(false));
        let pid = child.id();
        let mut uas = Self {
            child,
            pid,
            stdout: Some(BufReader::new(stdout)),
            closed: closed.clone(),
            handle: H323UasHandle { stdin, closed },
        };
        let ready = tokio::time::timeout(Duration::from_secs(10), uas.next_event())
            .await
            .map_err(|_| failure("H.323 UAS listener did not become ready"))?
            .map_err(failure)?;
        if ready.get("event").and_then(Value::as_str) != Some("listening") {
            return Err(failure(format!("H.323 UAS listener did not report readiness: {ready}")));
        }
        Ok(uas)
    }

    pub fn handle(&self) -> H323UasHandle {
        self.handle.clone()
    }

    /// Detaches the event stream for a dedicated reader task.
    pub fn take_event_stream(&mut self) -> Option<H323EventStream> {
        self.stdout.take()
    }

    /// Next JSON event from the listener. EOF and malformed events are errors.
    pub async fn next_event(&mut self) -> Result<Value, String> {
        let Some(reader) = self.stdout.as_mut() else {
            self.closed.store(true, Ordering::Release);
            return Err("H.323 UAS event stream already detached".into());
        };
        let event = read_event(reader).await;
        if event.is_err() {
            self.closed.store(true, Ordering::Release);
        }
        event
    }

    /// Graceful shutdown: ask the listener to close, then enforce a deadline.
    pub async fn shutdown(&mut self) {
        self.handle.shutdown().await;
        if tokio::time::timeout(Duration::from_secs(6), self.child.wait()).await.is_err() {
            warn!("H.323 UAS listener did not stop in time; killing process group");
            #[cfg(unix)]
            if let Some(pid) = self.pid {
                // Negative pid addresses the process group created above.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = self.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        }
        self.closed.store(true, Ordering::Release);
    }
}

impl Drop for H323Uas {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        // `kill_on_drop` kills the parent; the process group also covers
        // forked call children if the supervisor aborts unexpectedly.
        #[cfg(unix)]
        if self.child.try_wait().ok().flatten().is_none() {
            if let Some(pid) = self.pid {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }
            }
        }
    }
}

fn resolve_listener_helper() -> CallResult<PathBuf> {
    if let Some(raw) = std::env::var_os("VKS_H323_UAS_HELPER") {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Ok(path);
        }
        return Err(failure(format!("VKS_H323_UAS_HELPER does not exist: {}", path.display())));
    }
    let executable = std::env::current_exe().ok();
    let current_dir = std::env::current_dir().ok();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut candidates = Vec::new();
    if let Some(dir) = executable.as_deref().and_then(std::path::Path::parent) {
        candidates.push(dir.join("h323_listener.py"));
        candidates.push(dir.join("python/h323_listener.py"));
        candidates.push(dir.join("../share/vks-mcu/h323_listener.py"));
    }
    if cfg!(debug_assertions) {
        if let Some(dir) = current_dir.as_deref() {
            candidates.push(dir.join("crates/vks-gateway-h323/python/h323_listener.py"));
        }
    }
    candidates.push(manifest_dir.join("python/h323_listener.py"));
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| failure("H.323 UAS listener helper not found; set VKS_H323_UAS_HELPER"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_helper_resolves_in_source_tree() {
        let helper = resolve_listener_helper().expect("listener helper");
        assert!(helper.ends_with("h323_listener.py"));
    }

    #[tokio::test]
    async fn read_event_handles_a_line_split_across_chunks() {
        let (mut writer, reader) = tokio::io::duplex(256);
        tokio::spawn(async move {
            writer.write_all(b"{\"event\":\"in").await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
            writer.write_all(b"coming\",\"call_id\":\"c1\"}\n").await.unwrap();
        });
        let mut reader = BufReader::new(reader);
        let event = read_event(&mut reader).await.expect("chunked event");
        assert_eq!(event["event"], "incoming");
    }

    #[tokio::test]
    async fn spawn_rejects_enabled_without_allowlist() {
        std::env::remove_var("VKS_H323_UAS_ALLOW");
        let error =
            H323Uas::spawn_command("127.0.0.1:0", "127.0.0.1", H323VideoProfile::default(), Command::new("true"))
                .await
                .err();
        assert!(error.is_some());
    }

    #[tokio::test]
    async fn spawn_rejects_unspecified_local_ip() {
        let error = H323Uas::spawn("127.0.0.1:0", "0.0.0.0", H323VideoProfile::default()).await.err();
        assert!(error.is_some());
    }
}
