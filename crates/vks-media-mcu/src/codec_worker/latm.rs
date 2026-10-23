//! Streaming MP4A-LATM encoder around a long-lived FFmpeg child.
//!
//! The bridge feeds 20 ms (960 sample) PCM blocks; FFmpeg's AAC encoder
//! consumes 1024 samples per frame, so 16 ticks produce 15 LOAS frames. The
//! child's stdin/stdout are driven by separate tasks: a 20 ms RTP loop must
//! never block on a partially written pipe. Output is split back into
//! `audioMuxElement` payloads (the 3-byte LOAS header is not carried in RTP).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::WorkerError;

/// PCM block size consumed by the MCU audio tick.
pub const LATM_TICK_SAMPLES: usize = 960;
/// AAC-LC samples per frame.
pub const LATM_FRAME_SAMPLES: usize = 1024;
/// Bound for queued PCM ticks and encoded frames; never grow without limit.
const QUEUE_TICKS: usize = 8;

/// Builds the FFmpeg argument vector for one LATM encoder.
///
/// `-smc-interval 1` repeats `StreamMuxConfig` in every frame: a decoder that
/// missed the first packet (or joined late) can start at any frame. The
/// forbidden flags from T06b-8 must not reappear (`-flags low_delay`,
/// `-fflags nobuffer`, `-avioflags direct` all break AAC framing).
#[must_use]
pub fn encoder_args(bitrate_bps: u32) -> Vec<String> {
    let bitrate = bitrate_bps.max(16_000);
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        // Without this the raw-PCM input probe buffers ~5 MB before the first
        // encoded frame reaches stdout, which is fatal for a live bridge.
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-f".into(),
        "s16le".into(),
        "-ar".into(),
        "48000".into(),
        "-ac".into(),
        "1".into(),
        "-i".into(),
        "pipe:0".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        bitrate.to_string(),
        "-smc-interval".into(),
        "1".into(),
        "-f".into(),
        "latm".into(),
        "pipe:1".into(),
    ]
}

/// A live LATM encoder and its two pipe tasks.
pub struct LatmEncoder {
    pcm_tx: Option<mpsc::Sender<Vec<i16>>>,
    frame_rx: mpsc::Receiver<Vec<u8>>,
    failed: Arc<AtomicBool>,
    child: Child,
    input_task: tokio::task::JoinHandle<()>,
    output_task: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
    stderr: Arc<std::sync::Mutex<String>>,
}

impl LatmEncoder {
    /// Spawns FFmpeg for a 48 kHz mono AAC-LATM stream.
    ///
    /// # Errors
    /// [`WorkerError::Spawn`] when the child cannot start or its pipes are
    /// missing (the binary was not probed).
    pub fn spawn(ffmpeg: PathBuf, bitrate_bps: u32) -> Result<Self, WorkerError> {
        let mut child = Command::new(&ffmpeg)
            .args(encoder_args(bitrate_bps))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(WorkerError::Spawn)?;
        let mut stdin = child.stdin.take().ok_or_else(|| WorkerError::InvalidConfig("latm encoder stdin".into()))?;
        let mut stdout = child.stdout.take().ok_or_else(|| WorkerError::InvalidConfig("latm encoder stdout".into()))?;
        let stderr_pipe =
            child.stderr.take().ok_or_else(|| WorkerError::InvalidConfig("latm encoder stderr".into()))?;
        let (pcm_tx, mut pcm_rx) = mpsc::channel::<Vec<i16>>(QUEUE_TICKS);
        let (frame_tx, frame_rx) = mpsc::channel::<Vec<u8>>(QUEUE_TICKS);
        let failed = Arc::new(AtomicBool::new(false));
        let stderr = Arc::new(std::sync::Mutex::new(String::new()));

        let input_failed = failed.clone();
        let input_task = tokio::spawn(async move {
            while let Some(block) = pcm_rx.recv().await {
                let mut bytes = Vec::with_capacity(block.len() * 2);
                for sample in block {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                if stdin.write_all(&bytes).await.is_err() {
                    input_failed.store(true, Ordering::Release);
                    return;
                }
            }
        });

        let output_failed = failed.clone();
        let output_task = tokio::spawn(async move {
            loop {
                let mut header = [0u8; 3];
                match stdout.read_exact(&mut header).await {
                    Ok(_) if header[0] == 0x56 && header[1] & 0xe0 == 0xe0 => {}
                    Ok(_) | Err(_) => break,
                }
                let length = (usize::from(header[1] & 0x1f) << 8) | usize::from(header[2]);
                if length == 0 || length > 0x1fff {
                    output_failed.store(true, Ordering::Release);
                    break;
                }
                let mut payload = vec![0u8; length];
                if stdout.read_exact(&mut payload).await.is_err() {
                    output_failed.store(true, Ordering::Release);
                    break;
                }
                if frame_tx.send(payload).await.is_err() {
                    break;
                }
            }
        });

        let stderr_text = stderr.clone();
        let stderr_task = tokio::spawn(async move {
            let mut pipe = stderr_pipe;
            let mut buffer = [0u8; 1024];
            while let Ok(count) = pipe.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                if let Ok(mut text) = stderr_text.lock() {
                    text.push_str(&String::from_utf8_lossy(&buffer[..count]));
                    if text.len() > 4096 {
                        let split = text.len() - 2048;
                        *text = text.split_off(split);
                    }
                }
            }
        });

        Ok(Self { pcm_tx: Some(pcm_tx), frame_rx, failed, child, input_task, output_task, stderr_task, stderr })
    }

    /// Queues one 20 ms block of 48 kHz PCM. Returns `false` when the worker
    /// or its bounded queue has gone away; callers drop the tick instead of
    /// blocking the RTP loop.
    pub async fn push_pcm(&self, pcm: &[i16]) -> bool {
        match &self.pcm_tx {
            Some(sender) => sender.send(pcm.to_vec()).await.is_ok(),
            None => false,
        }
    }

    /// Closes the PCM side so FFmpeg flushes its encoder delay. Call only on
    /// teardown: a live bridge must keep feeding ticks.
    pub fn close_input(&mut self) {
        self.pcm_tx.take();
    }

    /// Non-blocking receive of one `audioMuxElement` (LOAS header stripped).
    #[must_use]
    pub fn try_frame(&mut self) -> Option<Vec<u8>> {
        self.frame_rx.try_recv().ok()
    }

    /// Whether FFmpeg exited or a pipe failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Last FFmpeg stderr fragment (diagnostics only).
    #[must_use]
    pub fn last_stderr(&self) -> String {
        self.stderr.lock().map(|text| text.clone()).unwrap_or_default()
    }

    /// Child process id (diagnostics/tests).
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Stops the child and both pipe tasks.
    pub async fn shutdown(mut self) {
        self.input_task.abort();
        self.output_task.abort();
        self.stderr_task.abort();
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ffmpeg_bin() -> String {
        std::env::var("VKS_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into())
    }

    fn ffmpeg_available() -> bool {
        std::process::Command::new(ffmpeg_bin()).arg("-version").output().is_ok_and(|out| out.status.success())
    }

    #[test]
    fn encoder_args_repeat_smc_and_avoid_known_breaking_flags() {
        let args = encoder_args(64_000);
        let joined = args.join(" ");
        assert!(joined.contains("-smc-interval 1"), "{joined}");
        assert!(joined.contains("-f latm"), "{joined}");
        assert!(joined.contains("-probesize 32 -analyzeduration 0"), "live pipe must not probe: {joined}");
        for forbidden in ["low_delay", "nobuffer", "avioflags", "direct"] {
            assert!(!joined.contains(forbidden), "{forbidden} must not be used for AAC: {joined}");
        }
    }

    #[tokio::test]
    async fn sixteen_pcm_ticks_produce_fifteen_smc_frames() {
        if !ffmpeg_available() {
            return;
        }
        let mut encoder = LatmEncoder::spawn(PathBuf::from(ffmpeg_bin()), 64_000).expect("spawn");
        let mut phase = 0.0f32;
        for _ in 0..16 {
            let mut block = [0i16; LATM_TICK_SAMPLES];
            for sample in &mut block {
                *sample = (phase.sin() * 8_000.0) as i16;
                phase += 2.0 * std::f32::consts::PI * 440.0 / 48_000.0;
            }
            assert!(encoder.push_pcm(&block).await, "queue accepted the tick");
        }
        encoder.close_input();
        let mut frames = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if let Some(frame) = encoder.try_frame() {
                frames.push(frame);
            } else if encoder.failed() {
                break;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        assert!(
            (15..=17).contains(&frames.len()),
            "16 x 960 = 15 x 1024 (+ flush): got {}; stderr={}",
            frames.len(),
            encoder.last_stderr()
        );
        assert!(frames[0].first().is_some_and(|byte| byte & 0x80 == 0), "first frame carries SMC");
        assert!(frames.iter().all(|frame| frame.len() <= 1200), "payload fits MTU");
        encoder.shutdown().await;
    }
}
