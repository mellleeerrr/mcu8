//! Unified FFmpeg command builder for real-time video/audio pipelines.
//!
//! Enforces security hardening (environment scrubbing, parent-death signals),
//! standard thread limits (`-threads 2 -filter_threads 2`), error logging,
//! and low-latency realtime streaming defaults (`-flush_packets 1`).

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use crate::process::{safe_command, safe_ffmpeg_command};

/// Builder for constructing hardened, real-time FFmpeg commands.
#[derive(Debug)]
pub struct FfmpegCmdBuilder {
    command: Command,
}

impl Default for FfmpegCmdBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FfmpegCmdBuilder {
    /// Creates a builder using the default system or env-configured FFmpeg binary.
    #[must_use]
    pub fn new() -> Self {
        let mut command = safe_ffmpeg_command();
        init_base_args(&mut command);
        Self { command }
    }

    /// Creates a builder targeting a specific FFmpeg executable path.
    #[must_use]
    pub fn with_bin(bin: &Path) -> Self {
        let mut command = safe_command(bin);
        init_base_args(&mut command);
        Self { command }
    }

    /// Toggles `-flags low_delay`.
    ///
    /// Enabled for live video RTP decoders; disabled for audio and legacy video decoders
    /// (such as raw H.261) where low_delay causes frame dropping.
    #[must_use]
    pub fn low_delay(mut self, enabled: bool) -> Self {
        if enabled {
            self.command.args(["-flags", "low_delay"]);
        }
        self
    }

    /// Realtime pipe probe reduction (`-probesize 32 -analyzeduration 0`).
    ///
    /// Essential for real-time streaming to prevent FFmpeg from buffering seconds of input.
    #[must_use]
    pub fn rt_probe_limits(mut self) -> Self {
        self.command.args(["-probesize", "32", "-analyzeduration", "0"]);
        self
    }

    /// Configures raw YUV420p video input from `pipe:0`.
    #[must_use]
    pub fn raw_video_input(mut self, width: u32, height: u32, fps: u32) -> Self {
        let size = format!("{width}x{height}");
        let fps_str = fps.to_string();
        self.command.args([
            "-f",
            "rawvideo",
            "-pixel_format",
            "yuv420p",
            "-video_size",
            &size,
            "-framerate",
            &fps_str,
            "-i",
            "pipe:0",
            "-an",
        ]);
        self
    }

    /// Adds a scale video filter (`-vf scale=W:H`).
    #[must_use]
    pub fn scale(mut self, width: u32, height: u32) -> Self {
        let vf = format!("scale={width}:{height}");
        self.command.args(["-vf", &vf]);
        self
    }

    /// Selects video codec (`-c:v {codec}`).
    #[must_use]
    pub fn video_codec(mut self, codec: &str) -> Self {
        self.command.args(["-c:v", codec]);
        self
    }

    /// Sets realtime video encoder presets (`-preset {preset} -tune {tune} -pix_fmt yuv420p`).
    #[must_use]
    pub fn realtime_video_preset(mut self, preset: &str, tune: &str) -> Self {
        self.command.args(["-preset", preset, "-tune", tune, "-pix_fmt", "yuv420p"]);
        self
    }

    /// Configures video bitrate in kbps (`-b:v {bitrate}k -maxrate {maxrate}k -bufsize {bufsize}k`).
    #[must_use]
    pub fn video_bitrate_kbps(mut self, bitrate_kbps: u32, maxrate_kbps: u32, bufsize_kbps: u32) -> Self {
        let b = format!("{bitrate_kbps}k");
        let max = format!("{maxrate_kbps}k");
        let buf = format!("{bufsize_kbps}k");
        self.command.args(["-b:v", &b, "-maxrate", &max, "-bufsize", &buf]);
        self
    }

    /// Configures video bitrate in bits per second (`-b:v {bitrate_bps}`).
    #[must_use]
    pub fn video_bitrate_bps(mut self, bitrate_bps: u32) -> Self {
        let b = bitrate_bps.to_string();
        self.command.args(["-b:v", &b]);
        self
    }

    /// Sets GOP size in frames (`-g {keyint}`).
    #[must_use]
    pub fn gop(mut self, keyint: u32) -> Self {
        let g = keyint.to_string();
        self.command.args(["-g", &g]);
        self
    }

    /// Sets minimum keyframe interval in frames (`-keyint_min {keyint_min}`).
    #[must_use]
    pub fn keyint_min(mut self, keyint_min: u32) -> Self {
        let k = keyint_min.to_string();
        self.command.args(["-keyint_min", &k]);
        self
    }

    /// Configures raw YUV420p video output to `pipe:1` with passthrough fps and flush.
    #[must_use]
    pub fn raw_video_output(mut self) -> Self {
        self.command.args([
            "-fps_mode",
            "passthrough",
            "-flush_packets",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ]);
        self
    }

    /// Configures raw PCM S16LE audio input from `pipe:0`.
    #[must_use]
    pub fn raw_audio_input(mut self, sample_rate: u32, channels: u8) -> Self {
        let rate_str = sample_rate.to_string();
        let chan_str = channels.to_string();
        self.command.args([
            "-f",
            "s16le",
            "-ar",
            &rate_str,
            "-ac",
            &chan_str,
            "-i",
            "pipe:0",
        ]);
        self
    }

    /// Selects audio codec (`-c:a {codec}`).
    #[must_use]
    pub fn audio_codec(mut self, codec: &str) -> Self {
        self.command.args(["-c:a", codec]);
        self
    }

    /// Configures audio bitrate in bits per second (`-b:a {bitrate_bps}`).
    #[must_use]
    pub fn audio_bitrate_bps(mut self, bitrate_bps: u32) -> Self {
        let b = bitrate_bps.to_string();
        self.command.args(["-b:a", &b]);
        self
    }

    /// Configures raw PCM audio output to `pipe:1` with packet flushing.
    #[must_use]
    pub fn raw_audio_output(mut self, sample_rate: u32, channels: u8) -> Self {
        let rate_str = sample_rate.to_string();
        let chan_str = channels.to_string();
        self.command.args([
            "-flush_packets",
            "1",
            "-f",
            "s16le",
            "-acodec",
            "pcm_s16le",
            "-ar",
            &rate_str,
            "-ac",
            &chan_str,
            "pipe:1",
        ]);
        self
    }

    /// Configures elementary stream / demuxed input from `pipe:0`.
    #[must_use]
    pub fn input_pipe(mut self, format: &str) -> Self {
        self.command.args(["-f", format, "-i", "pipe:0"]);
        self
    }

    /// Configures multiplexed / elementary stream output to `pipe:1` with packet flushing.
    #[must_use]
    pub fn output_pipe(mut self, format: &str) -> Self {
        self.command.args(["-flush_packets", "1", "-f", format, "pipe:1"]);
        self
    }

    /// Appends an arbitrary argument.
    #[must_use]
    pub fn arg<S: AsRef<OsStr>>(mut self, arg: S) -> Self {
        self.command.arg(arg);
        self
    }

    /// Appends arbitrary arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    /// Configures piped stdin/stdout and null stderr.
    #[must_use]
    pub fn piped_io(mut self) -> Self {
        self.command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        self
    }

    /// Configures piped stdin/stdout/stderr for error logging.
    #[must_use]
    pub fn piped_all_io(mut self) -> Self {
        self.command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self
    }

    /// Returns the configured [`Command`].
    #[must_use]
    pub fn build(self) -> Command {
        self.command
    }

    /// Returns a list of argument strings currently configured on the command.
    #[must_use]
    pub fn args_vec(&self) -> Vec<String> {
        self.command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect()
    }
}

fn init_base_args(command: &mut Command) {
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-threads",
        "2",
        "-filter_threads",
        "2",
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_contains_hardened_base_flags() {
        let builder = FfmpegCmdBuilder::new();
        let args = builder.args_vec();
        assert!(args.contains(&"-hide_banner".to_string()));
        assert!(args.contains(&"-loglevel".to_string()));
        assert!(args.contains(&"error".to_string()));
        assert!(args.contains(&"-nostdin".to_string()));
        assert!(args.contains(&"-threads".to_string()));
        assert!(args.contains(&"-filter_threads".to_string()));
    }

    #[test]
    fn builder_encoder_args_match_realtime_contract() {
        let builder = FfmpegCmdBuilder::new()
            .raw_video_input(1280, 720, 30)
            .scale(640, 360)
            .video_codec("libx264")
            .realtime_video_preset("ultrafast", "zerolatency")
            .args(["-profile:v", "baseline", "-level:v", "3.1"])
            .video_bitrate_kbps(1500, 1725, 750)
            .gop(60)
            .keyint_min(15)
            .args(["-x264-params", "slice-max-size=1200"])
            .output_pipe("h264");

        let args = builder.args_vec();
        assert!(args.contains(&"scale=640:360".to_string()));
        assert!(args.contains(&"-preset".to_string()));
        assert!(args.contains(&"ultrafast".to_string()));
        assert!(args.contains(&"-tune".to_string()));
        assert!(args.contains(&"zerolatency".to_string()));
        assert!(args.contains(&"-flush_packets".to_string()));
        assert!(args.contains(&"1".to_string()));
        assert!(args.contains(&"pipe:1".to_string()));
        assert!(!args.contains(&"-flags".to_string()));

        // Ensure -threads 2 is present exactly once, not duplicated
        let threads_count = args.windows(2).filter(|w| w[0] == "-threads" && w[1] == "2").count();
        assert_eq!(threads_count, 1, "-threads 2 must appear exactly once");
    }

    #[test]
    fn builder_decoder_args_honor_low_delay() {
        let builder = FfmpegCmdBuilder::new()
            .low_delay(true)
            .rt_probe_limits()
            .input_pipe("h264")
            .scale(1280, 720)
            .raw_video_output();

        let args = builder.args_vec();
        assert!(args.contains(&"-flags".to_string()));
        assert!(args.contains(&"low_delay".to_string()));
        assert!(args.contains(&"-probesize".to_string()));
        assert!(args.contains(&"32".to_string()));
        assert!(args.contains(&"-fps_mode".to_string()));
        assert!(args.contains(&"passthrough".to_string()));
    }

    #[test]
    fn builder_audio_args_include_flush_packets() {
        let builder = FfmpegCmdBuilder::new()
            .raw_audio_input(48000, 1)
            .audio_codec("aac")
            .audio_bitrate_bps(64000)
            .raw_audio_output(48000, 1);

        let args = builder.args_vec();
        assert!(args.contains(&"-flush_packets".to_string()));
        assert!(args.contains(&"pcm_s16le".to_string()));
    }
}
