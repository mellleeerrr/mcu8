//! FFmpeg-backed isolated worker.
//!
//! One worker = one child process and one codec direction. The parent writes
//! raw input frames to `stdin` and reads fixed-size output frames from
//! `stdout`; `stderr` is drained on a separate task so a chatty codec can
//! never fill the pipe and deadlock the pipeline. `kill_on_drop(true)` plus a
//! Unix process group guarantee that a cancelled room takes the codec down.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};

use super::{AudioCodec, SlotConfig, VideoCodec};

/// Runtime configuration shared by all workers.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// FFmpeg executable (PATH lookup when just `ffmpeg`).
    pub ffmpeg_path: PathBuf,
    /// Deadline for one frame read/write.
    pub io_timeout: Duration,
    /// Extra frames buffered by the caller before applying backpressure.
    pub max_queue: usize,
    /// Host-wide worker process cap (enforced by [`WorkerPool`]).
    pub max_workers: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self { ffmpeg_path: PathBuf::from("ffmpeg"), io_timeout: Duration::from_secs(5), max_queue: 30, max_workers: 8 }
    }
}

/// One worker pipeline direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    /// Decode raw audio payload bytes (one or more frames concatenated).
    AudioDecode(AudioCodec),
    /// Encode 48 kHz mono `s16le` PCM to the container used on the RTP side.
    AudioEncode(AudioCodec),
    /// Decode raw video bitstream bytes to `yuv420p`.
    VideoDecode(VideoCodec),
    /// Encode `yuv420p` to the raw codec bitstream.
    VideoEncode(VideoCodec),
}

/// Worker failure. All variants fail the affected slot closed; none panic the
/// MCU.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// Codec/FFmpeg is not installed on this host.
    #[error("codec worker unavailable: {0}")]
    Unavailable(String),
    /// Invalid slot configuration.
    #[error("invalid worker config: {0}")]
    InvalidConfig(String),
    /// Process could not be spawned.
    #[error("failed to spawn codec worker: {0}")]
    Spawn(#[source] std::io::Error),
    /// I/O failure talking to the child.
    #[error("codec worker I/O: {0}")]
    Io(#[source] std::io::Error),
    /// Frame read/write exceeded the deadline.
    #[error("codec worker timed out after {0:?}")]
    Timeout(Duration),
    /// Child exited (codec crash or malformed payload).
    #[error("codec worker exited: {0}")]
    Exited(String),
    /// Codec rejected the frame.
    #[error("codec error: {0}")]
    Codec(String),
}

/// Counters exposed to metrics/tests.
#[derive(Debug, Default)]
pub struct WorkerStats {
    /// Frames written to the child.
    pub frames_in: AtomicU64,
    /// Frames read from the child.
    pub frames_out: AtomicU64,
    /// Payload bytes written.
    pub bytes_in: AtomicU64,
    /// Raw bytes read.
    pub bytes_out: AtomicU64,
    /// Child restarts after a crash.
    pub restarts: AtomicU64,
    /// Timed-out operations.
    pub timeouts: AtomicU64,
}

/// Codec support of the host FFmpeg.
#[derive(Debug, Clone)]
pub struct FfmpegCapabilities {
    /// `ffmpeg -version` first line.
    pub version: String,
    decoders: BTreeSet<String>,
    encoders: BTreeSet<String>,
}

impl FfmpegCapabilities {
    /// Probes the configured FFmpeg binary.
    pub async fn probe(ffmpeg_path: &std::path::Path) -> Result<Self, WorkerError> {
        let version_out = tokio::time::timeout(
            Duration::from_secs(2),
            Command::new(ffmpeg_path).arg("-hide_banner").arg("-version").output(),
        )
        .await
        .map_err(|_| WorkerError::Timeout(Duration::from_secs(2)))?
        .map_err(WorkerError::Spawn)?;
        if !version_out.status.success() {
            return Err(WorkerError::Unavailable(format!("{} -version failed", ffmpeg_path.display())));
        }
        let version = String::from_utf8_lossy(&version_out.stdout).lines().next().unwrap_or_default().to_string();

        let encoders = list_codecs(ffmpeg_path, "-encoders").await?;
        let decoders = list_codecs(ffmpeg_path, "-decoders").await?;
        Ok(Self { version, decoders, encoders })
    }

    /// Whether the host can decode this audio codec.
    #[must_use]
    pub fn has_audio_decoder(&self, codec: AudioCodec) -> bool {
        self.decoders.contains(codec.ffmpeg_name())
    }

    /// Whether the host can encode this audio codec.
    #[must_use]
    pub fn has_audio_encoder(&self, codec: AudioCodec) -> bool {
        codec.ffmpeg_encoder().is_some_and(|name| self.encoders.contains(name))
    }

    /// Whether the host can decode this video codec.
    #[must_use]
    pub fn has_video_decoder(&self, codec: VideoCodec) -> bool {
        self.decoders.contains(codec.ffmpeg_name())
    }

    /// Whether the host can encode this video codec.
    #[must_use]
    pub fn has_video_encoder(&self, codec: VideoCodec) -> bool {
        self.encoders.contains(codec.ffmpeg_encoder())
    }

    /// Checks a mode against probed support.
    pub fn supports(&self, mode: WorkerMode) -> bool {
        match mode {
            WorkerMode::AudioDecode(codec) => self.has_audio_decoder(codec),
            WorkerMode::AudioEncode(codec) => self.has_audio_encoder(codec),
            WorkerMode::VideoDecode(codec) => self.has_video_decoder(codec),
            WorkerMode::VideoEncode(codec) => self.has_video_encoder(codec),
        }
    }
}

async fn list_codecs(ffmpeg: &std::path::Path, flag: &str) -> Result<BTreeSet<String>, WorkerError> {
    let output = Command::new(ffmpeg).arg("-hide_banner").arg(flag).output().await.map_err(WorkerError::Spawn)?;
    if !output.status.success() {
        return Err(WorkerError::Unavailable(format!("{} {flag} failed", ffmpeg.display())));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Format:
    //  V..... h261                 H.261
    //  A....D aac                  AAC ...
    // Flags end at the first space; the codec name is the next token.
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some(rest) = line.split_once(char::is_whitespace) else { continue };
        let (flags, rest) = rest;
        if flags.len() < 6 || rest.trim().is_empty() {
            continue;
        }
        if let Some(name) = rest.split_whitespace().next() {
            names.insert(name.to_string());
        }
    }
    Ok(names)
}

/// A live FFmpeg pipeline.
pub struct FfmpegWorker {
    mode: WorkerMode,
    slot: SlotConfig,
    config: WorkerConfig,
    child: Child,
    stdin: Option<ChildStdin>,
    last_stderr: Arc<Mutex<String>>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub stats: Arc<WorkerStats>,
}

impl FfmpegWorker {
    /// Spawns a worker after validating the slot and host support.
    pub async fn spawn(mode: WorkerMode, slot: SlotConfig, config: WorkerConfig) -> Result<Self, WorkerError> {
        if let WorkerMode::VideoEncode(codec) | WorkerMode::VideoDecode(codec) = mode {
            slot.validate_for(codec)?;
        } else {
            slot.validate()?;
        }
        if matches!(mode, WorkerMode::AudioEncode(AudioCodec::G729)) {
            return Err(WorkerError::Unavailable("FFmpeg has no G.729 encoder".into()));
        }
        let last_stderr = Arc::new(Mutex::new(String::new()));
        let (child, stdin) = spawn_child(mode, slot, &config, last_stderr.clone()).await?;
        Ok(Self {
            mode,
            slot,
            config,
            child,
            stdin: Some(stdin),
            last_stderr,
            permit: None,
            stats: Arc::new(WorkerStats::default()),
        })
    }

    /// The mode this worker was started with.
    #[must_use]
    pub fn mode(&self) -> WorkerMode {
        self.mode
    }

    /// The slot settings this worker was started with.
    #[must_use]
    pub fn slot(&self) -> SlotConfig {
        self.slot
    }

    /// Child process id (diagnostics/tests).
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Last line written by FFmpeg to stderr (diagnostics only).
    #[must_use]
    pub fn last_stderr(&self) -> String {
        self.last_stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Whether the child is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Writes one 20 ms mono PCM frame.
    pub async fn write_audio(&mut self, pcm: &[i16]) -> Result<(), WorkerError> {
        if pcm.len() != super::AUDIO_FRAME_SAMPLES {
            return Err(WorkerError::InvalidConfig(format!(
                "audio frame must be {} samples, got {}",
                super::AUDIO_FRAME_SAMPLES,
                pcm.len()
            )));
        }
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for sample in pcm {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.write_all(&bytes).await?;
        self.stats.frames_in.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Reads one 20 ms mono PCM frame.
    pub async fn read_audio(&mut self) -> Result<Vec<i16>, WorkerError> {
        self.read_audio_samples(super::AUDIO_FRAME_SAMPLES).await
    }

    /// Reads exactly `samples` mono `s16le` PCM samples.
    pub async fn read_audio_samples(&mut self, samples: usize) -> Result<Vec<i16>, WorkerError> {
        if samples == 0 || samples > super::WORKER_SAMPLE_RATE as usize {
            return Err(WorkerError::InvalidConfig(format!("audio block size {samples} outside 1..=48000")));
        }
        let bytes = self.read_exact(samples * 2).await?;
        self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
        Ok(bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect())
    }

    /// Reads exactly one encoded audio frame from an encoder worker.
    ///
    /// ADTS and LOAS/LATM are self-delimiting; this keeps RTP packetization
    /// aligned with codec frames instead of guessing byte counts.
    pub async fn read_encoded_audio(&mut self) -> Result<Vec<u8>, WorkerError> {
        let mode = match self.mode {
            WorkerMode::AudioEncode(codec) => codec,
            _ => return Err(WorkerError::InvalidConfig("read_encoded_audio on non-encoder worker".into())),
        };
        match mode {
            AudioCodec::Aac => {
                // ADTS: 7-byte header, 13-bit frame length at bits 30..43.
                let mut header = [0u8; 7];
                self.read_exact_into(&mut header).await?;
                if header[0] != 0xff || (header[1] & 0xf0) != 0xf0 {
                    return Err(WorkerError::Codec(format!("bad ADTS sync {:02x}{:02x}", header[0], header[1])));
                }
                let length = (usize::from(header[3] & 0x03) << 11)
                    | (usize::from(header[4]) << 3)
                    | (usize::from(header[5]) >> 5);
                if !(7..=8192).contains(&length) {
                    return Err(WorkerError::Codec(format!("invalid ADTS length {length}")));
                }
                let mut frame = vec![0u8; length];
                frame[..7].copy_from_slice(&header);
                self.read_exact_into(&mut frame[7..]).await?;
                self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
                Ok(frame)
            }
            AudioCodec::AacLatm => {
                // LOAS: 3-byte header, 13-bit payload length, sync 0x56e.
                let mut header = [0u8; 3];
                self.read_exact_into(&mut header).await?;
                if header[0] != 0x56 || (header[1] & 0xe0) != 0xe0 {
                    return Err(WorkerError::Codec(format!("bad LOAS sync {:02x}{:02x}", header[0], header[1])));
                }
                let length = (usize::from(header[1] & 0x1f) << 8) | usize::from(header[2]);
                if length == 0 || length > 8192 {
                    return Err(WorkerError::Codec(format!("invalid LOAS length {length}")));
                }
                let mut frame = vec![0u8; 3 + length];
                frame[..3].copy_from_slice(&header);
                self.read_exact_into(&mut frame[3..]).await?;
                self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
                Ok(frame)
            }
            AudioCodec::G729 => Err(WorkerError::Unavailable("FFmpeg has no G.729 encoder".into())),
        }
    }

    /// Writes one `yuv420p` frame of the slot resolution.
    pub async fn write_video(&mut self, frame: &[u8]) -> Result<(), WorkerError> {
        let expected = self.video_frame_bytes()?;
        if frame.len() != expected {
            return Err(WorkerError::InvalidConfig(format!(
                "video frame must be {expected} bytes, got {}",
                frame.len()
            )));
        }
        self.write_all(frame).await?;
        self.stats.frames_in.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Reads one `yuv420p` frame of the slot resolution.
    pub async fn read_video(&mut self) -> Result<Vec<u8>, WorkerError> {
        let expected = self.video_frame_bytes()?;
        let frame = self.read_exact(expected).await?;
        self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
        Ok(frame)
    }

    fn video_frame_bytes(&self) -> Result<usize, WorkerError> {
        let pixels = usize::from(self.slot.width) * usize::from(self.slot.height);
        if pixels == 0 {
            return Err(WorkerError::InvalidConfig("zero video frame".into()));
        }
        Ok(pixels * 3 / 2)
    }

    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), WorkerError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(WorkerError::Exited("stdin already closed".into()));
        };
        let result = tokio::time::timeout(self.config.io_timeout, async {
            stdin.write_all(bytes).await?;
            stdin.flush().await
        })
        .await;
        match result {
            Ok(Ok(())) => {
                self.stats.bytes_in.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                Ok(())
            }
            Ok(Err(error)) => Err(self.classify_io(error)),
            Err(_) => {
                self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                Err(WorkerError::Timeout(self.config.io_timeout))
            }
        }
    }

    async fn read_exact(&mut self, len: usize) -> Result<Vec<u8>, WorkerError> {
        let mut buffer = vec![0u8; len];
        self.read_exact_into(&mut buffer).await?;
        Ok(buffer)
    }

    async fn read_exact_into(&mut self, buffer: &mut [u8]) -> Result<(), WorkerError> {
        let len = buffer.len();
        let mut filled = 0;
        while filled < len {
            let Some(stdout) = self.child.stdout.as_mut() else {
                return Err(WorkerError::Exited("stdout already taken".into()));
            };
            let read = tokio::time::timeout(self.config.io_timeout, stdout.read(&mut buffer[filled..])).await;
            match read {
                Ok(Ok(0)) => {
                    return Err(WorkerError::Exited(self.exited_reason()));
                }
                Ok(Ok(count)) => filled += count,
                Ok(Err(error)) => return Err(self.classify_io(error)),
                Err(_) => {
                    self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                    return Err(WorkerError::Timeout(self.config.io_timeout));
                }
            }
        }
        self.stats.bytes_out.fetch_add(len as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Closes stdin, letting FFmpeg flush and exit after the remaining input.
    pub async fn finish_input(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.shutdown().await;
        }
    }

    /// Reads the rest of stdout (bounded) after [`Self::finish_input`].
    pub async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, WorkerError> {
        let mut output = Vec::new();
        let stdout = self.child.stdout.as_mut().ok_or_else(|| WorkerError::Exited("stdout closed".into()))?;
        loop {
            let mut chunk = [0u8; 8192];
            let read = tokio::time::timeout(self.config.io_timeout, stdout.read(&mut chunk)).await;
            match read {
                Ok(Ok(0)) => break,
                Ok(Ok(count)) => {
                    if output.len() + count > limit {
                        return Err(WorkerError::Codec(format!("output exceeds {limit} bytes")));
                    }
                    output.extend_from_slice(&chunk[..count]);
                }
                Ok(Err(error)) => return Err(self.classify_io(error)),
                Err(_) => {
                    self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                    return Err(WorkerError::Timeout(self.config.io_timeout));
                }
            }
        }
        self.stats.bytes_out.fetch_add(output.len() as u64, Ordering::Relaxed);
        Ok(output)
    }

    /// Waits for the child and reports a non-zero exit.
    pub async fn wait(&mut self) -> Result<(), WorkerError> {
        let status = tokio::time::timeout(self.config.io_timeout, self.child.wait()).await;
        match status {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(WorkerError::Exited(format!("status {status}"))),
            Ok(Err(error)) => Err(self.classify_io(error)),
            Err(_) => Err(WorkerError::Timeout(self.config.io_timeout)),
        }
    }

    /// Kills the child immediately (room teardown / fault injection).
    pub fn kill(&mut self) {
        let _ = self.child.start_kill();
    }

    /// Restarts a dead child with the same mode and slot settings.
    pub async fn restart(&mut self) -> Result<(), WorkerError> {
        self.kill();
        let _ = self.child.wait().await;
        let (child, stdin) = spawn_child(self.mode, self.slot, &self.config, self.last_stderr.clone()).await?;
        self.child = child;
        self.stdin = Some(stdin);
        self.stats.restarts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn exited_reason(&mut self) -> String {
        let stderr = self.last_stderr();
        match self.child.try_wait() {
            Ok(Some(status)) if stderr.is_empty() => format!("status {status}"),
            Ok(Some(status)) => format!("status {status}: {stderr}"),
            _ => {
                if stderr.is_empty() {
                    "unexpected EOF".into()
                } else {
                    stderr
                }
            }
        }
    }

    fn classify_io(&self, error: std::io::Error) -> WorkerError {
        match error.kind() {
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof => {
                WorkerError::Exited(match self.last_stderr.lock() {
                    Ok(stderr) if !stderr.is_empty() => stderr.clone(),
                    _ => error.to_string(),
                })
            }
            _ => WorkerError::Io(error),
        }
    }
}

impl Drop for FfmpegWorker {
    fn drop(&mut self) {
        // `kill_on_drop` in spawn_child also fires; explicit start_kill keeps
        // teardown deterministic for tests that observe process exit.
        let _ = self.child.start_kill();
    }
}

/// Host-wide bounded pool of codec processes.
///
/// A SIP/H.323 call cap is not a CPU budget: each audio worker costs ~50 MiB
/// and each H.265 encoder up to ~250 MiB. The pool refuses new workers instead
/// of letting a busy conference exhaust the host.
pub struct WorkerPool {
    semaphore: Arc<tokio::sync::Semaphore>,
    max_workers: usize,
}

impl WorkerPool {
    /// Creates a pool capped at `max_workers` concurrent children.
    pub fn new(max_workers: usize) -> Self {
        let max_workers = max_workers.max(1);
        Self { semaphore: Arc::new(tokio::sync::Semaphore::new(max_workers)), max_workers }
    }

    /// Configured process cap.
    #[must_use]
    pub fn max_workers(&self) -> usize {
        self.max_workers
    }

    /// Currently available permits (diagnostics).
    #[must_use]
    pub fn available(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Spawns a worker, applying backpressure while the pool is full.
    pub async fn spawn(
        &self,
        mode: WorkerMode,
        slot: SlotConfig,
        config: WorkerConfig,
    ) -> Result<FfmpegWorker, WorkerError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| WorkerError::Unavailable("worker pool closed".into()))?;
        let mut worker = FfmpegWorker::spawn(mode, slot, config).await?;
        worker.permit = Some(permit);
        Ok(worker)
    }

    /// Non-blocking variant for per-frame paths: returns `Unavailable` when
    /// the pool is saturated instead of queueing the caller.
    pub async fn try_spawn(
        &self,
        mode: WorkerMode,
        slot: SlotConfig,
        config: WorkerConfig,
    ) -> Result<FfmpegWorker, WorkerError> {
        let permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| WorkerError::Unavailable("codec worker pool is full".into()))?;
        let mut worker = FfmpegWorker::spawn(mode, slot, config).await?;
        worker.permit = Some(permit);
        Ok(worker)
    }
}

async fn spawn_child(
    mode: WorkerMode,
    slot: SlotConfig,
    config: &WorkerConfig,
    last_stderr: Arc<Mutex<String>>,
) -> Result<(Child, ChildStdin), WorkerError> {
    let args = ffmpeg_args(mode, slot);
    let mut command = Command::new(&config.ffmpeg_path);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // Codec subprocesses are untrusted parsers: never inherit the MCU's
        // environment (it contains JWT secrets and DB credentials).
        .env_clear()
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()))
        .current_dir("/");
    #[cfg(unix)]
    {
        // Own process group: a kill of the worker cannot signal the MCU.
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(WorkerError::Spawn)?;
    let stdin = child.stdin.take().ok_or_else(|| WorkerError::Spawn(std::io::Error::other("worker stdin missing")))?;

    if let Some(mut stderr) = child.stderr.take() {
        let last = last_stderr.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let text = String::from_utf8_lossy(&buffer[..count]);
                        if let Ok(mut slot) = last.lock() {
                            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                                slot.clear();
                                slot.push_str(line.trim());
                            }
                        }
                    }
                }
            }
        });
    }

    // Catch invalid FFmpeg arguments immediately instead of on the first
    // frame; this is the common failure mode for a codec that is advertised
    // but cannot start at the requested resolution.
    tokio::time::sleep(Duration::from_millis(30)).await;
    if let Ok(Some(status)) = child.try_wait() {
        let stderr = last_stderr.lock().map(|s| s.clone()).unwrap_or_default();
        return Err(WorkerError::Unavailable(format!("ffmpeg exited at startup ({status}): {stderr}")));
    }
    Ok((child, stdin))
}

fn ffmpeg_args(mode: WorkerMode, slot: SlotConfig) -> Vec<String> {
    // `-fflags nobuffer`/`-flags low_delay` make FFmpeg drop every frame for
    // some decoders (observed for raw H.261 and AAC), so the worker stays
    // with default buffering and relies on small raw-frame reads instead.
    // `-probesize 32 -analyzeduration 0` are essential for real-time pipes:
    // without them FFmpeg buffers roughly 1.6 KiB..5 s of input before the
    // first output frame, which adds hundreds of milliseconds of latency.
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-probesize", "32", "-analyzeduration", "0"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    match mode {
        WorkerMode::AudioDecode(codec) => {
            args.extend(["-f", codec.ffmpeg_name(), "-i", "pipe:0"].map(str::to_string));
            args.extend(
                ["-f", "s16le", "-acodec", "pcm_s16le", "-ar", "48000", "-ac", "1", "pipe:1"].map(str::to_string),
            );
        }
        WorkerMode::AudioEncode(codec) => {
            let encoder = codec.ffmpeg_encoder().unwrap_or("aac");
            // Native AAC only; G.729 encode is rejected before spawn.
            args.extend(["-f", "s16le", "-ar", "48000", "-ac", "1", "-i", "pipe:0"].map(str::to_string));
            args.extend(
                ["-c:a", encoder, "-b:a", "64000", "-f", codec.ffmpeg_container(), "pipe:1"].map(str::to_string),
            );
        }
        WorkerMode::VideoDecode(codec) => {
            args.extend(["-f", codec.ffmpeg_name(), "-i", "pipe:0"].map(str::to_string));
            args.extend(
                [
                    "-vf",
                    &format!("scale={}:{}", slot.width, slot.height),
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "pipe:1",
                ]
                .map(str::to_string),
            );
        }
        WorkerMode::VideoEncode(codec) => {
            args.extend(
                [
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-s",
                    &format!("{}x{}", slot.width, slot.height),
                    "-r",
                    &slot.fps.to_string(),
                    "-i",
                    "pipe:0",
                ]
                .map(str::to_string),
            );
            let encoder = codec.ffmpeg_encoder();
            args.extend(
                ["-c:v", encoder, "-b:v", &slot.bitrate_bps.to_string(), "-g", &slot.fps.to_string()]
                    .map(str::to_string),
            );
            if codec == VideoCodec::H265 {
                args.extend(["-preset", "ultrafast", "-x265-params", "log-level=error"].map(str::to_string));
            }
            args.extend(["-f", codec.ffmpeg_name(), "pipe:1"].map(str::to_string));
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|out| out.status.success())
    }

    fn pcm_sine(frames: usize) -> Vec<i16> {
        let mut pcm = Vec::with_capacity(frames * super::super::AUDIO_FRAME_SAMPLES);
        for frame in 0..frames {
            for sample in 0..super::super::AUDIO_FRAME_SAMPLES {
                let t = (frame * super::super::AUDIO_FRAME_SAMPLES + sample) as f32 / 48_000.0;
                let value = (t * 440.0 * std::f32::consts::TAU).sin() * 12_000.0;
                pcm.push(value as i16);
            }
        }
        pcm
    }

    fn yuv_gradient(width: u16, height: u16, frame: u8) -> Vec<u8> {
        let pixels = usize::from(width) * usize::from(height);
        let mut data = Vec::with_capacity(pixels * 3 / 2);
        for index in 0..pixels {
            data.push(((index % 256) as u8).wrapping_add(frame));
        }
        data.resize(pixels * 3 / 2, 128);
        data
    }

    #[tokio::test]
    async fn probe_reports_host_codecs() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg missing; skip");
            return;
        }
        let caps = FfmpegCapabilities::probe(std::path::Path::new("ffmpeg")).await.expect("probe");
        assert!(caps.version.contains("ffmpeg"), "{}", caps.version);
        assert!(caps.has_audio_decoder(AudioCodec::Aac));
        assert!(caps.has_audio_decoder(AudioCodec::G729));
        assert!(caps.has_audio_encoder(AudioCodec::Aac));
        assert!(!caps.has_audio_encoder(AudioCodec::G729));
        assert!(caps.has_video_decoder(VideoCodec::H261));
        assert!(caps.has_video_encoder(VideoCodec::H263));
    }

    #[tokio::test]
    async fn aac_audio_roundtrip_preserves_signal() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig::default_720p();
        let config = WorkerConfig::default();
        let pcm = pcm_sine(5);
        let mut encoder =
            FfmpegWorker::spawn(WorkerMode::AudioEncode(AudioCodec::Aac), slot, config.clone()).await.expect("encoder");
        for frame in pcm.chunks_exact(super::super::AUDIO_FRAME_SAMPLES) {
            encoder.write_audio(frame).await.expect("write pcm");
        }
        // Streaming encoder must produce the first ADTS frame without EOF.
        let first = encoder.read_encoded_audio().await.expect("streaming ADTS frame");
        assert_eq!(&first[..2], &[0xff, 0xf1]);
        encoder.finish_input().await;
        let encoded = encoder.read_to_end(1 << 20).await.expect("remaining bytes");
        assert!(encoded.len() > 100, "AAC stream tail too short: {}", encoded.len());
        encoder.wait().await.ok();

        let mut decoder =
            FfmpegWorker::spawn(WorkerMode::AudioDecode(AudioCodec::Aac), slot, config).await.expect("decoder");
        // ADTS output can be written as one blob, then decoded to EOF.
        decoder.stdin.as_mut().unwrap().write_all(&first).await.expect("write adts header frame");
        decoder.stdin.as_mut().unwrap().write_all(&encoded).await.expect("write adts tail");
        decoder.finish_input().await;
        let decoded = decoder.read_to_end(1 << 24).await.expect("decoded pcm");
        assert!(decoded.len() >= super::super::AUDIO_FRAME_BYTES, "decoded {} bytes", decoded.len());
        let energy: f64 = decoded
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f64)
            .map(|sample| sample * sample)
            .sum();
        assert!(energy > 1.0e8, "AAC roundtrip is silent: {energy}");
    }

    #[tokio::test]
    async fn aac_encoded_frames_are_self_delimiting() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig::default_720p();
        let config = WorkerConfig::default();
        let pcm = pcm_sine(40);
        for codec in [AudioCodec::Aac, AudioCodec::AacLatm] {
            let mut encoder =
                FfmpegWorker::spawn(WorkerMode::AudioEncode(codec), slot, config.clone()).await.expect("encoder");
            for frame in pcm.chunks_exact(super::super::AUDIO_FRAME_SAMPLES).take(20) {
                encoder.write_audio(frame).await.expect("pcm");
            }
            for _ in 0..5 {
                let frame = encoder.read_encoded_audio().await.expect("encoded frame");
                match codec {
                    AudioCodec::Aac => assert_eq!(&frame[..2], &[0xff, 0xf1]),
                    AudioCodec::AacLatm => assert_eq!(&frame[..2], &[0x56, 0xe0]),
                    AudioCodec::G729 => unreachable!(),
                }
                assert!(frame.len() > 7, "{codec:?} frame too short");
            }
            encoder.finish_input().await;
            encoder.wait().await.ok();
        }
    }

    #[tokio::test]
    async fn latm_audio_roundtrip_produces_pcm() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig::default_720p();
        let config = WorkerConfig::default();
        let pcm = pcm_sine(30);
        let mut encoder = FfmpegWorker::spawn(WorkerMode::AudioEncode(AudioCodec::AacLatm), slot, config.clone())
            .await
            .expect("encoder");
        for frame in pcm.chunks_exact(super::super::AUDIO_FRAME_SAMPLES).take(20) {
            encoder.write_audio(frame).await.expect("pcm");
        }
        let first = encoder.read_encoded_audio().await.expect("latm frame");
        let second = encoder.read_encoded_audio().await.expect("latm frame");
        encoder.finish_input().await;
        encoder.wait().await.ok();

        let mut decoder =
            FfmpegWorker::spawn(WorkerMode::AudioDecode(AudioCodec::AacLatm), slot, config).await.expect("decoder");
        decoder.stdin.as_mut().unwrap().write_all(&first).await.expect("latm");
        decoder.stdin.as_mut().unwrap().write_all(&second).await.expect("latm");
        decoder.finish_input().await;
        let decoded = decoder.read_to_end(1 << 24).await.expect("pcm");
        assert!(decoded.len() >= 1024 * 2, "LATM decoded {} bytes", decoded.len());
    }

    #[tokio::test]
    async fn video_roundtrip_for_supported_codecs() {
        if !ffmpeg_available() {
            return;
        }
        for (codec, width, height) in
            [(VideoCodec::H261, 176, 144), (VideoCodec::H263, 176, 144), (VideoCodec::H265, 176, 144)]
        {
            let slot = SlotConfig { bitrate_bps: 200_000, width, height, fps: 10, max_queue: 4 };
            let config = WorkerConfig::default();
            let mut encoder = match FfmpegWorker::spawn(WorkerMode::VideoEncode(codec), slot, config.clone()).await {
                Ok(worker) => worker,
                Err(WorkerError::Unavailable(_)) => continue,
                Err(error) => panic!("{codec:?} encoder: {error}"),
            };
            for frame in 0..3u8 {
                encoder.write_video(&yuv_gradient(width, height, frame)).await.expect("write frame");
            }
            encoder.finish_input().await;
            let bitstream = encoder.read_to_end(4 << 20).await.expect("bitstream");
            assert!(!bitstream.is_empty(), "{codec:?} produced no bytes");
            encoder.wait().await.ok();

            let mut decoder = FfmpegWorker::spawn(WorkerMode::VideoDecode(codec), slot, config)
                .await
                .unwrap_or_else(|error| panic!("{codec:?} decoder: {error}"));
            decoder.stdin.as_mut().unwrap().write_all(&bitstream).await.expect("write bitstream");
            decoder.finish_input().await;
            let frames = decoder.read_to_end(4 << 20).await.expect("decoded frames");
            let expected = usize::from(width) * usize::from(height) * 3 / 2;
            assert!(frames.len() >= expected, "{codec:?} decoded {} < {expected}", frames.len());
        }
    }

    #[tokio::test]
    async fn worker_pool_refuses_over_capacity() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig { bitrate_bps: 200_000, width: 176, height: 144, fps: 10, max_queue: 4 };
        let pool = WorkerPool::new(1);
        let first = pool
            .spawn(WorkerMode::VideoDecode(VideoCodec::H261), slot, WorkerConfig::default())
            .await
            .expect("first worker");
        assert_eq!(pool.available(), 0);
        let second = pool.try_spawn(WorkerMode::VideoDecode(VideoCodec::H261), slot, WorkerConfig::default()).await;
        assert!(matches!(second, Err(WorkerError::Unavailable(_))), "pool must refuse, not queue");
        drop(first);
        assert_eq!(pool.available(), 1, "dropping a worker must return its permit");
    }

    #[tokio::test]
    async fn child_env_is_scrubbed_and_restart_changes_pid() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig { bitrate_bps: 200_000, width: 176, height: 144, fps: 10, max_queue: 4 };
        let mut worker = FfmpegWorker::spawn(
            WorkerMode::VideoDecode(VideoCodec::H261),
            slot,
            WorkerConfig { io_timeout: Duration::from_millis(300), ..WorkerConfig::default() },
        )
        .await
        .expect("worker");
        let first_pid = worker.pid().expect("pid");
        let environ = std::fs::read(format!("/proc/{first_pid}/environ")).unwrap_or_default();
        let environ = String::from_utf8_lossy(&environ);
        assert!(!environ.contains("VKS_"), "MCU secrets leaked into codec child: {environ}");
        assert!(environ.contains("LC_ALL=C"), "minimal locale missing: {environ}");
        worker.restart().await.expect("restart");
        let second_pid = worker.pid().expect("pid");
        assert_ne!(first_pid, second_pid, "restart must relaunch the process");
        assert!(worker.is_running());
    }

    #[tokio::test]
    async fn malformed_payload_fails_closed_and_restarts() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig { bitrate_bps: 200_000, width: 176, height: 144, fps: 10, max_queue: 4 };
        let mut worker = FfmpegWorker::spawn(
            WorkerMode::VideoDecode(VideoCodec::H261),
            slot,
            WorkerConfig { io_timeout: Duration::from_millis(400), ..WorkerConfig::default() },
        )
        .await
        .expect("worker");
        let started = Instant::now();
        worker.stdin.as_mut().unwrap().write_all(&[0xff; 64]).await.expect("write junk");
        let result = worker.read_video().await;
        assert!(result.is_err(), "garbage must not yield a frame");
        assert!(started.elapsed() < Duration::from_secs(3), "garbage handling must not hang");
        worker.restart().await.expect("restart");
        assert!(worker.is_running());
    }

    #[tokio::test]
    async fn video_frame_size_validation_is_exact() {
        let slot = SlotConfig { bitrate_bps: 200_000, width: 176, height: 144, fps: 10, max_queue: 4 };
        // Spawning needs ffmpeg; validation itself is checked without it.
        let expected = usize::from(slot.width) * usize::from(slot.height) * 3 / 2;
        assert_eq!(expected, 38_016);
        if !ffmpeg_available() {
            return;
        }
        let mut worker = FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H261), slot, WorkerConfig::default())
            .await
            .expect("worker");
        let error = worker.write_video(&vec![0u8; expected + 1]).await.unwrap_err();
        assert!(matches!(error, WorkerError::InvalidConfig(_)));
    }
}
