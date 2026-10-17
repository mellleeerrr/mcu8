// Треки и медиа.
//
// `TrackId` — идентификатор медиатрека. `TrackKind` — тип источника
// (камера / экран / микрофон). `MediaKind` — типовой класс медиат (в,
// видео, экран). ` `Media` — дескриптор медиатрека, используемый в
// подписках и в MediaEngine.
//
// Инвариант прозрачности MCU/SFU: в SFU клиент получает N треков (по
// одному на участника), в MCU — 1 композитный трек. Дескриптор трека
// единый в обоих режимах, чтобы клиентский SDK не знал о режиме.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Идентикатор медиатрека (UUIDv7 по ADR-0024 / ARCHITECTURE §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackId(Uuid);

impl TrackId {
    /// Новый глобально уникальный идентификатор трека.
    ///
    /// В ядре используется UUIDv4 (deterministic tests inject their own
    /// ids); в wire-слое может быть UUIDv7 — ядро не зависит от версии.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Создаёт идентификатор из существующего [`Uuid`].
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Возвращает внутренний [`Uuid`].
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl PartialOrd for TrackId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TrackId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for TrackId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Тип источника медиатрека.
///
/// `Video` — камера, `Screen` — трансляция экрана, `Audio` — микрофон.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Камера (видео).
    Video,
    /// Трансляция экрана.
    Screen,
    /// Микрофон (аудио).
    Audio,
}

impl TrackKind {
    /// Название типа (для логов и wire-форматов).
    pub fn as_str(&self) -> &'static str {
        match self {
            TrackKind::Video => "video",
            TrackKind::Screen => "screen",
            TrackKind::Audio => "audio",
        }
    }
}

impl std::fmt::Display for TrackKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Типовой класс медиатрека.
///
/// Отличается от [`TrackKind`] гранулярностью: `MediaKind::Video`
/// объединяет и камеру, и (в некоторых режимах) экран; `MediaKind::Screen`
/// — отдельный класс. Используется в подписках и в MediaEngine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Аудио.
    Audio,
    /// Видео.
    Video,
    /// Экран.
    Screen,
}

impl MediaKind {
    /// Название класса (для логов и wire-форматов).
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
            MediaKind::Screen => "screen",
        }
    }
}

impl std::fmt::Display for MediaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<TrackKind> for MediaKind {
    /// Преобразование конкретного типа в типовой класс.
    fn from(kind: TrackKind) -> Self {
        match kind {
            TrackKind::Video => MediaKind::Video,
            TrackKind::Screen => MediaKind::Screen,
            TrackKind::Audio => MediaKind::Audio,
        }
    }
}

/// Дескриптор медиатрека.
///
/// Единый в обоих режимах (MCU/SFU) — часть прозрачности (ARCHITECTURE §2).
/// В SFU-режиме клиент получает по одному такому треку на участника; в
/// MCU-режиме сервер выдаёт один композитный трек.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    /// Идентикатор трека.
    pub id: TrackId,
    /// Идентикатор участника-источника.
    pub participant_id: crate::participant::ParticipantId,
    /// Конкретный тип источника.
    pub kind: TrackKind,
    /// Типовой класс медиат.
    pub media_kind: MediaKind,
}

impl Track {
    /// Создаёт новый дескриптор трека с указанным типом.
    pub fn new(participant_id: crate::participant::ParticipantId, kind: TrackKind) -> Self {
        Self { id: TrackId::new(), participant_id, media_kind: MediaKind::from(kind), kind }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_id_roundtrip() {
        let id = TrackId::new();
        let s = id.to_string();
        let parsed: TrackId = s.parse().expect("parse");
        assert_eq!(id, parsed);
    }

    #[test]
    fn track_kind_to_media_kind() {
        assert_eq!(MediaKind::from(TrackKind::Video), MediaKind::Video);
        assert_eq!(MediaKind::from(TrackKind::Screen), MediaKind::Screen);
        assert_eq!(MediaKind::from(TrackKind::Audio), MediaKind::Audio);
    }

    #[test]
    fn track_kind_display() {
        assert_eq!(TrackKind::Video.to_string(), "video");
        assert_eq!(TrackKind::Screen.to_string(), "screen");
        assert_eq!(TrackKind::Audio.to_string(), "audio");
    }

    #[test]
    fn media_kind_display() {
        assert_eq!(MediaKind::Audio.to_string(), "audio");
        assert_eq!(MediaKind::Video.to_string(), "video");
        assert_eq!(MediaKind::Screen.to_string(), "screen");
    }

    #[test]
    fn track_new_sets_kind_and_media_kind() {
        let pid = crate::participant::ParticipantId::new();
        let t = Track::new(pid, TrackKind::Screen);
        assert_eq!(t.kind, TrackKind::Screen);
        assert_eq!(t.media_kind, MediaKind::Screen);
        assert_eq!(t.participant_id, pid);
    }

    #[test]
    fn serde_roundtrip_track() {
        let pid = crate::participant::ParticipantId::new();
        let t = Track::new(pid, TrackKind::Audio);
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Track = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }
}
