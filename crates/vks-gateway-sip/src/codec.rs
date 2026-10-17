//! Codec negotiation SIP SDP → WebRTC SDP (ADR-0006, DEVELOPMENT_PLAN Фаза 6).
//!
//! WebRTC-комната говорит Opus (audio) / VP8 (video). SIP-терминал обычно
//! предлагает G.711 (audio) / H.264 (video). Мismatch → транскодирование на
//! шлюзе (participant = gateway ретранслирует потоки).
//!
//! [`CodecMap::negotiate`] — детерминированная таблица соответствий:
//!
//! | SIP-оффер  | WebRTC-кодек | транскодирование |
//! |------------|--------------|------------------|
//! | PCMU/PCMA  (G.711) | Opus | да |
//! | H.264      | VP8  | да |
//! | Opus       | Opus | нет |
//! | VP8        | VP8  | нет |
//!
//! Аудио/видео разделяются: первый неподдерживаемый audio-кодек в оффере
//! выбирается для audio, первый video-кодек — для video.

use serde::{Deserialize, Serialize};

/// Кодек из SIP SDP-оффера (payload name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SipCodec {
    /// Название кодека (например `PCMU`, `PCMA`, `H264`, `OPUS`).
    pub name: String,
}

impl SipCodec {
    /// Создаёт описание SIP-кодека по имени.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// Кодек WebRTC-стороны комнаты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebRtcCodec {
    /// Название кодека (например `OPUS`, `VP8`).
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
    /// `true`, если шлюзу требуется транскодирование SIP→WebRTC.
    pub needs_transcode: bool,
}

/// Ошибка неготации кодеков.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecNegotiationError {
    #[error("SDP codec offer is empty")]
    EmptyOffer,
    #[error("no compatible audio or video codec found in offer: {0}")]
    NoCompatibleCodec(String),
}

/// Результат успешной неготации с сохранением исходного кодека.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedSelection {
    /// Выбранный кодек из оффера абонента.
    pub offered_codec: SipCodec,
    /// Соответствующий целевой WebRTC-кодек и необходимость транскодирования.
    pub negotiated: NegotiatedCodec,
}

/// Детерминированная таблица соответствий SIP ↔ WebRTC.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodecMap;

impl CodecMap {
    /// WebRTC-аудио-кодек комнаты.
    pub const AUDIO: &'static str = "OPUS";
    /// WebRTC-видео-кодек комнаты.
    pub const VIDEO: &'static str = "VP8";

    /// Нормализует имя SIP-кодека к каноническому виду.
    ///
    /// Аудио: `PCMU`/`PCMA` → `G711`; `OPUS` → `OPUS`.
    /// Видео: `H264`/`H.264` → `H264`; `VP8`/`VP9` → как есть.
    fn canonical(name: &str) -> String {
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "PCMU" | "PCMA" => "G711".to_owned(),
            "H.264" | "H264" => "H264".to_owned(),
            other => other.to_owned(),
        }
    }

    /// Является ли аудио-кодеком.
    fn is_audio(name: &str) -> bool {
        matches!(Self::canonical(name).as_str(), "G711" | "OPUS")
    }

    /// Является ли видео-кодеком.
    fn is_video(name: &str) -> bool {
        matches!(Self::canonical(name).as_str(), "H264" | "VP8" | "VP9")
    }

    /// Выбирает WebRTC-кодек для данного SIP-кодека и флагирует
    /// необходимость транскодирования.
    ///
    /// Returns: `None`, если кодек не audio и не video (например DTMF).
    fn for_codec(sip: &SipCodec) -> Option<NegotiatedCodec> {
        let canonical = Self::canonical(&sip.name);
        let (web, transcode) = if Self::is_audio(&sip.name) {
            (Self::AUDIO, canonical != Self::AUDIO)
        } else if Self::is_video(&sip.name) {
            (Self::VIDEO, canonical != Self::VIDEO)
        } else {
            return None;
        };
        Some(NegotiatedCodec { web: WebRtcCodec::new(web), needs_transcode: transcode })
    }

    /// Строгая неготация кодека по SIP SDP-офферу.
    /// Возвращает ошибку, если оффер пуст или не содержит ни одного поддерживаемого аудио/видео кодека.
    pub fn try_negotiate(sip_offer: &[SipCodec]) -> Result<NegotiatedSelection, CodecNegotiationError> {
        if sip_offer.is_empty() {
            return Err(CodecNegotiationError::EmptyOffer);
        }

        let chosen = sip_offer
            .iter()
            .find(|c| Self::is_audio(&c.name))
            .or_else(|| sip_offer.iter().find(|c| Self::is_video(&c.name)));

        match chosen {
            Some(c) => {
                let negotiated = Self::for_codec(c).ok_or_else(|| {
                    let list = sip_offer.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ");
                    CodecNegotiationError::NoCompatibleCodec(list)
                })?;
                Ok(NegotiatedSelection {
                    offered_codec: c.clone(),
                    negotiated,
                })
            }
            None => {
                let list = sip_offer.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ");
                Err(CodecNegotiationError::NoCompatibleCodec(list))
            }
        }
    }

    /// Переговор по полному SIP SDP-офферу с обратной совместимостью (fallback к OPUS при ошибке).
    pub fn negotiate(sip_offer: &[SipCodec]) -> (WebRtcCodec, bool) {
        match Self::try_negotiate(sip_offer) {
            Ok(sel) => (sel.negotiated.web, sel.negotiated.needs_transcode),
            Err(_) => (WebRtcCodec::new(Self::AUDIO), false),
        }
    }

    /// Неготация отдельно для audio- и video-потоков (полный мост).
    ///
    /// Возвращает `Option` на каждый поток: `None` — поток в оффере не
    /// был предложен.
    pub fn negotiate_streams(sip_offer: &[SipCodec]) -> (Option<NegotiatedCodec>, Option<NegotiatedCodec>) {
        let audio = sip_offer.iter().find(|c| Self::is_audio(&c.name)).and_then(Self::for_codec);
        let video = sip_offer.iter().find(|c| Self::is_video(&c.name)).and_then(Self::for_codec);
        (audio, video)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_pcma_maps_to_opus_with_transcode() {
        let offer = [SipCodec::new("PCMA")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(transcode);
    }

    #[test]
    fn g711_pcmu_maps_to_opus_with_transcode() {
        let offer = [SipCodec::new("PCMU")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(transcode);
    }

    #[test]
    fn opus_offer_needs_no_transcode() {
        let offer = [SipCodec::new("OPUS")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(!transcode);
    }

    #[test]
    fn h264_maps_to_vp8_with_transcode() {
        let offer = [SipCodec::new("H264")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "VP8");
        assert!(transcode);
    }

    #[test]
    fn h264_with_dot_maps_to_vp8() {
        let offer = [SipCodec::new("H.264")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "VP8");
        assert!(transcode);
    }

    #[test]
    fn vp8_offer_needs_no_transcode() {
        let offer = [SipCodec::new("VP8")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "VP8");
        assert!(!transcode);
    }

    #[test]
    fn audio_priority_over_video_in_mixed_offer() {
        let offer = [SipCodec::new("H264"), SipCodec::new("PCMU")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(transcode);
    }

    #[test]
    fn empty_offer_defaults_to_opus() {
        let (web, transcode) = CodecMap::negotiate(&[]);
        assert_eq!(web.name, "OPUS");
        assert!(!transcode);
    }

    #[test]
    fn negotiate_streams_splits_audio_and_video() {
        let offer = [SipCodec::new("PCMA"), SipCodec::new("H264")];
        let (audio, video) = CodecMap::negotiate_streams(&offer);
        let audio = audio.expect("audio negotiated");
        let video = video.expect("video negotiated");
        assert_eq!(audio.web.name, "OPUS");
        assert!(audio.needs_transcode);
        assert_eq!(video.web.name, "VP8");
        assert!(video.needs_transcode);
    }

    #[test]
    fn negotiate_streams_audio_only() {
        let offer = [SipCodec::new("OPUS")];
        let (audio, video) = CodecMap::negotiate_streams(&offer);
        assert!(audio.is_some());
        assert!(video.is_none());
    }

    #[test]
    fn unknown_codec_ignored() {
        let offer = [SipCodec::new("telephone-event")];
        let (web, transcode) = CodecMap::negotiate(&offer);
        assert_eq!(web.name, "OPUS");
        assert!(!transcode);
    }
}
