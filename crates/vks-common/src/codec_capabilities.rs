//! Единый источник правды о кодеках, которые шлюз реально мостит без
//! транскодера (T06a). Списки настроек/UI остаются «намерением» и могут
//! содержать полную матрицу T06b; SDP и выбор кодека фильтруются здесь.

/// Аудио, которое native-профиль шлюза принимает/отдаёт без транскодера.
pub const NATIVE_AUDIO: &[&str] = &["PCMU", "PCMA", "G711", "G711U", "G711A", "G722"];
/// Видео, которое native-профиль шлюза кодирует/декодирует (H.264 baseline mode=1).
pub const NATIVE_VIDEO: &[&str] = &["H264"];

fn normalize(name: &str) -> String {
    name.trim().to_ascii_uppercase().replace(['.', '-', '_', ' '], "")
}

/// Мостится ли аудиокодек native-профилем (`G711` — общий алиас PCMU/PCMA).
#[must_use]
pub fn is_native_audio(name: &str) -> bool {
    NATIVE_AUDIO.contains(&normalize(name).as_str())
}

/// Мостится ли видеокодек native-профилем. `H264-HIGH`/`H265` — нет.
#[must_use]
pub fn is_native_video(name: &str) -> bool {
    NATIVE_VIDEO.contains(&normalize(name).as_str())
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
        for name in ["PCMU", "PCMA", "G711", "g711u", "G.711-A", "G722", "g722"] {
            assert!(is_native_audio(name), "{name} должен моститься");
        }
        for name in ["G729", "OPUS", "AAC", "G7221C"] {
            assert!(!is_native_audio(name), "{name} требует T06b-транскодера");
        }
        assert!(is_native_video("H264"));
        assert!(is_native_video("h.264"));
        for name in ["H264-HIGH", "H265", "H263", "VP8"] {
            assert!(!is_native_video(name), "{name} требует T06b-транскодера");
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
}
