//! Public compatibility facade over [`crate::codec_spec`] (T04b).
//!
//! The SSOT lives in [`crate::codec_spec::CODECS`]; this module keeps the
//! historical API (`NATIVE_AUDIO`, `NATIVE_VIDEO`, `is_*`, `matrix_mode`) so
//! existing callers keep working while deriving every answer from the table.
//!
//! Probe state is process-global: `main` publishes it once at startup, tests
//! use the pure `*_with` helpers so they never race on the atomics.

use crate::codec_spec::{self, CodecId, CodecKind, Disposition, Reason};

/// Аудио, которое native-профиль шлюза принимает/отдаёт без транскодера.
/// Производный срез [`codec_spec::CODECS`] (канонические имена + алиасы).
pub const NATIVE_AUDIO: &[&str] = &codec_spec::native_audio_names();
/// Видео, которое native-профиль шлюза кодирует/декодирует: H.264 baseline
/// (high — RX-only) и H.265 через FFmpeg-мост.
pub const NATIVE_VIDEO: &[&str] = &codec_spec::native_video_names();
/// Worker/transcode-срез (AAC-LATM и алиасы).
pub const WORKER_CODECS: &[&str] = &codec_spec::worker_codec_names();
/// Canonical native audio names for public JSON (`/net/status`): one entry per
/// codec, no alias duplicates (`G726-16` but not `G72616`; `G723` but not
/// `G7231`). Settings validation keeps using [`NATIVE_AUDIO`].
pub const PUBLIC_NATIVE_AUDIO: &[&str] = &codec_spec::public_native_audio_names();
/// Canonical native video names for public JSON (`H264`, `H265`; no `HEVC`).
pub const PUBLIC_NATIVE_VIDEO: &[&str] = &codec_spec::public_native_video_names();

static H264_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static H265_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Результат FFmpeg probe: можно ли реально поднять мост для кодека.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoCodecAvailability {
    pub h264: bool,
    pub h265: bool,
}

/// Доступность аудио-мостов, зависящих от внешнего FFmpeg и лицензионного
/// решения оператора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCodecAvailability {
    /// Operator accepted `VKS_CODEC_LICENSE_ACCEPTED=aac-latm`.
    pub aac_latm_license: bool,
    /// FFmpeg probe found the encoder + `latm` muxer and decoder + `loas`
    /// demuxer.
    pub aac_latm_probe: bool,
    /// Worker pool can serve at least two AAC workers (encode + decode).
    pub aac_latm_pool: bool,
}

impl AudioCodecAvailability {
    /// The bridge is usable only when all three independent gates are green.
    #[must_use]
    pub const fn aac_latm_available(self) -> bool {
        self.aac_latm_license && self.aac_latm_probe && self.aac_latm_pool
    }
}

static AAC_LATM_LICENSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static AAC_LATM_PROBE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static AAC_LATM_POOL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Записывает доступность аудио-мостов (один раз на старте сервера).
pub fn set_audio_codec_availability(availability: AudioCodecAvailability) {
    AAC_LATM_LICENSE.store(availability.aac_latm_license, std::sync::atomic::Ordering::Release);
    AAC_LATM_PROBE.store(availability.aac_latm_probe, std::sync::atomic::Ordering::Release);
    AAC_LATM_POOL.store(availability.aac_latm_pool, std::sync::atomic::Ordering::Release);
}

fn aac_latm_available() -> bool {
    AAC_LATM_LICENSE.load(std::sync::atomic::Ordering::Acquire)
        && AAC_LATM_PROBE.load(std::sync::atomic::Ordering::Acquire)
        && AAC_LATM_POOL.load(std::sync::atomic::Ordering::Acquire)
}

/// Записывает доступность видео-мостов (один раз на старте сервера).
pub fn set_video_codec_availability(availability: VideoCodecAvailability) {
    H264_AVAILABLE.store(availability.h264, std::sync::atomic::Ordering::Release);
    H265_AVAILABLE.store(availability.h265, std::sync::atomic::Ordering::Release);
}

fn is_aac_latm(name: &str) -> bool {
    codec_spec::lookup(name).is_some_and(|spec| spec.id == CodecId::AacLatm)
}

/// Доступен ли AAC-LATM мост на этом хосте. До публикации результата probe
/// возвращает `false`, чтобы SDP никогда не обещал AAC без воркера.
#[must_use]
pub fn audio_codec_available(name: &str) -> bool {
    audio_codec_available_with(name, aac_latm_available())
}

fn audio_codec_available_with(name: &str, aac_latm: bool) -> bool {
    is_aac_latm(name) && aac_latm
}

/// Доступен ли мост конкретного видео-кодека на этом хосте.
#[must_use]
pub fn video_codec_available(name: &str) -> bool {
    match codec_spec::lookup(name).map(|spec| spec.id) {
        Some(CodecId::H264) => H264_AVAILABLE.load(std::sync::atomic::Ordering::Acquire),
        Some(CodecId::H265) => H265_AVAILABLE.load(std::sync::atomic::Ordering::Acquire),
        _ => false,
    }
}

/// Мостится ли аудиокодек native-профилем (`G711` — семейство PCMU/PCMA).
#[must_use]
pub fn is_native_audio(name: &str) -> bool {
    if let Some(family) = codec_spec::family(name) {
        return family.members.iter().any(|id| {
            codec_spec::spec(*id).kind == CodecKind::Audio && codec_spec::spec(*id).sip == Disposition::Native
        });
    }
    codec_spec::lookup(name).is_some_and(|spec| spec.kind == CodecKind::Audio && spec.sip == Disposition::Native)
}

/// Variant-aware audio check: `G726` is only supported at 16/24/32/40 kbit/s,
/// so settings entries `{name:"G726", bitrate:...}` must be validated with
/// their bitrate instead of a generic name match.
#[must_use]
pub fn is_native_audio_variant(name: &str, bitrate: Option<u32>) -> bool {
    if let Some(spec) = codec_spec::lookup_variant(name, bitrate) {
        return spec.kind == CodecKind::Audio && spec.sip == Disposition::Native;
    }
    // Alias families (G711) have no single spec.
    bitrate.is_none() && is_native_audio(name)
}

/// Audio codec the SIP gateway can actually bridge: native sample codecs plus
/// AAC-LATM once its FFmpeg workers were probed **and** the operator accepted
/// the license. H.323 callers must not use this (its matrix is static).
#[must_use]
pub fn is_supported_audio_variant(name: &str, bitrate: Option<u32>) -> bool {
    supported_audio_variant_with(name, bitrate, aac_latm_available())
}

fn supported_audio_variant_with(name: &str, bitrate: Option<u32>, aac_latm: bool) -> bool {
    if is_native_audio_variant(name, bitrate) {
        return true;
    }
    match codec_spec::lookup_variant(name, bitrate) {
        Some(spec) if spec.kind == CodecKind::Audio && spec.sip == Disposition::Worker => {
            is_aac_latm(spec.name) && aac_latm
        }
        _ => false,
    }
}

/// Мостится ли видеокодек native-профилем *и* доступен ли его мост на хосте.
/// `H264-HIGH` не входит в native-список (RX-only).
#[must_use]
pub fn is_native_video(name: &str) -> bool {
    native_video_with(name, video_codec_available("H264"), video_codec_available("H265"))
}

/// Кодеки, у которых на хосте есть **и кодировщик, и декодировщик**.
///
/// Односторонние пути сюда не входят: FFmpeg умеет декодировать G.729, но не
/// кодировать, а G.722.1/G.719 не умеет вообще. Объявлять их в SDP нельзя —
/// терминал ответит кодеком, который шлюз не сможет отдать обратно.
#[must_use]
pub fn is_transcodeable_audio(name: &str) -> bool {
    codec_spec::lookup(name).is_some_and(|spec| spec.kind == CodecKind::Audio && spec.sip == Disposition::Worker)
}

fn native_video_with(name: &str, h264: bool, h265: bool) -> bool {
    match codec_spec::lookup(name).map(|spec| spec.id) {
        Some(CodecId::H264) => h264,
        Some(CodecId::H265) => h265,
        _ => false,
    }
}

/// Public machine-readable reason why an audio settings variant is not
/// supported right now (`None` when it is).
#[must_use]
pub fn audio_unavailable_reason(name: &str, bitrate: Option<u32>) -> Option<&'static str> {
    audio_unavailable_reason_with(
        name,
        bitrate,
        AAC_LATM_LICENSE.load(std::sync::atomic::Ordering::Acquire),
        AAC_LATM_PROBE.load(std::sync::atomic::Ordering::Acquire),
        AAC_LATM_POOL.load(std::sync::atomic::Ordering::Acquire),
    )
}

/// Truthful reason order for a worker codec: license first (only when it is
/// actually the blocker), then probe, then pool, then the spec reason. Before
/// this helper every AAC row said `license_not_accepted` even when the operator
/// had accepted the license and FFmpeg/pool was the real cause (review A P2-7).
fn audio_unavailable_reason_with(
    name: &str,
    bitrate: Option<u32>,
    license: bool,
    probe: bool,
    pool: bool,
) -> Option<&'static str> {
    if supported_audio_variant_with(name, bitrate, license && probe && pool) {
        return None;
    }
    match codec_spec::lookup_variant(name, bitrate) {
        Some(spec) if spec.sip == Disposition::Worker => {
            if spec.license.token().is_some() && !license {
                Some(Reason::LicenseNotAccepted.as_id())
            } else if !probe {
                Some(Reason::ProbeUnavailable.as_id())
            } else if !pool {
                Some(Reason::WorkerPoolUnavailable.as_id())
            } else {
                Some(spec.reason.unwrap_or(Reason::NoImplementation).as_id())
            }
        }
        Some(spec) => Some(spec.reason.unwrap_or(Reason::NoImplementation).as_id()),
        // `G726:20` is a known codec with a bitrate outside the matrix: report
        // the real cause instead of "unknown_codec" (review A P2-6/7).
        None if codec_spec::is_g726_name(name) || codec_spec::is_variant_codec(name) => {
            Some(Reason::UnsupportedBitrate.as_id())
        }
        None => Some(Reason::UnknownCodec.as_id()),
    }
}

/// Public machine-readable reason why a video settings entry is not supported.
#[must_use]
pub fn video_unavailable_reason(name: &str) -> Option<&'static str> {
    if is_native_video(name) {
        return None;
    }
    let Some(spec) = codec_spec::lookup(name) else {
        return Some(Reason::UnknownCodec.as_id());
    };
    let reason = match spec.sip {
        Disposition::Native if !video_codec_available(name) => Reason::ProbeUnavailable,
        Disposition::Native | Disposition::Worker | Disposition::WebRtc => {
            spec.reason.unwrap_or(Reason::NoImplementation)
        }
        Disposition::RxOnly | Disposition::Unsupported(_) => spec.reason.unwrap_or(Reason::NoImplementation),
    };
    Some(reason.as_id())
}

/// True, если значение — «истина» в смысле env-переменных.
///
/// Единая трактовка `1`/`true`/`TRUE`/`yes`/`on` для всех флагов проекта.
#[must_use]
pub fn truthy(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1" | "true" | "TRUE" | "yes" | "on"))
}

/// Полная матрица в SDP допустима только с транскодером (T06b) или явным
/// lab-флагом. Без них `VKS_SDP_CODEC_MATRIX=full` не должен объявлять
/// немостимые кодеки: иначе терминал выберет их и звонок умрёт.
#[must_use]
pub fn full_matrix_allowed(transcoder: Option<&str>, lab: Option<&str>) -> bool {
    truthy(transcoder) || truthy(lab)
}

/// `native` (production) или `full` (lab/T06b). Неизвестное значение — native.
#[must_use]
pub fn matrix_mode(matrix: Option<&str>, transcoder: Option<&str>, lab: Option<&str>) -> &'static str {
    if matrix.map(str::trim).is_some_and(|value| value.eq_ignore_ascii_case("full"))
        && full_matrix_allowed(transcoder, lab)
    {
        "full"
    } else {
        "native"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_names_match_gateway_offer() {
        for name in ["PCMU", "PCMA", "G711", "g711u", "G.711-A", "G722", "g722", "OPUS", "opus", "G7231"] {
            assert!(is_native_audio(name), "{name} должен моститься");
        }
        for (name, bitrate) in [("G726", Some(16)), ("G726", Some(24)), ("G726", Some(32)), ("G726", Some(40))] {
            assert!(is_native_audio_variant(name, bitrate), "{name}:{bitrate:?} должен моститься");
        }
        for (name, bitrate) in
            [("G726", None), ("G726", Some(48)), ("G726", Some(8)), ("G72616", Some(48)), ("G72648", None)]
        {
            assert!(!is_native_audio_variant(name, bitrate), "{name}:{bitrate:?} не поддерживается");
        }
        assert!(is_native_audio_variant("G72616", None));
        for name in ["G729", "AAC", "G7221C", "G719"] {
            assert!(!is_native_audio(name), "{name} требует T06b-транскодера");
        }
        for name in ["H264", "h.264", "H265", "h.265", "HEVC"] {
            assert!(is_native_video(name), "{name} мостится worker-мостом");
        }
        for name in ["H264-HIGH", "H263", "VP8"] {
            assert!(!is_native_video(name), "{name} требует отдельного пейлоадера/моста");
        }
    }

    #[test]
    fn full_matrix_requires_transcoder_or_lab_flag() {
        assert_eq!(matrix_mode(Some("full"), None, None), "native");
        assert_eq!(matrix_mode(Some("full"), Some("0"), None), "native");
        assert_eq!(matrix_mode(Some("full"), Some("1"), None), "full");
        assert_eq!(matrix_mode(Some("FULL"), None, Some("1")), "full");
        assert_eq!(matrix_mode(Some("native"), Some("1"), Some("1")), "native");
        assert_eq!(matrix_mode(Some("garbage"), Some("1"), Some("1")), "native");
    }

    #[test]
    fn missing_video_bridge_hides_that_codec_only() {
        assert!(native_video_with("H264", true, false));
        assert!(!native_video_with("H265", true, false));
        assert!(!native_video_with("HEVC", true, false));
        assert!(native_video_with("H265", false, true));
        assert!(!native_video_with("H264", false, true));
        assert!(!native_video_with("H261", true, true));
        assert!(!native_video_with("H264-HIGH", true, true));
    }

    #[test]
    fn supported_audio_variant_tracks_the_aac_probe_and_license() {
        assert!(is_supported_audio_variant("G722", None));
        assert!(is_supported_audio_variant("G726", Some(32)));
        assert!(!is_supported_audio_variant("G726", Some(20)));
        assert!(!is_supported_audio_variant("AAC", None));
        assert!(audio_codec_available_with("AAC", true));
        assert!(!is_supported_audio_variant("AAC", None), "global probe is off in tests");
        // License/probe state is injected: without it AAC is not advertised,
        // with it the worker codec becomes supported.
        assert!(!supported_audio_variant_with("MP4A-LATM", None, false));
        assert!(supported_audio_variant_with("MP4A-LATM", None, true));
        assert!(supported_audio_variant_with("aac", None, true));
        assert!(supported_audio_variant_with("AAC-LATM", None, true));
        assert!(!supported_audio_variant_with("G729", None, true));
    }

    #[test]
    fn unsupported_reasons_are_complete() {
        assert_eq!(audio_unavailable_reason("AAC", None), Some("license_not_accepted"));
        assert_eq!(audio_unavailable_reason("AAC", None), Some(Reason::LicenseNotAccepted.as_id()));
        for (name, reason) in [
            ("G729", "encoder_not_bit_exact"),
            ("G7221", "no_implementation"),
            ("G7221C", "no_implementation"),
            ("G719", "no_implementation"),
            ("ILBC", "no_implementation"),
            ("not-a-codec", "unknown_codec"),
        ] {
            assert_eq!(audio_unavailable_reason(name, None), Some(reason), "{name}");
        }
        for (name, reason) in
            [("H264-HIGH", "tx_profile_unsupported"), ("H261", "no_implementation"), ("VP8", "webrtc_only")]
        {
            assert_eq!(video_unavailable_reason(name), Some(reason), "{name}");
        }
        assert!(video_unavailable_reason("H264").is_none());
        assert!(audio_unavailable_reason("PCMU", None).is_none());
        // G.726 has known variants: an unsupported bitrate is not an unknown codec.
        assert_eq!(audio_unavailable_reason("G726", Some(20)), Some("unsupported_bitrate"));
        assert_eq!(audio_unavailable_reason("G726:20", None), Some("unsupported_bitrate"));
        assert_eq!(audio_unavailable_reason("G72648", None), Some("unsupported_bitrate"));
        assert_eq!(audio_unavailable_reason("G726", Some(32)), None);
        assert_eq!(audio_unavailable_reason("G7221C", Some(48)), Some("no_implementation"));
        assert_eq!(audio_unavailable_reason("G7221C", Some(64)), Some("unsupported_bitrate"));
    }

    #[test]
    fn aac_reason_order_is_license_then_probe_then_pool() {
        // The operator has not accepted the license: license is the real cause.
        assert_eq!(audio_unavailable_reason_with("AAC", None, false, true, true), Some("license_not_accepted"));
        // License accepted, FFmpeg probe missing: a license error would lie.
        assert_eq!(audio_unavailable_reason_with("AAC", None, true, false, true), Some("probe_unavailable"));
        // Probe green, pool too small: report the worker pool.
        assert_eq!(audio_unavailable_reason_with("AAC", None, true, true, false), Some("worker_pool_unavailable"));
        // All gates green: supported, no reason.
        assert_eq!(audio_unavailable_reason_with("AAC", None, true, true, true), None);
        // Non-worker codecs never report a license/probe cause.
        assert_eq!(audio_unavailable_reason_with("G729", None, false, false, false), Some("encoder_not_bit_exact"));
    }

    #[test]
    fn only_two_way_audio_codecs_are_transcodeable() {
        assert!(is_transcodeable_audio("MP4A-LATM"));
        assert!(is_transcodeable_audio("aac"));
        for name in ["G729", "G7221", "G719", "PCMU", "OPUS"] {
            assert!(!is_transcodeable_audio(name), "{name} не имеет двустороннего пути");
        }
    }

    #[test]
    fn aac_is_unavailable_until_the_host_probe_publishes_it() {
        assert!(!audio_codec_available("AAC"));
        assert!(!audio_codec_available("MP4A-LATM"));
        assert!(!audio_codec_available("G711"));
        assert!(audio_codec_available_with("aac", true));
        assert!(!audio_codec_available_with("AAC", false));
    }

    #[test]
    fn native_consts_equal_generated_slices() {
        assert_eq!(NATIVE_AUDIO, &codec_spec::native_audio_names());
        assert_eq!(NATIVE_VIDEO, &codec_spec::native_video_names());
        assert_eq!(WORKER_CODECS, &codec_spec::worker_codec_names());
        assert!(NATIVE_AUDIO.contains(&"G7231"));
        assert!(NATIVE_VIDEO.contains(&"HEVC"));
        assert!(!NATIVE_VIDEO.contains(&"H264-HIGH"));
        assert_eq!(PUBLIC_NATIVE_AUDIO, &codec_spec::public_native_audio_names());
        assert_eq!(PUBLIC_NATIVE_VIDEO, &codec_spec::public_native_video_names());
        // Public lists are canonical: no aliases, no duplicates (review A P2-6).
        assert!(!PUBLIC_NATIVE_AUDIO.contains(&"G72616"));
        assert!(PUBLIC_NATIVE_AUDIO.contains(&"G726-16"));
        assert!(!PUBLIC_NATIVE_AUDIO.contains(&"G7231"));
        assert!(PUBLIC_NATIVE_AUDIO.contains(&"G723"));
        assert!(!PUBLIC_NATIVE_VIDEO.contains(&"HEVC"));
        let unique: std::collections::BTreeSet<&str> = PUBLIC_NATIVE_AUDIO.iter().copied().collect();
        assert_eq!(unique.len(), PUBLIC_NATIVE_AUDIO.len());
    }
}
