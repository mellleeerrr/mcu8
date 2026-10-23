//! Единый источник правды о кодеках, которые шлюз реально мостит без
//! транскодера (T06a). Списки настроек/UI остаются «намерением» и могут
//! содержать полную матрицу T06b; SDP и выбор кодека фильтруются здесь.
//!
//! Для видео одного статического списка мало: H.264/H.265 мосты запускают
//! внешний FFmpeg, поэтому SDP обязан спрашивать [`video_codec_available`],
//! а `main` выставляет её по результату probe. Значения по умолчанию `true`
//! нужны unit-тестам и библиотечным вызовам; production всегда переопределяет
//! их на старте.

/// Аудио, которое native-профиль шлюза принимает/отдаёт без транскодера.
pub const NATIVE_AUDIO: &[&str] =
    &["PCMU", "PCMA", "G711", "G711U", "G711A", "G722", "G72616", "G72624", "G72632", "G72640", "G728", "G723", "OPUS"];
/// Видео, которое native-профиль шлюза кодирует/декодирует: H.264 baseline
/// (high — отдельная конфигурация) и H.265 через FFmpeg-мост.
pub const NATIVE_VIDEO: &[&str] = &["H264", "H265", "HEVC"];

static H264_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static H265_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Результат FFmpeg probe: можно ли реально поднять мост для кодека.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoCodecAvailability {
    pub h264: bool,
    pub h265: bool,
}

/// Доступность аудио-мостов, зависящих от внешнего FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCodecAvailability {
    /// MP4A-LATM bridge: AAC encoder + `latm` muxer + `aac_latm` decoder + `loas` demuxer.
    pub aac_latm: bool,
}

static AAC_LATM_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Записывает доступность аудио-мостов (один раз на старте сервера).
pub fn set_audio_codec_availability(availability: AudioCodecAvailability) {
    AAC_LATM_AVAILABLE.store(availability.aac_latm, std::sync::atomic::Ordering::Release);
}

/// Доступен ли AAC-LATM мост на этом хосте. До публикации результата probe
/// возвращает `false`, чтобы SDP никогда не обещал AAC без воркера.
#[must_use]
pub fn audio_codec_available(name: &str) -> bool {
    audio_codec_available_with(name, AAC_LATM_AVAILABLE.load(std::sync::atomic::Ordering::Acquire))
}

fn audio_codec_available_with(name: &str, aac_latm: bool) -> bool {
    match normalize(name).as_str() {
        "AAC" | "AACLATM" | "MP4ALATM" | "MP4A-LATM" => aac_latm,
        _ => false,
    }
}

/// Записывает доступность видео-мостов (один раз на старте сервера).
pub fn set_video_codec_availability(availability: VideoCodecAvailability) {
    H264_AVAILABLE.store(availability.h264, std::sync::atomic::Ordering::Release);
    H265_AVAILABLE.store(availability.h265, std::sync::atomic::Ordering::Release);
}

/// Доступен ли мост конкретного видео-кодека на этом хосте.
#[must_use]
pub fn video_codec_available(name: &str) -> bool {
    match normalize(name).as_str() {
        "H264" => H264_AVAILABLE.load(std::sync::atomic::Ordering::Acquire),
        "H265" | "HEVC" => H265_AVAILABLE.load(std::sync::atomic::Ordering::Acquire),
        _ => false,
    }
}

fn normalize(name: &str) -> String {
    name.trim().to_ascii_uppercase().replace(['.', '-', '_', ' '], "")
}

/// Мостится ли аудиокодек native-профилем (`G711` — общий алиас PCMU/PCMA).
#[must_use]
pub fn is_native_audio(name: &str) -> bool {
    NATIVE_AUDIO.contains(&normalize(name).as_str())
}

/// Variant-aware audio check: `G726` is only supported at 16/24/32/40 kbit/s,
/// so settings entries `{name:"G726", bitrate:...}` must be validated with
/// their bitrate instead of a generic name match.
#[must_use]
pub fn is_native_audio_variant(name: &str, bitrate: Option<u32>) -> bool {
    if let Some(suffix) = normalize(name).strip_prefix("G726") {
        if let Ok(bits) = suffix.parse::<u32>() {
            return matches!(bits, 16 | 24 | 32 | 40) && bitrate.is_none_or(|value| value == bits);
        }
        return matches!(bitrate, Some(16 | 24 | 32 | 40));
    }
    is_native_audio(name)
}

/// Audio codec the SIP gateway can actually bridge: the native sample codecs
/// plus AAC-LATM once its FFmpeg workers were probed. H.323 callers must not
/// use this (its single-codec matrix stays G.711/G.722/G.723.1).
#[must_use]
pub fn is_supported_audio_variant(name: &str, bitrate: Option<u32>) -> bool {
    is_native_audio_variant(name, bitrate)
        || (matches!(normalize(name).as_str(), "AAC" | "MP4ALATM") && audio_codec_available("MP4A-LATM"))
}

/// Мостится ли видеокодек native-профилем *и* доступен ли его мост на хосте.
/// `H264-HIGH` не входит в native-список (это отдельная конфигурация).
#[must_use]
pub fn is_native_video(name: &str) -> bool {
    native_video_with(name, video_codec_available("H264"), video_codec_available("H265"))
}

fn native_video_with(name: &str, h264: bool, h265: bool) -> bool {
    match normalize(name).as_str() {
        "H264" => h264,
        "H265" | "HEVC" => h265,
        _ => false,
    }
}

fn truthy(value: Option<&str>) -> bool {
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
        for name in ["PCMU", "PCMA", "G711", "g711u", "G.711-A", "G722", "g722", "OPUS", "opus"] {
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
        assert!(is_native_video("H264"));
        assert!(is_native_video("h.264"));
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
    }

    #[test]
    fn supported_audio_variant_tracks_the_aac_probe() {
        assert!(is_supported_audio_variant("G722", None));
        assert!(is_supported_audio_variant("G726", Some(32)));
        assert!(!is_supported_audio_variant("G726", Some(20)));
        assert!(!is_supported_audio_variant("AAC", None));
        assert!(audio_codec_available_with("AAC", true));
        assert!(!is_supported_audio_variant("AAC", None), "global probe is off in tests");
    }

    #[test]
    fn aac_is_unavailable_until_the_host_probe_publishes_it() {
        assert!(!audio_codec_available("AAC"));
        assert!(!audio_codec_available("MP4A-LATM"));
        assert!(!audio_codec_available("G711"));
        assert!(audio_codec_available_with("aac", true));
        assert!(!audio_codec_available_with("AAC", false));
    }
}
