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

/// Video codec identifier for FFmpeg encoding commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoEncoderCodec {
    H264,
    H265,
}

impl From<bool> for VideoEncoderCodec {
    fn from(is_h265: bool) -> Self {
        if is_h265 {
            Self::H265
        } else {
            Self::H264
        }
    }
}

/// Hardware acceleration backend for FFmpeg video pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HwAccel {
    /// Automatic detection based on host environment/devices.
    #[default]
    Auto,
    /// NVIDIA NVENC hardware acceleration (`h264_nvenc`, `hevc_nvenc`).
    Nvenc,
    /// Intel/AMD Video Acceleration API (`h264_vaapi`, `hevc_vaapi`).
    Vaapi,
    /// Intel Quick Sync Video (`h264_qsv`, `hevc_qsv`).
    QuickSync,
    /// Pure software encoding (`libx264`, `libx265`).
    Software,
}

impl HwAccel {
    /// Alias for [`HwAccel::Software`] (no hardware acceleration) in idiomatic constant case.
    pub const NONE: Self = Self::Software;

    /// Alias for [`HwAccel::Software`] (no hardware acceleration).
    #[allow(non_upper_case_globals)]
    pub const None: Self = Self::Software;

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Nvenc => "nvenc",
            Self::Vaapi => "vaapi",
            Self::QuickSync => "qsv",
            Self::Software => "software",
        }
    }

    #[must_use]
    pub const fn is_hardware(self) -> bool {
        matches!(self, Self::Nvenc | Self::Vaapi | Self::QuickSync)
    }

    /// Case-insensitive string parser with alias support.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("auto") {
            Some(Self::Auto)
        } else if s.eq_ignore_ascii_case("nvenc") || s.eq_ignore_ascii_case("cuda") || s.eq_ignore_ascii_case("nvidia")
        {
            Some(Self::Nvenc)
        } else if s.eq_ignore_ascii_case("vaapi") || s.eq_ignore_ascii_case("va") {
            Some(Self::Vaapi)
        } else if s.eq_ignore_ascii_case("qsv") || s.eq_ignore_ascii_case("quicksync") {
            Some(Self::QuickSync)
        } else if s.eq_ignore_ascii_case("software")
            || s.eq_ignore_ascii_case("sw")
            || s.eq_ignore_ascii_case("none")
            || s.eq_ignore_ascii_case("off")
            || s.eq_ignore_ascii_case("0")
        {
            Some(Self::Software)
        } else {
            None
        }
    }

    /// Reads `VKS_HWACCEL` env variable with fallback to `Auto`.
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var("VKS_HWACCEL").ok().and_then(|v| Self::parse(&v)).unwrap_or(Self::Auto)
    }

    /// Resolves `Auto` into a concrete backend by checking host device nodes (<1 µs, single stat).
    #[must_use]
    pub fn resolve(self) -> Self {
        match self {
            Self::Auto => {
                if Path::new("/dev/nvidia0").exists() || Path::new("/dev/nvidiactl").exists() {
                    Self::Nvenc
                } else if Path::new("/dev/dri/renderD128").exists() {
                    Self::Vaapi
                } else {
                    Self::Software
                }
            }
            explicit => explicit,
        }
    }

    /// Returns the FFmpeg video encoder name for the given codec.
    #[must_use]
    pub const fn encoder_name(self, codec: VideoEncoderCodec) -> &'static str {
        match (self, codec) {
            (Self::Nvenc, VideoEncoderCodec::H264) => "h264_nvenc",
            (Self::Nvenc, VideoEncoderCodec::H265) => "hevc_nvenc",
            (Self::Vaapi, VideoEncoderCodec::H264) => "h264_vaapi",
            (Self::Vaapi, VideoEncoderCodec::H265) => "hevc_vaapi",
            (Self::QuickSync, VideoEncoderCodec::H264) => "h264_qsv",
            (Self::QuickSync, VideoEncoderCodec::H265) => "hevc_qsv",
            (Self::Software | Self::Auto, VideoEncoderCodec::H264) => "libx264",
            (Self::Software | Self::Auto, VideoEncoderCodec::H265) => "libx265",
        }
    }
}

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

    /// Selects video encoder based on hardware acceleration backend and codec.
    #[must_use]
    pub fn video_encoder(self, hw: HwAccel, codec: VideoEncoderCodec) -> Self {
        self.video_codec(hw.encoder_name(codec))
    }

    /// Configures realtime presets and low-latency flags tailored to the selected backend.
    #[must_use]
    pub fn realtime_video_encoder_preset(mut self, hw: HwAccel, _codec: VideoEncoderCodec) -> Self {
        match hw {
            HwAccel::Software | HwAccel::Auto => {
                self.command.args(["-preset", "ultrafast", "-tune", "zerolatency", "-pix_fmt", "yuv420p"]);
            }
            HwAccel::Nvenc => {
                self.command.args([
                    "-preset",
                    "p1",
                    "-tune",
                    "ull",
                    "-zerolatency",
                    "1",
                    "-forced-idr",
                    "1",
                    "-aud",
                    "1",
                    "-pix_fmt",
                    "yuv420p",
                ]);
            }
            HwAccel::Vaapi => {
                self.command.args(["-low_power", "1", "-aud", "1"]);
            }
            HwAccel::QuickSync => {
                self.command.args(["-preset", "veryfast", "-forced_idr", "1", "-low_delay_brc", "1", "-aud", "1"]);
            }
        }
        self
    }

    /// Sets H.264 profile and level, adapting `baseline` to `constrained_baseline` for VAAPI.
    #[must_use]
    pub fn video_encoder_profile(mut self, hw: HwAccel, codec: VideoEncoderCodec, profile: &str, level: &str) -> Self {
        if codec == VideoEncoderCodec::H264 && !profile.is_empty() {
            let actual_profile = if hw == HwAccel::Vaapi && profile.eq_ignore_ascii_case("baseline") {
                "constrained_baseline"
            } else {
                profile
            };
            self.command.args(["-profile:v", actual_profile]);
            if !level.is_empty() {
                self.command.args(["-level:v", level]);
            }
        }
        self
    }

    /// Appends software encoder params (`-x264-params` / `-x265-params`), safely skipping
    /// them for hardware backends to prevent command line parsing failures.
    #[must_use]
    pub fn video_encoder_params(mut self, hw: HwAccel, codec: VideoEncoderCodec, params: &str) -> Self {
        if matches!(hw, HwAccel::Software | HwAccel::Auto) && !params.is_empty() {
            let flag = match codec {
                VideoEncoderCodec::H264 => "-x264-params",
                VideoEncoderCodec::H265 => "-x265-params",
            };
            self.command.args([flag, params]);
        }
        self
    }

    /// Specifies VA-API render device (e.g. `/dev/dri/renderD128`).
    #[must_use]
    pub fn vaapi_device(mut self, device: &str) -> Self {
        if !device.is_empty() {
            self.command.args(["-vaapi_device", device]);
        }
        self
    }

    /// High-level convenience method: configures the full real-time encoder pipeline.
    #[must_use]
    pub fn realtime_video_encoder(
        mut self,
        hw: HwAccel,
        codec: VideoEncoderCodec,
        profile_level: Option<(&str, &str)>,
        encoder_params: Option<&str>,
    ) -> Self {
        let hw = hw.resolve();
        self = self.video_encoder(hw, codec);
        self = self.realtime_video_encoder_preset(hw, codec);
        if let Some((profile, level)) = profile_level {
            self = self.video_encoder_profile(hw, codec, profile, level);
        }
        if let Some(params) = encoder_params {
            self = self.video_encoder_params(hw, codec, params);
        }
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
        self.command.args(["-f", "s16le", "-ar", &rate_str, "-ac", &chan_str, "-i", "pipe:0"]);
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
        self.command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        self
    }

    /// Configures piped stdin/stdout/stderr for error logging.
    #[must_use]
    pub fn piped_all_io(mut self) -> Self {
        self.command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self
    }

    /// Returns the configured [`Command`].
    #[must_use]
    pub fn build(self) -> Command {
        self.command
    }

    /// Spawns the configured command as a child process.
    pub fn spawn(mut self) -> Result<tokio::process::Child, std::io::Error> {
        self.command.spawn()
    }

    /// Returns a list of argument strings currently configured on the command.
    #[must_use]
    pub fn args_vec(&self) -> Vec<String> {
        self.command.as_std().get_args().map(|s| s.to_string_lossy().to_string()).collect()
    }
}

fn init_base_args(command: &mut Command) {
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "2", "-filter_threads", "2"]);
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

    #[test]
    fn hwaccel_parsing_and_aliases() {
        assert_eq!(HwAccel::parse("auto"), Some(HwAccel::Auto));
        assert_eq!(HwAccel::parse("NVENC"), Some(HwAccel::Nvenc));
        assert_eq!(HwAccel::parse("cuda"), Some(HwAccel::Nvenc));
        assert_eq!(HwAccel::parse("nvidia"), Some(HwAccel::Nvenc));
        assert_eq!(HwAccel::parse("vaapi"), Some(HwAccel::Vaapi));
        assert_eq!(HwAccel::parse("va"), Some(HwAccel::Vaapi));
        assert_eq!(HwAccel::parse("qsv"), Some(HwAccel::QuickSync));
        assert_eq!(HwAccel::parse("QuickSync"), Some(HwAccel::QuickSync));
        assert_eq!(HwAccel::parse("software"), Some(HwAccel::Software));
        assert_eq!(HwAccel::parse("none"), Some(HwAccel::Software));
        assert_eq!(HwAccel::parse("0"), Some(HwAccel::Software));
        assert_eq!(HwAccel::parse("unknown"), None);
        assert_eq!(HwAccel::None, HwAccel::Software);
        assert!(HwAccel::Nvenc.is_hardware());
        assert!(HwAccel::Vaapi.is_hardware());
        assert!(HwAccel::QuickSync.is_hardware());
        assert!(!HwAccel::Software.is_hardware());

        // Resolution idempotency for explicit variants
        assert_eq!(HwAccel::Nvenc.resolve(), HwAccel::Nvenc);
        assert_eq!(HwAccel::Vaapi.resolve(), HwAccel::Vaapi);
        assert_eq!(HwAccel::QuickSync.resolve(), HwAccel::QuickSync);
        assert_eq!(HwAccel::Software.resolve(), HwAccel::Software);

        // Codec conversion
        assert_eq!(VideoEncoderCodec::from(true), VideoEncoderCodec::H265);
        assert_eq!(VideoEncoderCodec::from(false), VideoEncoderCodec::H264);

        // Env parsing fallback
        assert_eq!(HwAccel::from_env(), HwAccel::Auto);
    }

    #[test]
    fn vaapi_device_configuration() {
        let builder = FfmpegCmdBuilder::new().vaapi_device("/dev/dri/renderD128");
        let args = builder.args_vec();
        assert!(args.windows(2).any(|w| w[0] == "-vaapi_device" && w[1] == "/dev/dri/renderD128"));

        // Empty device string is ignored
        let empty_builder = FfmpegCmdBuilder::new().vaapi_device("");
        assert!(!empty_builder.args_vec().contains(&"-vaapi_device".to_string()));
    }

    #[test]
    fn builder_software_h264_args() {
        let builder = FfmpegCmdBuilder::new().realtime_video_encoder(
            HwAccel::Software,
            VideoEncoderCodec::H264,
            Some(("baseline", "3.1")),
            Some("repeat-headers=1:aud=1"),
        );
        let args = builder.args_vec();

        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "libx264"));
        assert!(args.windows(2).any(|w| w[0] == "-preset" && w[1] == "ultrafast"));
        assert!(args.windows(2).any(|w| w[0] == "-tune" && w[1] == "zerolatency"));
        assert!(args.windows(2).any(|w| w[0] == "-profile:v" && w[1] == "baseline"));
        assert!(args.windows(2).any(|w| w[0] == "-level:v" && w[1] == "3.1"));
        assert!(args.windows(2).any(|w| w[0] == "-x264-params" && w[1] == "repeat-headers=1:aud=1"));
        assert!(!args.contains(&"-zerolatency".to_string()));
        assert!(!args.contains(&"-low_power".to_string()));
    }

    #[test]
    fn builder_software_h265_args() {
        let builder = FfmpegCmdBuilder::new().realtime_video_encoder(
            HwAccel::Software,
            VideoEncoderCodec::H265,
            None,
            Some("pools=1:aud=1"),
        );
        let args = builder.args_vec();

        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "libx265"));
        assert!(args.windows(2).any(|w| w[0] == "-preset" && w[1] == "ultrafast"));
        assert!(args.windows(2).any(|w| w[0] == "-x265-params" && w[1] == "pools=1:aud=1"));
        assert!(!args.contains(&"-x264-params".to_string()));
    }

    #[test]
    fn builder_nvenc_flags_purge_incompatible_options() {
        let builder = FfmpegCmdBuilder::new().realtime_video_encoder(
            HwAccel::Nvenc,
            VideoEncoderCodec::H264,
            Some(("baseline", "3.1")),
            Some("repeat-headers=1"),
        );
        let args = builder.args_vec();

        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "h264_nvenc"));
        assert!(args.windows(2).any(|w| w[0] == "-preset" && w[1] == "p1"));
        assert!(args.windows(2).any(|w| w[0] == "-tune" && w[1] == "ull"));
        assert!(args.windows(2).any(|w| w[0] == "-zerolatency" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-forced-idr" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-aud" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-profile:v" && w[1] == "baseline"));

        assert!(!args.contains(&"ultrafast".to_string()));
        assert!(!args.contains(&"zerolatency".to_string()));
        assert!(!args.contains(&"-x264-params".to_string()));
        assert!(!args.contains(&"-x265-params".to_string()));
    }

    #[test]
    fn builder_nvenc_hevc_flags() {
        let builder = FfmpegCmdBuilder::new().realtime_video_encoder(
            HwAccel::Nvenc,
            VideoEncoderCodec::H265,
            None,
            Some("ignored"),
        );
        let args = builder.args_vec();

        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "hevc_nvenc"));
        assert!(args.windows(2).any(|w| w[0] == "-preset" && w[1] == "p1"));
        assert!(args.windows(2).any(|w| w[0] == "-tune" && w[1] == "ull"));
        assert!(!args.contains(&"-x265-params".to_string()));
    }

    #[test]
    fn builder_vaapi_adapts_baseline_and_omits_presets() {
        let builder = FfmpegCmdBuilder::new().realtime_video_encoder(
            HwAccel::Vaapi,
            VideoEncoderCodec::H264,
            Some(("baseline", "3.1")),
            Some("repeat-headers=1"),
        );
        let args = builder.args_vec();

        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "h264_vaapi"));
        assert!(args.windows(2).any(|w| w[0] == "-low_power" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-aud" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-profile:v" && w[1] == "constrained_baseline"));

        assert!(!args.contains(&"-preset".to_string()));
        assert!(!args.contains(&"-tune".to_string()));
        assert!(!args.contains(&"-x264-params".to_string()));
    }

    #[test]
    fn builder_vaapi_hevc_flags() {
        let builder =
            FfmpegCmdBuilder::new().realtime_video_encoder(HwAccel::Vaapi, VideoEncoderCodec::H265, None, None);
        let args = builder.args_vec();

        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "hevc_vaapi"));
        assert!(args.windows(2).any(|w| w[0] == "-low_power" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-aud" && w[1] == "1"));
        assert!(!args.contains(&"-preset".to_string()));
    }

    #[test]
    fn builder_quicksync_uses_veryfast_and_low_delay() {
        let builder = FfmpegCmdBuilder::new().realtime_video_encoder(
            HwAccel::QuickSync,
            VideoEncoderCodec::H264,
            Some(("baseline", "3.1")),
            Some("ignored"),
        );
        let args = builder.args_vec();

        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "h264_qsv"));
        assert!(args.windows(2).any(|w| w[0] == "-preset" && w[1] == "veryfast"));
        assert!(args.windows(2).any(|w| w[0] == "-forced_idr" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-low_delay_brc" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-aud" && w[1] == "1"));
        assert!(args.windows(2).any(|w| w[0] == "-profile:v" && w[1] == "baseline"));

        assert!(!args.contains(&"ultrafast".to_string()));
        assert!(!args.contains(&"-tune".to_string()));
        assert!(!args.contains(&"-x264-params".to_string()));
    }
}
