//! Startup probe and host-wide budget for isolated codec workers.
//!
//! The transcoder is intentionally **not** advertised in SDP yet: this module
//! only records what the host FFmpeg can do and enforces a process cap. Codec
//! selection/packetization lands separately, so a failed probe degrades
//! cleanly to the native profile instead of advertising codecs the bridge
//! cannot carry.

use std::path::PathBuf;
use std::sync::OnceLock;
use vks_common::codec_capabilities::truthy;

use tracing::{info, warn};
use vks_media_mcu::{AudioCodec, FfmpegCapabilities, VideoCodec, WorkerPool};

static CAPABILITIES: OnceLock<Option<FfmpegCapabilities>> = OnceLock::new();
static POOL: OnceLock<WorkerPool> = OnceLock::new();

/// Probes FFmpeg once at startup and publishes the worker pool.
///
/// `VKS_FFMPEG_BIN` overrides the binary (default `ffmpeg`);
/// `VKS_CODEC_WORKER_POOL` caps concurrent children (default 8);
/// `VKS_TRANSCODER_REQUIRED=1` turns a failed probe into a startup error.
pub async fn init() -> Result<(), String> {
    let ffmpeg = std::env::var("VKS_FFMPEG_BIN").map_or_else(|_| PathBuf::from("ffmpeg"), PathBuf::from);
    let max_workers = std::env::var("VKS_CODEC_WORKER_POOL")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8);
    let required = truthy(std::env::var("VKS_TRANSCODER_REQUIRED").ok().as_deref());

    // One retry: the first FFmpeg exec during a cold systemd start can lose
    // the race against a saturated CPU even though the binary is fine.
    let capabilities = match match FfmpegCapabilities::probe(&ffmpeg).await {
        Err(_) => {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            FfmpegCapabilities::probe(&ffmpeg).await
        }
        first => first,
    } {
        Ok(capabilities) => {
            info!(
                binary = %ffmpeg.display(),
                version = %capabilities.version,
                aac_latm_decode = capabilities.has_audio_decoder(AudioCodec::AacLatm),
                g729_decode = capabilities.has_audio_decoder(AudioCodec::G729),
                h261 = capabilities.has_video_decoder(VideoCodec::H261),
                h263 = capabilities.has_video_decoder(VideoCodec::H263),
                h263p = capabilities.has_video_decoder(VideoCodec::H263Plus),
                h265 = capabilities.has_video_decoder(VideoCodec::H265),
                "codec worker capabilities probed (not advertised until bridge wiring lands)"
            );
            Some(capabilities)
        }
        Err(error) => {
            warn!(%error, "codec worker probe failed; native profile remains active");
            if required {
                return Err(format!("VKS_TRANSCODER_REQUIRED=1 but codec probe failed: {error}"));
            }
            None
        }
    };

    if let Some(capabilities) = &capabilities {
        publish_support(capabilities);
    }
    let _ = POOL.set(WorkerPool::with_capabilities(max_workers, capabilities.clone()));
    let _ = CAPABILITIES.set(capabilities);
    vks_common::metrics::CODEC_WORKER_POOL_SIZE.set(i64::try_from(max_workers).unwrap_or(i64::MAX));
    info!(max_workers, "codec worker pool initialized");
    Ok(())
}

fn publish_support(capabilities: &FfmpegCapabilities) {
    use vks_common::metrics::CODEC_WORKER_SUPPORTED;
    // H.264 is intentionally absent: it is probed by `gateways::video_bridge_availability`
    // (the SIP gateway bridge uses `ffmpeg -encoders/-decoders`), not by the
    // worker `VideoCodec` enum. The label set must stay a subset of `codec_spec`.
    for (codec, decode, encode) in [
        (
            "aac_latm",
            capabilities.has_audio_decoder(AudioCodec::AacLatm),
            capabilities.has_audio_encoder(AudioCodec::AacLatm),
        ),
        ("g729", capabilities.has_audio_decoder(AudioCodec::G729), false),
        ("h261", capabilities.has_video_decoder(VideoCodec::H261), capabilities.has_video_encoder(VideoCodec::H261)),
        ("h263", capabilities.has_video_decoder(VideoCodec::H263), capabilities.has_video_encoder(VideoCodec::H263)),
        (
            "h263p",
            capabilities.has_video_decoder(VideoCodec::H263Plus),
            capabilities.has_video_encoder(VideoCodec::H263Plus),
        ),
        ("h265", capabilities.has_video_decoder(VideoCodec::H265), capabilities.has_video_encoder(VideoCodec::H265)),
    ] {
        CODEC_WORKER_SUPPORTED.with_label_values(&[codec, "decode"]).set(i64::from(decode));
        CODEC_WORKER_SUPPORTED.with_label_values(&[codec, "encode"]).set(i64::from(encode));
    }
}

/// Probed capabilities, if FFmpeg was available at startup.
#[must_use]
pub fn capabilities() -> Option<&'static FfmpegCapabilities> {
    CAPABILITIES.get().and_then(Option::as_ref)
}

/// Host-wide worker pool, available only after a successful [`init`].
///
/// Returning `Option` is deliberate: a lazy default here would silently
/// ignore `VKS_CODEC_WORKER_POOL` for any caller that races startup.
#[must_use]
pub fn pool() -> Option<&'static WorkerPool> {
    POOL.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_flag_parsing_is_explicit() {
        assert!(truthy(Some("1")));
        assert!(truthy(Some("true")));
        assert!(!truthy(Some("0")));
        assert!(!truthy(None));
    }

    #[test]
    fn pool_is_absent_before_init() {
        assert!(pool().is_none());
    }

    #[test]
    fn worker_metric_labels_are_subset_of_specs() {
        // Every `/metrics CODEC_WORKER_SUPPORTED{codec=...}` label must resolve
        // through the SSOT (T15): a label the matrix does not know is a lie.
        for label in ["aac_latm", "g729", "h261", "h263", "h263p", "h265"] {
            assert!(vks_common::codec_spec::lookup(label).is_some(), "metric label {label} is not in CODECS");
        }
    }
}
