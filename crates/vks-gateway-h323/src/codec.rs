//! H.323 → WebRTC codec negotiation (ADR-0007, DEVELOPMENT_PLAN Фаза 6).
//!
//! WebRTC-комната говорит Opus (audio) / VP8/VP9 (video). H.323-терминалы
//! предлагают H.263/H.264 (video) и G.711/G.729 (audio) → mismatch →
//! транскодирование на шлюзе (participant = gateway).
//!
//! [`H323CodecMap::negotiate`] — детерминированная таблица соответствий:
//!
//! | H.323-оффер | WebRTC-кодек | транскодирование |
//! |-------------|--------------|------------------|
//! | G.711 (PCMU/PCMA) | Opus | да |
//! | G.729   | Opus | да |
//! | OPUS    | Opus | нет |
//! | H.263   | VP8  | да |
//! | H.264   | VP9  | да |
//! | VP8     | VP8  | нет |
//!
//! Видео: H.263 → VP8 (простой профиль), H.264 → VP9 (высокое качество).
//! Аудио: G.711/G.729 → Opus (Opus заменяет и low-bandwidth G.729).

use serde::{Deserialize, Serialize};

/// Кодек из H.323-оффера (H.225 capability advertisement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct H323Codec {
    /// Название кодека (например `PCMU`, `G729`, `H263`, `H264`).
    pub name: String,
}

impl H323Codec {
    /// Создаёт описание H.323-кодека по имени.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// Кодек WebRTC-стороны комнаты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebRtcCodec {
    /// Название кодека (например `OPUS`, `VP8`, `VP9`).
    pub name: String,
}

impl WebRtcCodec {
    /// Создаёт описание WebRTC-кодека по имени.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// Результат negotiate: выбранный WebRTC-кодек + нужен ли мост-транскодер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedCodec {
    /// WebRTC-кодек, которым комната будет принимать этот поток.
    pub web: WebRtcCodec,
    /// `true`, если шлюзу требуется транскодирование H.323→WebRTC.
    pub needs_transcode: bool,
}

/// Детерминированная таблица соответствий H.323 ↔ WebRTC.
#[derive(Debug, Clone, Copy, Default)]
pub struct H323CodecMap;

impl H323CodecMap {
    /// Нормализует имя H.323-кодека к каноническому виду.
    fn canonical(name: &str) -> String {
        let upper: String = name.to_ascii_uppercase().chars().filter(|c| *c != '.' && *c != ' ').collect();
        match upper.as_str() {
            "PCMU" | "PCMA" | "G711" => "G711".to_owned(),
            "G729" => "G729".to_owned(),
            "OPUS" => "OPUS".to_owned(),
            "H263" => "H263".to_owned(),
            "H264" => "H264".to_owned(),
            "VP8" => "VP8".to_owned(),
            "VP9" => "VP9".to_owned(),
            other => other.to_owned(),
        }
    }

    /// Является ли аудио-кодеком.
    fn is_audio(name: &str) -> bool {
        matches!(Self::canonical(name).as_str(), "G711" | "G729" | "OPUS")
    }

    /// Является ли видео-кодеком.
    fn is_video(name: &str) -> bool {
        matches!(Self::canonical(name).as_str(), "H263" | "H264" | "VP8" | "VP9")
    }

    /// Выбирает WebRTC-кодек для данного H.323-кодека.
    ///
    /// Returns: `None`, если кодек не audio и не video.
    fn for_codec(h323: &H323Codec) -> Option<NegotiatedCodec> {
        let canonical = Self::canonical(&h323.name);
        let web = if Self::is_audio(&h323.name) {
            // All H.323 audio → Opus.
            "OPUS"
        } else if canonical == "H263" {
            "VP8"
        } else if canonical == "H264" {
            "VP9"
        } else if Self::is_video(&h323.name) {
            // VP8/VP9 — native WebRTC, no transcode.
            canonical.as_str()
        } else {
            return None;
        };
        Some(NegotiatedCodec { web: WebRtcCodec::new(web), needs_transcode: canonical != web })
    }

    /// Переговор по полному H.323-офферу.
    ///
    /// Первый аудио-кодек определяет audio-поток, при его отсутствии —
    /// первый видео-кодек. Возвращает `(web_codec, needs_transcode)`.
    /// Пустой оффер → Opus, без транскодирования.
    pub fn negotiate(h323_offer: &[H323Codec]) -> (WebRtcCodec, bool) {
        let chosen = h323_offer
            .iter()
            .find(|c| Self::is_audio(&c.name))
            .or_else(|| h323_offer.iter().find(|c| Self::is_video(&c.name)));
        match chosen.and_then(Self::for_codec) {
            Some(n) => (n.web, n.needs_transcode),
            None => (WebRtcCodec::new("OPUS"), false),
        }
    }

    /// Неготация отдельно для audio- и video-потоков (полный мост).
    ///
    /// Возвращает `(Option audio, Option video)`: `None` — поток в
    /// оффере не был предложен.
    pub fn negotiate_streams(h323_offer: &[H323Codec]) -> (Option<NegotiatedCodec>, Option<NegotiatedCodec>) {
        let audio = h323_offer.iter().find(|c| Self::is_audio(&c.name)).and_then(Self::for_codec);
        let video = h323_offer.iter().find(|c| Self::is_video(&c.name)).and_then(Self::for_codec);
        (audio, video)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h263_maps_to_vp8_with_transcode() {
        let offer = [H323Codec::new("H263")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "VP8");
        assert!(transcode);
    }

    #[test]
    fn h264_maps_to_vp9_with_transcode() {
        let offer = [H323Codec::new("H264")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "VP9");
        assert!(transcode);
    }

    #[test]
    fn g711_maps_to_opus_with_transcode() {
        let offer = [H323Codec::new("PCMU")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(transcode);
    }

    #[test]
    fn g729_maps_to_opus_with_transcode() {
        let offer = [H323Codec::new("G729")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(transcode);
    }

    #[test]
    fn opus_offer_needs_no_transcode() {
        let offer = [H323Codec::new("OPUS")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(!transcode);
    }

    #[test]
    fn vp8_offer_needs_no_transcode() {
        let offer = [H323Codec::new("VP8")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "VP8");
        assert!(!transcode);
    }

    #[test]
    fn vp9_offer_needs_no_transcode() {
        let offer = [H323Codec::new("VP9")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "VP9");
        assert!(!transcode);
    }

    #[test]
    fn audio_priority_over_video_in_mixed_offer() {
        let offer = [H323Codec::new("H263"), H323Codec::new("PCMU")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(transcode);
    }

    #[test]
    fn negotiate_streams_splits_audio_and_video() {
        let offer = [H323Codec::new("G729"), H323Codec::new("H264")];
        let (audio, video) = H323CodecMap::negotiate_streams(&offer);
        let audio = audio.expect("audio negotiated");
        let video = video.expect("video negotiated");
        assert_eq!(audio.web.name, "OPUS");
        assert!(audio.needs_transcode);
        assert_eq!(video.web.name, "VP9");
        assert!(video.needs_transcode);
    }

    #[test]
    fn empty_offer_defaults_to_opus() {
        let (web, transcode) = H323CodecMap::negotiate(&[]);
        assert_eq!(web.name, "OPUS");
        assert!(!transcode);
    }

    #[test]
    fn unknown_codec_ignored() {
        let offer = [H323Codec::new("telephone-event")];
        let (web, transcode) = H323CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(!transcode);
    }
}
