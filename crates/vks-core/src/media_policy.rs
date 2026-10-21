//! Разрешение медиапараметров по шести уровням (L1…L6).
//!
//! Модель взята из контрактов TrueConf MCU (профиль → конференция → сессия →
//! адрес/терминал) и числовой лестницы Cisco CMS. Здесь нет I/O: слои
//! передаются вызывающим кодом, ядро лишь применяет приоритет и ограничения.
//!
//! Приоритет — **fallback**: более высокий слой (L6) перекрывает низкий (L4),
//! а не выбирается минимум. При этом:
//! - кодек — **пересечение** списков (профиль ∩ whitelist ∩ терминал);
//! - битрейт/разрешение/fps — значение верхнего слоя, зажатое в допустимый
//!   диапазон; жёсткий потолок движка задаётся `max_bitrate_kbps`/`max_width`.
//!
//! ## Инварианты
//! - Результат детерминирован для одного и того же набора слоёв.
//! - Ширина/высота — чётные, ≥ 16; fps ∈ [1, 60]; битрейт ∈ [32, 50 000] кбит/с.
//! - Пустой список кодеков в слое означает «не ограничивать», а не «запретить».

/// Один уровень медиаполитики. `None` — уровень не задаёт параметр.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaLayer {
    /// Разрешённые аудиокодеки (канонические имена: `OPUS`, `G711`, `G722`…).
    pub audio_codecs: Option<Vec<String>>,
    /// Разрешённые видеокодеки (`H264`, `H265`, `VP8`…).
    pub video_codecs: Option<Vec<String>>,
    /// Целевой битрейт видео, кбит/с.
    pub max_bitrate_kbps: Option<u32>,
    /// Ширина кадра.
    pub width: Option<u32>,
    /// Высота кадра.
    pub height: Option<u32>,
    /// Частота кадров.
    pub fps: Option<u32>,
    /// Профиль кодека (`baseline`, `main`, `high`, `constrained-baseline`).
    pub profile: Option<String>,
    /// Уровень H.264 (`3.0`, `3.1`, `4.0`…).
    pub level: Option<String>,
}

impl MediaLayer {
    /// Пустой слой (ничего не задаёт).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Слой только с кодеками.
    #[must_use]
    pub fn codecs(audio: &[&str], video: &[&str]) -> Self {
        Self {
            audio_codecs: Some(audio.iter().map(|value| (*value).to_string()).collect()),
            video_codecs: Some(video.iter().map(|value| (*value).to_string()).collect()),
            ..Self::default()
        }
    }
}

/// Итоговые параметры, которые применяются к SDP, шлюзу и MCU-композитору.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveMedia {
    pub audio_codecs: Vec<String>,
    pub video_codecs: Vec<String>,
    pub max_bitrate_kbps: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub profile: String,
    pub level: String,
}

/// Аудиокодеки по умолчанию: royalty-free и всегда доступные в native-слое.
pub const DEFAULT_AUDIO_CODECS: &[&str] = &["OPUS", "G711", "G722", "G726"];
/// Видеокодеки по умолчанию: OpenH264 — единственный native-энкодер без GPL.
pub const DEFAULT_VIDEO_CODECS: &[&str] = &["H264"];

const MIN_BITRATE_KBPS: u32 = 32;
const MAX_BITRATE_KBPS: u32 = 50_000;
const MIN_DIMENSION: u32 = 16;
const MAX_DIMENSION: u32 = 7680;
const MIN_FPS: u32 = 1;
const MAX_FPS: u32 = 60;

/// Применяет слои от низкого (L1) к высокому (L6).
///
/// `layers` должен быть упорядочен от общего к частному: движок → профиль →
/// конференция → сессия → адрес/терминал.
#[must_use]
pub fn resolve(layers: &[MediaLayer]) -> EffectiveMedia {
    let mut audio: Vec<String> = DEFAULT_AUDIO_CODECS.iter().map(|value| (*value).to_string()).collect();
    let mut video: Vec<String> = DEFAULT_VIDEO_CODECS.iter().map(|value| (*value).to_string()).collect();
    let mut bitrate = 2048;
    let mut width = 1280;
    let mut height = 720;
    let mut fps = 30;
    let mut profile = "baseline".to_string();
    let mut level = "3.1".to_string();

    for layer in layers {
        if let Some(list) = layer.audio_codecs.as_ref().filter(|list| !list.is_empty()) {
            audio.retain(|codec| list.iter().any(|allowed| allowed.eq_ignore_ascii_case(codec)));
            if audio.is_empty() {
                // Пересечение пусто — слой не должен «запрещать всё»: сохраняем
                // предыдущий набор, иначе терминал останется без аудио.
                audio = list.clone();
            }
        }
        if let Some(list) = layer.video_codecs.as_ref().filter(|list| !list.is_empty()) {
            video.retain(|codec| list.iter().any(|allowed| allowed.eq_ignore_ascii_case(codec)));
            if video.is_empty() {
                video = list.clone();
            }
        }
        if let Some(value) = layer.max_bitrate_kbps {
            bitrate = value;
        }
        if let Some(value) = layer.width {
            width = value;
        }
        if let Some(value) = layer.height {
            height = value;
        }
        if let Some(value) = layer.fps {
            fps = value;
        }
        if let Some(value) = layer.profile.as_ref().filter(|value| !value.is_empty()) {
            profile = value.clone();
        }
        if let Some(value) = layer.level.as_ref().filter(|value| !value.is_empty()) {
            level = value.clone();
        }
    }

    width = even_clamp(width, MIN_DIMENSION, MAX_DIMENSION);
    height = even_clamp(height, MIN_DIMENSION, MAX_DIMENSION);
    EffectiveMedia {
        audio_codecs: audio,
        video_codecs: video,
        max_bitrate_kbps: bitrate.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS),
        width,
        height,
        fps: fps.clamp(MIN_FPS, MAX_FPS),
        profile,
        level,
    }
}

fn even_clamp(value: u32, min: u32, max: u32) -> u32 {
    let clamped = value.clamp(min, max);
    if clamped.is_multiple_of(2) {
        clamped
    } else {
        (clamped + 1).min(max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_layer_overrides_lower() {
        let layers = [
            MediaLayer { max_bitrate_kbps: Some(4096), width: Some(1920), height: Some(1080), ..MediaLayer::empty() },
            MediaLayer {
                max_bitrate_kbps: Some(1024),
                width: Some(640),
                height: Some(360),
                fps: Some(15),
                ..MediaLayer::empty()
            },
        ];
        let media = resolve(&layers);
        assert_eq!(media.max_bitrate_kbps, 1024);
        assert_eq!((media.width, media.height, media.fps), (640, 360, 15));
    }

    #[test]
    fn codecs_are_intersected_and_order_kept() {
        let media = resolve(&[MediaLayer::codecs(&["G711", "OPUS"], &["H264", "H265"])]);
        assert_eq!(media.audio_codecs, vec!["OPUS".to_string(), "G711".to_string()]);
        // H265 не входит в native-набор по умолчанию, поэтому пересечение — только H264.
        assert_eq!(media.video_codecs, vec!["H264".to_string()]);
    }

    #[test]
    fn empty_layer_does_not_restrict() {
        let media = resolve(&[MediaLayer::empty()]);
        assert!(media.audio_codecs.iter().any(|codec| codec == "OPUS"));
        assert!(media.video_codecs.iter().any(|codec| codec == "H264"));
        assert_eq!((media.width, media.height), (1280, 720));
    }

    #[test]
    fn values_are_clamped_and_even() {
        let media = resolve(&[MediaLayer {
            max_bitrate_kbps: Some(1),
            width: Some(15),
            height: Some(10_000),
            fps: Some(0),
            ..MediaLayer::empty()
        }]);
        assert_eq!(media.max_bitrate_kbps, MIN_BITRATE_KBPS);
        assert_eq!(media.width, 16);
        assert_eq!(media.height, MAX_DIMENSION);
        assert_eq!(media.fps, MIN_FPS);
    }

    #[test]
    fn profile_and_level_fallback() {
        let media = resolve(&[
            MediaLayer { profile: Some("baseline".into()), level: Some("3.0".into()), ..MediaLayer::empty() },
            MediaLayer { profile: Some("high".into()), ..MediaLayer::empty() },
        ]);
        assert_eq!(media.profile, "high");
        assert_eq!(media.level, "3.0");
    }
}
