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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Idle timeout for one pipe read/write (not a total frame deadline: a
    /// multi-chunk frame may take several reads) and for child reaping.
    pub io_timeout: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        let ffmpeg_path = std::env::var_os("VKS_FFMPEG_BIN").map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from);
        Self { ffmpeg_path, io_timeout: Duration::from_secs(5) }
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
#[non_exhaustive]
pub enum WorkerError {
    /// Codec/FFmpeg is not installed on this host (permanent: fall back to
    /// the native profile).
    #[error("codec worker unavailable: {0}")]
    Unavailable(String),
    /// Host process budget is saturated (transient: retry after a slot ends).
    #[error("codec worker pool is full")]
    PoolBusy,
    /// Pool was shut down (permanent for this process).
    #[error("codec worker pool is closed")]
    PoolClosed,
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
    muxers: BTreeSet<String>,
    demuxers: BTreeSet<String>,
}

impl FfmpegCapabilities {
    /// Probes the configured FFmpeg binary.
    pub async fn probe(ffmpeg_path: &std::path::Path) -> Result<Self, WorkerError> {
        let mut version_command = command(ffmpeg_path);
        version_command.arg("-hide_banner").arg("-version");
        let version_out = run_bounded(version_command).await?;
        if !version_out.status.success() {
            return Err(WorkerError::Unavailable(format!(
                "{} -version failed: {}",
                ffmpeg_path.display(),
                String::from_utf8_lossy(&version_out.stderr).trim()
            )));
        }
        let version = String::from_utf8_lossy(&version_out.stdout).lines().next().unwrap_or_default().to_string();

        let encoders = list_codecs(ffmpeg_path, "-encoders").await?;
        let decoders = list_codecs(ffmpeg_path, "-decoders").await?;
        let muxers = list_formats(ffmpeg_path, "-muxers").await?;
        let demuxers = list_formats(ffmpeg_path, "-demuxers").await?;
        Ok(Self { version, decoders, encoders, muxers, demuxers })
    }

    /// Whether the host can decode this audio codec.
    #[must_use]
    pub fn has_audio_decoder(&self, codec: AudioCodec) -> bool {
        self.decoders.contains(codec.decoder_name())
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

    /// Whether the host can carry MP4A-LATM in both directions: an AAC encoder
    /// plus the `latm` muxer, and the `aac_latm` decoder plus the `loas`
    /// demuxer. Codec presence alone is not enough; containers can be built
    /// out separately.
    #[must_use]
    pub fn has_audio_latm_bridge(&self) -> bool {
        self.has_audio_encoder(AudioCodec::AacLatm)
            && self.has_audio_decoder(AudioCodec::AacLatm)
            && self.muxers.contains("latm")
            && self.demuxers.contains("loas")
    }

    /// Whether a muxer was listed by `ffmpeg -muxers`.
    #[must_use]
    pub fn has_muxer(&self, name: &str) -> bool {
        self.muxers.contains(name)
    }

    /// Whether a demuxer was listed by `ffmpeg -demuxers`.
    #[must_use]
    pub fn has_demuxer(&self, name: &str) -> bool {
        self.demuxers.contains(name)
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

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for one encoded video frame (4K IDR headroom; exceeds it =
/// corrupted stream).
const MAX_ENCODED_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Parent-side read chunk; distinct from codec-defined length caps.
const IO_CHUNK_BYTES: usize = 8192;
/// Give FFmpeg a moment to fail on bad CLI arguments before the first I/O.
const STARTUP_SETTLE: Duration = Duration::from_millis(30);

/// Среда дочернего FFmpeg: пусто, кроме локали, `PATH` и (если задан)
/// `LD_LIBRARY_PATH` — последний нужен для ffmpeg из приватного префикса
/// (`VKS_FFMPEG_BIN`), который грузит свои `.so` не из `ld.so.conf`.
/// Остальное окружение сервера (JWT-секреты, креды БД) наружу не уходит.
fn scrubbed_env(command: &mut Command) -> &mut Command {
    command
        .env_clear()
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()));
    if let Some(loader) = std::env::var_os("LD_LIBRARY_PATH") {
        command.env("LD_LIBRARY_PATH", loader);
    }
    command
}

fn command(ffmpeg: &std::path::Path) -> Command {
    let mut command = Command::new(ffmpeg);
    command.kill_on_drop(true);
    scrubbed_env(&mut command);
    #[cfg(unix)]
    {
        // Собственная группа процессов: таймаут и teardown убивают её целиком.
        command.process_group(0);
    }
    command
}

/// Убивает всю группу процессов воркера (`kill(-pgid, SIGKILL)`).
///
/// Один syscall вместо порождения `/usr/bin/kill`: вызов неблокирующий, поэтому
/// применим и в `Drop`, и в async-пути (PROJECT_RULES §1.2 — без блокирующих
/// вызовов в сетевом/медиа-пути). Ошибка «группа уже мертва» игнорируется.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: `pid` — идентификатор дочернего процесса этого воркера; отрицательный
    // pid в `kill()` означает «вся группа процессов». `ESRCH` (группа уже мертва)
    // и `EPERM` игнорируются — вызывающая сторона всё равно делает `start_kill()`.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Runs one bounded probe command in its own process group. A timeout kills
/// the whole group (not just the leader), so a shell/forked helper cannot be
/// reparented and leak.
async fn run_bounded(mut command: Command) -> Result<std::process::Output, WorkerError> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn().map_err(WorkerError::Spawn)?;
    let pid = child.id();
    let timed_out = Arc::new(AtomicBool::new(false));
    let watchdog_flag = Arc::clone(&timed_out);
    let watchdog = tokio::spawn(async move {
        tokio::time::sleep(PROBE_TIMEOUT).await;
        watchdog_flag.store(true, Ordering::SeqCst);
        if let Some(pid) = pid {
            kill_process_group(pid);
        }
    });
    let output = child.wait_with_output().await.map_err(WorkerError::Spawn)?;
    watchdog.abort();
    if timed_out.load(Ordering::SeqCst) {
        return Err(WorkerError::Timeout(PROBE_TIMEOUT));
    }
    Ok(output)
}

async fn list_codecs(ffmpeg: &std::path::Path, flag: &str) -> Result<BTreeSet<String>, WorkerError> {
    let mut command = command(ffmpeg);
    command.arg("-hide_banner").arg(flag);
    let output = run_bounded(command).await?;
    if !output.status.success() {
        return Err(WorkerError::Unavailable(format!(
            "{} {flag} failed: {}",
            ffmpeg.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
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
        // The trailing legend uses the same six-dot shape (`D..... = Decoding
        // supported`); never insert its `=` token as a codec name.
        if rest.trim_start().starts_with('=') {
            continue;
        }
        // Флаги FFmpeg — заглавные буквы и точки (`V....D`, `A....D`). Легенда
        // в конце вывода (`D..... = Decoding supported`) такую строку не даёт,
        // но если формат изменится — имя кодека вставится в набор мусором.
        if !flags.chars().all(|symbol| symbol.is_ascii_uppercase() || symbol == '.') {
            continue;
        }
        if let Some(name) = rest.split_whitespace().next() {
            names.insert(name.to_string());
        }
    }
    Ok(names)
}

async fn list_formats(ffmpeg: &std::path::Path, flag: &str) -> Result<BTreeSet<String>, WorkerError> {
    let mut command = command(ffmpeg);
    command.arg("-hide_banner").arg(flag);
    let output = run_bounded(command).await?;
    if !output.status.success() {
        return Err(WorkerError::Unavailable(format!(
            "{} {flag} failed: {}",
            ffmpeg.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(parse_format_list(&String::from_utf8_lossy(&output.stdout)))
}

/// Parses `ffmpeg -muxers`/`-demuxers` output:
/// `  E  latm             MPEG-4 LATM AAC`.
fn parse_format_list(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim_start();
        let mut tokens = line.split_whitespace();
        let Some(flags) = tokens.next() else { continue };
        let Some(name) = tokens.next() else { continue };
        if !flags.chars().all(|symbol| matches!(symbol, 'D' | 'E' | '.')) {
            continue;
        }
        if name == "=" || name.is_empty() {
            continue;
        }
        names.insert(name.to_string());
    }
    names
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
    poisoned: bool,
    encoded_pending: Vec<u8>,
    encoded_eof: bool,
    pub stats: Arc<WorkerStats>,
}

impl FfmpegWorker {
    /// Spawns a worker after validating codec-specific slot ranges.
    ///
    /// This does not consult host FFmpeg encoders; use [`Self::spawn_checked`]
    /// (or [`WorkerPool`] with capabilities attached) when availability must be
    /// enforced before the first frame.
    pub async fn spawn(mode: WorkerMode, slot: SlotConfig, config: WorkerConfig) -> Result<Self, WorkerError> {
        Self::spawn_internal(mode, slot, config, None).await
    }

    /// Spawns a worker and rejects a mode the probed FFmpeg cannot run.
    pub async fn spawn_checked(
        mode: WorkerMode,
        slot: SlotConfig,
        config: WorkerConfig,
        capabilities: &FfmpegCapabilities,
    ) -> Result<Self, WorkerError> {
        Self::spawn_internal(mode, slot, config, Some(capabilities)).await
    }

    async fn spawn_internal(
        mode: WorkerMode,
        slot: SlotConfig,
        config: WorkerConfig,
        capabilities: Option<&FfmpegCapabilities>,
    ) -> Result<Self, WorkerError> {
        match mode {
            WorkerMode::VideoEncode(codec) => slot.validate_for(codec)?,
            WorkerMode::VideoDecode(codec) => slot.validate_decode(codec)?,
            WorkerMode::AudioEncode(_) | WorkerMode::AudioDecode(_) => slot.validate_audio()?,
        }
        if matches!(mode, WorkerMode::AudioEncode(AudioCodec::G729)) {
            return Err(WorkerError::Unavailable("FFmpeg has no G.729 encoder".into()));
        }
        if let Some(capabilities) = capabilities {
            if !capabilities.supports(mode) {
                return Err(WorkerError::Unavailable(format!("{mode:?} is not supported by this FFmpeg")));
            }
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
            poisoned: false,
            encoded_pending: Vec::new(),
            encoded_eof: false,
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

    /// Whether a partial read/write desynchronised the pipe. A poisoned worker
    /// must be [`Self::restart`]ed before further frames are exchanged.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn require_audio_encode(&self) -> Result<(), WorkerError> {
        match self.mode {
            WorkerMode::AudioEncode(_) => Ok(()),
            other => Err(WorkerError::InvalidConfig(format!("write_audio requires AudioEncode, got {other:?}"))),
        }
    }

    fn require_audio_decode(&self) -> Result<(), WorkerError> {
        match self.mode {
            WorkerMode::AudioDecode(_) => Ok(()),
            other => Err(WorkerError::InvalidConfig(format!("read_audio requires AudioDecode, got {other:?}"))),
        }
    }

    fn require_video_encode(&self) -> Result<(), WorkerError> {
        match self.mode {
            WorkerMode::VideoEncode(_) => Ok(()),
            other => Err(WorkerError::InvalidConfig(format!("write_video requires VideoEncode, got {other:?}"))),
        }
    }

    fn require_video_decode(&self) -> Result<(), WorkerError> {
        match self.mode {
            WorkerMode::VideoDecode(_) => Ok(()),
            other => Err(WorkerError::InvalidConfig(format!("read_video requires VideoDecode, got {other:?}"))),
        }
    }

    fn require_encoded_input(&self) -> Result<(), WorkerError> {
        match self.mode {
            WorkerMode::AudioDecode(_) | WorkerMode::VideoDecode(_) => Ok(()),
            other => Err(WorkerError::InvalidConfig(format!("write_encoded requires a decoder, got {other:?}"))),
        }
    }

    /// Writes one 20 ms mono PCM frame.
    pub async fn write_audio(&mut self, pcm: &[i16]) -> Result<(), WorkerError> {
        self.require_audio_encode()?;
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

    /// Reads exactly `samples` mono `s16le` PCM samples.
    pub async fn read_audio_samples(&mut self, samples: usize) -> Result<Vec<i16>, WorkerError> {
        self.require_audio_decode()?;
        if samples == 0 || samples > super::WORKER_SAMPLE_RATE as usize {
            return Err(WorkerError::InvalidConfig(format!("audio block size {samples} outside 1..=48000")));
        }
        let bytes = self.read_exact(samples * 2).await?;
        self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
        Ok(bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect())
    }

    /// Writes raw encoded input bytes to a decoder worker (RTP payload,
    /// ADTS/LOAS frame, or a video bitstream fragment). No framing is added;
    /// the caller must feed complete codec frames for deterministic output.
    pub async fn write_encoded(&mut self, bytes: &[u8]) -> Result<(), WorkerError> {
        self.require_encoded_input()?;
        self.write_all(bytes).await
    }

    /// Reads one Annex-B NAL unit from an H.265 encoder worker.
    ///
    /// Returns the NAL **including** its start code, or `Ok(None)` at clean EOF
    /// after [`Self::finish_input`] (errors are never reported as end-of-stream).
    /// The last NAL of an access
    /// unit is released only when the next access unit's start code arrives;
    /// a reader must therefore keep one access unit of lookahead and use an
    /// `io_timeout` longer than a paused-stream gap. H.261/H.263 raw FFmpeg
    /// output is not Annex-B and is rejected here: those encoders are EOF-only
    /// via [`Self::read_to_end`] after [`Self::finish_input`].
    pub async fn read_encoded_video(&mut self) -> Result<Option<Vec<u8>>, WorkerError> {
        self.require_video_encode()?;
        if matches!(self.mode, WorkerMode::VideoEncode(codec) if codec != VideoCodec::H265) {
            return Err(WorkerError::InvalidConfig(
                "no Annex-B framer for this video codec; use read_to_end after finish_input".into(),
            ));
        }
        if self.poisoned {
            return Err(WorkerError::Exited("worker is poisoned; restart is required".into()));
        }
        loop {
            if self.encoded_eof && self.encoded_pending.is_empty() {
                return Ok(None);
            }
            if let Some(nal) = take_annexb_nal(&mut self.encoded_pending, self.encoded_eof) {
                self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
                self.stats.bytes_out.fetch_add(nal.len() as u64, Ordering::Relaxed);
                return Ok(Some(nal));
            }
            let mut chunk = [0u8; IO_CHUNK_BYTES];
            let read = {
                let Some(stdout) = self.child.stdout.as_mut() else {
                    return Err(WorkerError::Exited("stdout already taken".into()));
                };
                tokio::time::timeout(self.config.io_timeout, stdout.read(&mut chunk)).await
            };
            match read {
                Ok(Ok(0)) => {
                    self.encoded_eof = true;
                    if self.encoded_pending.len() > MAX_ENCODED_FRAME_BYTES {
                        return Err(self.poison(WorkerError::Codec(format!(
                            "encoded video frame exceeds {MAX_ENCODED_FRAME_BYTES} bytes"
                        ))));
                    }
                }
                Ok(Ok(count)) => {
                    self.encoded_pending.extend_from_slice(&chunk[..count]);
                    if self.encoded_pending.len() > MAX_ENCODED_FRAME_BYTES {
                        return Err(self.poison(WorkerError::Codec(format!(
                            "encoded video frame exceeds {MAX_ENCODED_FRAME_BYTES} bytes"
                        ))));
                    }
                }
                Ok(Err(error)) => {
                    let error = self.classify_io(error);
                    return Err(self.poison(error));
                }
                Err(_) => {
                    self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                    return Err(self.poison(WorkerError::Timeout(self.config.io_timeout)));
                }
            }
        }
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
                    return Err(
                        self.poison(WorkerError::Codec(format!("bad ADTS sync {:02x}{:02x}", header[0], header[1])))
                    );
                }
                let length = (usize::from(header[3] & 0x03) << 11)
                    | (usize::from(header[4]) << 3)
                    | (usize::from(header[5]) >> 5);
                if !(7..=8192).contains(&length) {
                    return Err(self.poison(WorkerError::Codec(format!("invalid ADTS length {length}"))));
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
                    return Err(
                        self.poison(WorkerError::Codec(format!("bad LOAS sync {:02x}{:02x}", header[0], header[1])))
                    );
                }
                let length = (usize::from(header[1] & 0x1f) << 8) | usize::from(header[2]);
                if length == 0 || length > 8192 {
                    return Err(self.poison(WorkerError::Codec(format!("invalid LOAS length {length}"))));
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
        self.require_video_encode()?;
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
        self.require_video_decode()?;
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
        if self.poisoned {
            return Err(WorkerError::Exited("worker is poisoned; restart is required".into()));
        }
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
            Ok(Err(error)) => {
                let error = self.classify_io(error);
                Err(self.poison(error))
            }
            Err(_) => {
                self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                Err(self.poison(WorkerError::Timeout(self.config.io_timeout)))
            }
        }
    }

    async fn read_exact(&mut self, len: usize) -> Result<Vec<u8>, WorkerError> {
        let mut buffer = vec![0u8; len];
        self.read_exact_into(&mut buffer).await?;
        Ok(buffer)
    }

    async fn read_exact_into(&mut self, buffer: &mut [u8]) -> Result<(), WorkerError> {
        if self.poisoned {
            return Err(WorkerError::Exited("worker is poisoned; restart is required".into()));
        }
        let len = buffer.len();
        let mut filled = 0;
        while filled < len {
            let Some(stdout) = self.child.stdout.as_mut() else {
                return Err(WorkerError::Exited("stdout already taken".into()));
            };
            let read = tokio::time::timeout(self.config.io_timeout, stdout.read(&mut buffer[filled..])).await;
            match read {
                Ok(Ok(0)) => {
                    let error = WorkerError::Exited(self.exited_reason());
                    return Err(self.poison(error));
                }
                Ok(Ok(count)) => filled += count,
                Ok(Err(error)) => {
                    let error = self.classify_io(error);
                    return Err(self.poison(error));
                }
                Err(_) => {
                    self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                    return Err(self.poison(WorkerError::Timeout(self.config.io_timeout)));
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
        if self.poisoned {
            return Err(WorkerError::Exited("worker is poisoned; restart is required".into()));
        }
        let mut output = Vec::new();
        let stdout = self.child.stdout.as_mut().ok_or_else(|| WorkerError::Exited("stdout closed".into()))?;
        loop {
            let mut chunk = [0u8; IO_CHUNK_BYTES];
            let read = tokio::time::timeout(self.config.io_timeout, stdout.read(&mut chunk)).await;
            match read {
                Ok(Ok(0)) => break,
                Ok(Ok(count)) => {
                    if output.len() + count > limit {
                        return Err(self.poison(WorkerError::Codec(format!("output exceeds {limit} bytes"))));
                    }
                    output.extend_from_slice(&chunk[..count]);
                }
                Ok(Err(error)) => {
                    let error = self.classify_io(error);
                    return Err(self.poison(error));
                }
                Err(_) => {
                    self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                    return Err(self.poison(WorkerError::Timeout(self.config.io_timeout)));
                }
            }
        }
        self.stats.bytes_out.fetch_add(output.len() as u64, Ordering::Relaxed);
        // H.263+ is encoded through the AVI muxer (the raw `h263` muxer
        // refuses the `h263p` encoder): unwrap the elementary stream back out
        // so the caller always sees a plain codec bitstream.
        if let WorkerMode::VideoEncode(codec) = self.mode {
            if codec.needs_avi_container() {
                let es = extract_avi_video_es(&output)?;
                self.stats.bytes_out.fetch_add(es.len() as u64, Ordering::Relaxed);
                return Ok(es);
            }
        }
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

    /// Kills the child process group immediately (room teardown / fault
    /// injection). FFmpeg has no children today, but the group is killed so a
    /// future helper process cannot outlive the worker.
    pub fn kill(&mut self) {
        if let Some(pid) = self.child.id() {
            kill_process_group(pid);
        }
        let _ = self.child.start_kill();
    }

    /// Marks the worker desynchronised and kills the child. The caller must
    /// `restart()` before reusing it.
    fn poison(&mut self, error: WorkerError) -> WorkerError {
        self.poisoned = true;
        self.stdin = None;
        self.kill();
        error
    }

    /// Restarts a dead or poisoned child with the same mode and slot settings.
    pub async fn restart(&mut self) -> Result<(), WorkerError> {
        self.poisoned = true;
        self.kill();
        if tokio::time::timeout(self.config.io_timeout, self.child.wait()).await.is_err() {
            let _ = self.child.start_kill();
        }
        self.stdin = None;
        self.encoded_pending.clear();
        self.encoded_eof = false;
        // Новое поколение буфера: дренаж stderr уже умершего ребёнка пишет в
        // свою строку и не может перемешать вывод со stderr нового процесса.
        self.last_stderr = Arc::new(Mutex::new(String::new()));
        let (child, stdin) = spawn_child(self.mode, self.slot, &self.config, self.last_stderr.clone()).await?;
        self.child = child;
        self.stdin = Some(stdin);
        self.poisoned = false;
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
        // `kill_on_drop` also fires, but it signals only the leader. The child
        // is still unreaped here, so its pgid cannot have been reused: killing
        // the group is safe and takes any helper down with the worker.
        if let Some(pid) = self.child.id() {
            kill_process_group(pid);
        }
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
    capabilities: Option<Arc<FfmpegCapabilities>>,
}

impl WorkerPool {
    /// Creates a pool capped at `max_workers` concurrent children.
    pub fn new(max_workers: usize) -> Self {
        Self::with_capabilities(max_workers, None)
    }

    /// Creates a pool that rejects modes the probed FFmpeg cannot run.
    pub fn with_capabilities(max_workers: usize, capabilities: Option<FfmpegCapabilities>) -> Self {
        let max_workers = max_workers.max(1);
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_workers)),
            max_workers,
            capabilities: capabilities.map(Arc::new),
        }
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

    /// Atomically reserves `permits` worker slots for a pipeline that spawns
    /// FFmpeg directly (AAC-LATM's split-pipe pair). The returned permits must
    /// live as long as the children; dropping them returns the slots.
    #[must_use]
    pub fn try_reserve(&self, permits: usize) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let permits = u32::try_from(permits).ok()?;
        self.semaphore.clone().try_acquire_many_owned(permits).ok()
    }

    /// Spawns a worker, applying backpressure while the pool is full.
    pub async fn spawn(
        &self,
        mode: WorkerMode,
        slot: SlotConfig,
        config: WorkerConfig,
    ) -> Result<FfmpegWorker, WorkerError> {
        let permit = self.semaphore.clone().acquire_owned().await.map_err(|_| WorkerError::PoolClosed)?;
        let mut worker = match self.capabilities.as_deref() {
            Some(capabilities) => FfmpegWorker::spawn_checked(mode, slot, config, capabilities).await?,
            None => FfmpegWorker::spawn(mode, slot, config).await?,
        };
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
        let permit = self.semaphore.clone().try_acquire_owned().map_err(|_| WorkerError::PoolBusy)?;
        let mut worker = match self.capabilities.as_deref() {
            Some(capabilities) => FfmpegWorker::spawn_checked(mode, slot, config, capabilities).await?,
            None => FfmpegWorker::spawn(mode, slot, config).await?,
        };
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
    let mut command = command(&config.ffmpeg_path);
    command.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).current_dir("/");
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
    tokio::time::sleep(STARTUP_SETTLE).await;
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
                [
                    "-c:a",
                    encoder,
                    "-b:a",
                    &slot.audio_bitrate_bps.to_string(),
                    "-f",
                    codec.ffmpeg_container(),
                    "pipe:1",
                ]
                .map(str::to_string),
            );
        }
        WorkerMode::VideoDecode(codec) => {
            args.extend(["-f", codec.ffmpeg_name(), "-i", "pipe:0"].map(str::to_string));
            args.extend(
                [
                    "-vf",
                    &format!("scale={}:{}", slot.width, slot.height),
                    "-fps_mode",
                    "passthrough",
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
                // Real-time: no B-frames/lookahead, otherwise the first frame
                // is 5+ frames late and encode/decode 1-in/1-out desyncs.
                args.extend(
                    [
                        "-preset",
                        "ultrafast",
                        "-tune",
                        "zerolatency",
                        "-x265-params",
                        "log-level=error:bframes=0:rc-lookahead=0:scenecut=0:frame-threads=1:pools=1:aud=1:repeat-headers=1",
                    ]
                    .map(str::to_string),
                );
            }
            args.extend(["-f", ffmpeg_muxer(codec), "pipe:1"].map(str::to_string));
        }
    }
    args
}

/// FFmpeg muxer for the encoded elementary stream.
///
/// The raw `h263` muxer refuses the H.263+ encoder, so `H263Plus` goes
/// through the AVI muxer and the worker extracts the elementary stream from
/// the RIFF container afterwards.
fn ffmpeg_muxer(codec: VideoCodec) -> &'static str {
    if codec.needs_avi_container() {
        "avi"
    } else {
        codec.ffmpeg_name()
    }
}

/// Extracts the H.263+ elementary stream from an AVI (RIFF) container.
///
/// FFmpeg's AVI muxer writes each video frame as a `00dc` chunk inside the
/// `movi` list; the payload of those chunks *is* the H.263+ bitstream. The
/// raw `h263` muxer cannot carry the `h263p` encoder, so the worker wraps
/// the encode into AVI and unwraps it here. The H.261/H.263 encode path is
/// EOF-only anyway, so a single pass over the finished buffer is enough.
fn extract_avi_video_es(avi: &[u8]) -> Result<Vec<u8>, WorkerError> {
    /// Finds a chunk (or a LIST with the wanted list type) in a RIFF tree.
    ///
    /// For a LIST the **payload without the list type** is returned, so the
    /// caller can walk the chunks inside it directly.
    fn find_chunk<'a>(data: &'a [u8], id: &[u8; 4]) -> Option<&'a [u8]> {
        let mut offset = 0usize;
        while offset + 8 <= data.len() {
            let chunk_id = &data[offset..offset + 4];
            let size =
                u32::from_le_bytes([data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]]) as usize;
            let start = offset + 8;
            let end = start.saturating_add(size).min(data.len());
            if chunk_id == id {
                return Some(&data[start..end]);
            }
            if chunk_id == b"LIST" || chunk_id == b"RIFF" {
                if start + 4 > end {
                    break;
                }
                let payload = &data[start + 4..end];
                if chunk_id == b"LIST" && &data[start..start + 4] == id {
                    return Some(payload);
                }
                if let Some(found) = find_chunk(payload, id) {
                    return Some(found);
                }
            }
            offset = end + (size & 1);
        }
        None
    }

    let movi = find_chunk(avi, b"movi").ok_or_else(|| WorkerError::Codec("AVI container has no movi list".into()))?;
    // The `movi` list holds one `00dc` chunk per video frame; the chunk
    // payload *is* the H.263+ elementary stream.
    let mut es = Vec::new();
    let mut offset = 0usize;
    while offset + 8 <= movi.len() {
        let chunk_id = &movi[offset..offset + 4];
        let size =
            u32::from_le_bytes([movi[offset + 4], movi[offset + 5], movi[offset + 6], movi[offset + 7]]) as usize;
        let start = offset + 8;
        let end = start.saturating_add(size).min(movi.len());
        if chunk_id.starts_with(b"0") && chunk_id.ends_with(b"dc") {
            es.extend_from_slice(&movi[start..end]);
        }
        offset = end + (size & 1);
    }
    if es.is_empty() {
        return Err(WorkerError::Codec("AVI container holds no video frames".into()));
    }
    Ok(es)
}

/// Extracts one Annex-B NAL unit from `buffer`, keeping the next start code.
fn take_annexb_nal(buffer: &mut Vec<u8>, eof: bool) -> Option<Vec<u8>> {
    loop {
        let Some(start) = find_start_code(buffer, 0) else {
            if eof {
                buffer.clear();
            }
            return None;
        };
        // Keep the leading zero of a 4-byte start code (`00 00 00 01`):
        // `find_start_code` reports it at index 1 because it matches the
        // 3-byte sub-code first.
        if start > 1 {
            buffer.drain(..start);
        }
        let code_len = if buffer.len() >= 4 && buffer[..4] == [0, 0, 0, 1] { 4 } else { 3 };
        match find_start_code(buffer, code_len) {
            Some(3) => {
                // Empty NAL: drop the current start code and look at the next.
                buffer.drain(..3);
            }
            Some(next) => {
                // A 4-byte start code `00 00 00 01` is found at the 3-byte
                // sub-code; move the split one byte left so the previous NAL
                // does not keep the leading zero of the next prefix.
                let split = if next >= 1 && buffer[next - 1] == 0 { next - 1 } else { next };
                return Some(buffer.drain(..split).collect());
            }
            None if eof && buffer.len() > 4 => return Some(std::mem::take(buffer)),
            None if eof => {
                buffer.clear();
                return None;
            }
            None => return None,
        }
    }
}

fn find_start_code(buffer: &[u8], from: usize) -> Option<usize> {
    if buffer.len() < from + 3 {
        return None;
    }
    buffer[from..].windows(3).position(|window| window == [0, 0, 1]).map(|index| index + from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ffmpeg_bin() -> std::path::PathBuf {
        std::env::var_os("VKS_FFMPEG_BIN").map_or_else(|| std::path::PathBuf::from("ffmpeg"), std::path::PathBuf::from)
    }

    fn ffmpeg_available() -> bool {
        std::process::Command::new(ffmpeg_bin()).arg("-version").output().is_ok_and(|out| out.status.success())
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
        let caps = FfmpegCapabilities::probe(&ffmpeg_bin()).await.expect("probe");
        assert!(caps.version.contains("ffmpeg"), "{}", caps.version);
        assert!(caps.has_audio_decoder(AudioCodec::Aac));
        assert!(caps.has_audio_decoder(AudioCodec::G729));
        assert!(caps.has_audio_decoder(AudioCodec::AacLatm), "LATM uses the aac_latm decoder");
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
        decoder.write_encoded(&first).await.expect("write adts header frame");
        decoder.write_encoded(&encoded).await.expect("write adts tail");
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
        decoder.write_encoded(&first).await.expect("latm");
        decoder.write_encoded(&second).await.expect("latm");
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
            let slot =
                SlotConfig { bitrate_bps: 200_000, width, height, fps: 10, audio_bitrate_bps: 64_000, max_queue: 4 };
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
            decoder.write_encoded(&bitstream).await.expect("write bitstream");
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
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let pool = WorkerPool::new(1);
        let first = pool
            .spawn(WorkerMode::VideoDecode(VideoCodec::H261), slot, WorkerConfig::default())
            .await
            .expect("first worker");
        assert_eq!(pool.available(), 0);
        let second = pool.try_spawn(WorkerMode::VideoDecode(VideoCodec::H261), slot, WorkerConfig::default()).await;
        assert!(matches!(second, Err(WorkerError::PoolBusy)), "pool must refuse, not queue");
        drop(first);
        assert_eq!(pool.available(), 1, "dropping a worker must return its permit");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn child_env_is_scrubbed_and_restart_changes_pid() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
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
    async fn h263_encoder_roundtrips_704x576() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 800_000,
            width: 704,
            height: 576,
            fps: 5,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut encoder = FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H263), slot, WorkerConfig::default())
            .await
            .expect("h263 encoder");
        encoder.write_video(&vec![32u8; 704 * 576 * 3 / 2]).await.expect("frame");
        encoder.finish_input().await;
        let bitstream = encoder.read_to_end(4 * 1024 * 1024).await.expect("bitstream");
        assert!(!bitstream.is_empty());
        let mut decoder = FfmpegWorker::spawn(WorkerMode::VideoDecode(VideoCodec::H263), slot, WorkerConfig::default())
            .await
            .expect("h263 decoder");
        decoder.write_encoded(&bitstream).await.expect("bitstream in");
        decoder.finish_input().await;
        let pixels = decoder.read_to_end(4 * 1024 * 1024).await.expect("pixels");
        assert!(pixels.len() >= 704 * 576 * 3 / 2);
    }

    #[tokio::test]
    async fn h263_plus_encodes_320x240_which_base_h263_rejects() {
        if !ffmpeg_available() {
            return;
        }
        // Базовый H.263 кодировщик FFmpeg 320×240 **не принимает**
        // («Try H.263+»); H.263+ (`h263p`) — единственный путь к этому формату.
        let slot = SlotConfig {
            bitrate_bps: 400_000,
            width: 320,
            height: 240,
            fps: 5,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut encoder =
            FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H263Plus), slot, WorkerConfig::default())
                .await
                .expect("h263p encoder");
        encoder.write_video(&vec![32u8; 320 * 240 * 3 / 2]).await.expect("frame");
        encoder.finish_input().await;
        let bitstream = encoder.read_to_end(4 * 1024 * 1024).await.expect("bitstream");
        assert!(!bitstream.is_empty(), "h263p must produce a bitstream at 320x240");

        let mut decoder =
            FfmpegWorker::spawn(WorkerMode::VideoDecode(VideoCodec::H263Plus), slot, WorkerConfig::default())
                .await
                .expect("h263p decoder");
        decoder.write_encoded(&bitstream).await.expect("bitstream in");
        decoder.finish_input().await;
        let pixels = decoder.read_to_end(4 * 1024 * 1024).await.expect("pixels");
        assert!(pixels.len() >= 320 * 240 * 3 / 2);
    }

    #[tokio::test]
    async fn all_wrong_direction_calls_fail_fast() {
        if !ffmpeg_available() {
            return;
        }
        let video = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let audio = SlotConfig { audio_bitrate_bps: 48_000, ..video };
        let mut video_encoder =
            FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H261), video, WorkerConfig::default())
                .await
                .expect("video encoder");
        assert!(matches!(video_encoder.read_video().await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(video_encoder.write_encoded(&[0]).await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(video_encoder.write_audio(&[0i16; 960]).await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(video_encoder.read_audio_samples(960).await, Err(WorkerError::InvalidConfig(_))));

        let mut video_decoder =
            FfmpegWorker::spawn(WorkerMode::VideoDecode(VideoCodec::H261), video, WorkerConfig::default())
                .await
                .expect("video decoder");
        assert!(matches!(video_decoder.write_video(&[0]).await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(video_decoder.read_encoded_audio().await, Err(WorkerError::InvalidConfig(_))));

        let mut audio_encoder =
            FfmpegWorker::spawn(WorkerMode::AudioEncode(AudioCodec::Aac), audio, WorkerConfig::default())
                .await
                .expect("audio encoder");
        assert!(matches!(audio_encoder.read_video().await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(audio_encoder.read_encoded_video().await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(audio_encoder.write_video(&[0]).await, Err(WorkerError::InvalidConfig(_))));

        let mut audio_decoder =
            FfmpegWorker::spawn(WorkerMode::AudioDecode(AudioCodec::Aac), audio, WorkerConfig::default())
                .await
                .expect("audio decoder");
        assert!(matches!(audio_decoder.write_video(&[0]).await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(audio_decoder.read_video().await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(audio_decoder.write_audio(&[0i16; 960]).await, Err(WorkerError::InvalidConfig(_))));
    }

    #[test]
    fn annexb_split_handles_mixed_start_codes() {
        let mut buffer = vec![0, 0, 1, 0x42, 0, 0, 0, 1, 0x44, 0x55];
        let first = take_annexb_nal(&mut buffer, false).expect("first nal");
        assert_eq!(first, vec![0, 0, 1, 0x42], "4-byte prefix leaked into previous NAL");
        assert_eq!(buffer, vec![0, 0, 0, 1, 0x44, 0x55]);
        assert!(take_annexb_nal(&mut buffer, false).is_none());
        let last = take_annexb_nal(&mut buffer, true).expect("last nal at EOF");
        assert_eq!(last, vec![0, 0, 0, 1, 0x44, 0x55]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn annexb_split_preserves_leading_four_byte_prefix_and_skips_empty_nal() {
        let mut leading = vec![0, 0, 0, 1, 0x42, 0, 0, 1, 0x44];
        let first = take_annexb_nal(&mut leading, false).expect("first");
        assert_eq!(first, vec![0, 0, 0, 1, 0x42]);
        assert_eq!(leading, vec![0, 0, 1, 0x44]);

        let mut empty = vec![0, 0, 1, 0, 0, 1, 0x42, 0, 0, 1, 0x44];
        let first = take_annexb_nal(&mut empty, false).expect("non-empty after empty NAL");
        assert_eq!(first, vec![0, 0, 1, 0x42]);
        assert_eq!(empty, vec![0, 0, 1, 0x44]);
    }

    #[tokio::test]
    async fn h261_has_no_annexb_stream_and_rejects_nal_read() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut encoder = FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H261), slot, WorkerConfig::default())
            .await
            .expect("h261 encoder");
        assert!(matches!(encoder.read_encoded_video().await, Err(WorkerError::InvalidConfig(_))));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn hung_probe_times_out_and_kills_child() {
        let dir = std::env::temp_dir();
        let pidfile = dir.join(format!("vks-hang-ffmpeg-{}.pid", std::process::id()));
        // Сценарий передаётся аргументом `sh -c`, а не отдельным исполняемым
        // файлом: запись файла и его exec в одном процессе дают ETXTBSY
        // («Text file busy»), если параллельный поток успел отfork'иться
        // ребёнка, унаследовавшего открытый на запись дескриптор.
        let script = format!("sleep 30 & echo \"$$ $!\" > {} ; wait", pidfile.display());
        let started = Instant::now();
        let mut command = command(std::path::Path::new("/bin/sh"));
        command.arg("-c").arg(&script);
        let result = run_bounded(command).await;
        assert!(matches!(result, Err(WorkerError::Timeout(_))), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(8));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pids = std::fs::read_to_string(&pidfile).expect("fake ffmpeg recorded its pids");
        for pid in pids.split_whitespace() {
            let alive = std::process::Command::new("kill")
                .arg("-0")
                .arg(pid)
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            assert!(!alive, "timed-out probe left pid {pid} alive (process group not killed)");
        }
        let _ = std::fs::remove_file(&pidfile);
    }

    #[tokio::test]
    async fn h265_encoder_emits_annexb_nal_before_eof() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 400_000,
            width: 176,
            height: 144,
            fps: 5,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut encoder = FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H265), slot, WorkerConfig::default())
            .await
            .expect("h265 encoder");
        let frame = vec![16u8; 176 * 144 * 3 / 2];
        encoder.write_video(&frame).await.expect("frame 1");
        encoder.write_video(&frame).await.expect("frame 2");
        let nal = tokio::time::timeout(Duration::from_secs(5), encoder.read_encoded_video())
            .await
            .expect("nal before EOF")
            .expect("read error")
            .expect("nal");
        assert!(nal.len() > 4);
        assert!(nal.starts_with(&[0, 0, 1]) || nal.starts_with(&[0, 0, 0, 1]), "not Annex-B: {nal:?}");
        encoder.finish_input().await;
        let mut rest = 0usize;
        while let Some(nal) = encoder.read_encoded_video().await.expect("drain") {
            assert!(nal.starts_with(&[0, 0, 1]) || nal.starts_with(&[0, 0, 0, 1]));
            rest += 1;
            if rest > 10_000 {
                panic!("NAL loop did not terminate");
            }
        }
    }

    #[tokio::test]
    async fn wrong_mode_calls_fail_fast_without_touching_the_pipe() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut encoder = FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H261), slot, WorkerConfig::default())
            .await
            .expect("encoder");
        let started = Instant::now();
        assert!(matches!(encoder.read_video().await, Err(WorkerError::InvalidConfig(_))));
        assert!(matches!(encoder.write_encoded(b"x").await, Err(WorkerError::InvalidConfig(_))));
        assert!(started.elapsed() < Duration::from_millis(200), "mode guards must not block");
        assert!(!encoder.is_poisoned());
    }

    #[tokio::test]
    async fn read_timeout_poisons_worker_and_restart_recovers() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut worker = FfmpegWorker::spawn(
            WorkerMode::VideoDecode(VideoCodec::H261),
            slot,
            WorkerConfig { io_timeout: Duration::from_millis(150), ..WorkerConfig::default() },
        )
        .await
        .expect("worker");
        worker.write_encoded(&[0u8; 32]).await.expect("partial input");
        let error = worker.read_video().await.unwrap_err();
        assert!(matches!(error, WorkerError::Timeout(_)), "{error:?}");
        assert!(worker.is_poisoned(), "partial read must poison the pipe");
        worker.restart().await.expect("restart");
        assert!(!worker.is_poisoned());
        assert!(worker.is_running());
    }

    #[tokio::test]
    async fn g729_decoder_produces_pcm_without_encoder() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut decoder = FfmpegWorker::spawn(WorkerMode::AudioDecode(AudioCodec::G729), slot, WorkerConfig::default())
            .await
            .expect("g729 decoder");
        // FFmpeg's raw demuxer needs a couple of KiB before it emits frames.
        decoder.write_encoded(&vec![0u8; 1600]).await.expect("g729 frames");
        let pcm = decoder.read_audio_samples(960).await.expect("decoded pcm");
        assert_eq!(pcm.len(), 960);
        assert!(!decoder.is_poisoned());
    }

    #[tokio::test]
    async fn malformed_payload_fails_closed_and_restarts() {
        if !ffmpeg_available() {
            return;
        }
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
        let mut worker = FfmpegWorker::spawn(
            WorkerMode::VideoDecode(VideoCodec::H261),
            slot,
            WorkerConfig { io_timeout: Duration::from_millis(400), ..WorkerConfig::default() },
        )
        .await
        .expect("worker");
        let started = Instant::now();
        worker.write_encoded(&[0xff; 64]).await.expect("write junk");
        let result = worker.read_video().await;
        assert!(result.is_err(), "garbage must not yield a frame");
        assert!(started.elapsed() < Duration::from_secs(3), "garbage handling must not hang");
        worker.restart().await.expect("restart");
        assert!(worker.is_running());
    }

    #[tokio::test]
    async fn video_frame_size_validation_is_exact() {
        let slot = SlotConfig {
            bitrate_bps: 200_000,
            width: 176,
            height: 144,
            fps: 10,
            audio_bitrate_bps: 64_000,
            max_queue: 4,
        };
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

    #[test]
    fn parses_muxer_and_demuxer_lists_without_legend_rows() {
        let text = "File formats:\n  D. = Demuxing supported\n  .E = Muxing supported\n --\n  E  latm             MPEG-4 LATM AAC\n  D  loas             LOAS/LATM\n  E  adts             ADTS AAC\n";
        let formats = parse_format_list(text);
        assert!(formats.contains("latm"));
        assert!(formats.contains("loas"));
        assert!(formats.contains("adts"));
        assert!(!formats.contains("="), "{formats:?}");
        assert!(!formats.contains("supported"));
    }

    #[tokio::test]
    async fn probe_reports_latm_muxer_and_loas_demuxer() {
        if !ffmpeg_available() {
            return;
        }
        let capabilities = FfmpegCapabilities::probe(std::path::Path::new(&ffmpeg_bin())).await.expect("probe");
        assert!(capabilities.has_muxer("latm"), "FFmpeg must expose the latm muxer");
        assert!(capabilities.has_demuxer("loas"), "FFmpeg must expose the loas demuxer");
        assert!(capabilities.has_audio_encoder(AudioCodec::Aac));
        assert!(capabilities.has_audio_decoder(AudioCodec::AacLatm));
        assert!(capabilities.has_audio_latm_bridge());
    }
}
